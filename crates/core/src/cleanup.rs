use crate::{DuplicateFile, ScanResult, SelectionError};
use std::{fs, path::PathBuf, time::SystemTime};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CleanupAction {
    Trash,
    PermanentDelete,
}
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FileSnapshot {
    pub path: PathBuf,
    pub size: u64,
    pub modified: Option<SystemTime>,
}
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CleanupPlan {
    pub action: CleanupAction,
    pub files: Vec<FileSnapshot>,
    pub bytes: u64,
}
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CleanupPreflight {
    pub plan: CleanupPlan,
    pub missing: Vec<PathBuf>,
    pub changed: Vec<PathBuf>,
}
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CleanupFailure {
    pub path: PathBuf,
    pub message: String,
}
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CleanupOutcome {
    pub action: CleanupAction,
    pub removed: Vec<PathBuf>,
    pub recovered_bytes: u64,
    pub failures: Vec<CleanupFailure>,
}
#[derive(Debug, thiserror::Error)]
pub enum CleanupError {
    #[error("invalid duplicate selection: {0}")]
    InvalidSelection(#[from] SelectionError),
    #[error("cleanup preflight rejected {missing} missing and {changed} changed files")]
    UnsafePreflight { missing: usize, changed: usize },
}

pub struct CleanupService;
impl CleanupService {
    pub fn plan(result: &ScanResult, action: CleanupAction) -> Result<CleanupPlan, CleanupError> {
        result.validate_selection()?;
        let files: Vec<_> = result
            .groups
            .iter()
            .flat_map(|g| g.selected_files())
            .map(snapshot)
            .collect();
        let bytes = files.iter().map(|f| f.size).sum();
        Ok(CleanupPlan {
            action,
            files,
            bytes,
        })
    }
    pub fn preflight(plan: CleanupPlan) -> CleanupPreflight {
        let mut missing = vec![];
        let mut changed = vec![];
        for f in &plan.files {
            match fs::metadata(&f.path) {
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => missing.push(f.path.clone()),
                Err(_) => changed.push(f.path.clone()),
                Ok(meta) if meta.len() != f.size || meta.modified().ok() != f.modified => {
                    changed.push(f.path.clone())
                }
                Ok(_) => {}
            }
        }
        CleanupPreflight {
            plan,
            missing,
            changed,
        }
    }
    pub fn execute(preflight: CleanupPreflight) -> Result<CleanupOutcome, CleanupError> {
        if !preflight.missing.is_empty() || !preflight.changed.is_empty() {
            return Err(CleanupError::UnsafePreflight {
                missing: preflight.missing.len(),
                changed: preflight.changed.len(),
            });
        }
        let mut out = CleanupOutcome {
            action: preflight.plan.action,
            removed: vec![],
            recovered_bytes: 0,
            failures: vec![],
        };
        for file in preflight.plan.files {
            match remove(&file.path, preflight.plan.action) {
                Ok(()) => {
                    out.recovered_bytes += file.size;
                    out.removed.push(file.path)
                }
                Err(error) => out.failures.push(CleanupFailure {
                    path: file.path,
                    message: error.to_string(),
                }),
            }
        }
        Ok(out)
    }
}
fn snapshot(file: &DuplicateFile) -> FileSnapshot {
    FileSnapshot {
        path: file.path.clone(),
        size: file.size,
        modified: file.modified,
    }
}
fn remove(
    path: &std::path::Path,
    action: CleanupAction,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    match action {
        CleanupAction::Trash => trash::delete(path).map_err(Into::into),
        CleanupAction::PermanentDelete => fs::remove_file(path).map_err(Into::into),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{DuplicateFileId, GroupId, *};
    use tempfile::tempdir;
    fn result(path: PathBuf) -> ScanResult {
        let m = fs::metadata(&path).unwrap();
        let mut g = DuplicateGroup::new(
            GroupId(1),
            m.len(),
            vec![
                DuplicateFile {
                    id: DuplicateFileId(1),
                    path: path.clone(),
                    size: m.len(),
                    modified: m.modified().ok(),
                },
                DuplicateFile {
                    id: DuplicateFileId(2),
                    path: path.with_extension("keep"),
                    size: m.len(),
                    modified: None,
                },
            ],
        )
        .unwrap();
        g.set_selected(DuplicateFileId(1), true).unwrap();
        ScanResult::from_groups(vec![g])
    }
    #[test]
    fn preflight_rejects_missing_file_without_deleting_anything() {
        let d = tempdir().unwrap();
        let p = d.path().join("a");
        fs::write(&p, b"same").unwrap();
        let plan =
            CleanupService::plan(&result(p.clone()), CleanupAction::PermanentDelete).unwrap();
        fs::remove_file(&p).unwrap();
        let check = CleanupService::preflight(plan);
        assert_eq!(check.missing, vec![p]);
        assert!(matches!(
            CleanupService::execute(check),
            Err(CleanupError::UnsafePreflight { .. })
        ));
    }
    #[test]
    fn preflight_rejects_changed_file() {
        let d = tempdir().unwrap();
        let p = d.path().join("a");
        fs::write(&p, b"same").unwrap();
        let plan =
            CleanupService::plan(&result(p.clone()), CleanupAction::PermanentDelete).unwrap();
        fs::write(&p, b"changed content").unwrap();
        let check = CleanupService::preflight(plan);
        assert_eq!(check.changed, vec![p]);
    }
    #[test]
    fn permanent_delete_reports_recovered_bytes() {
        let d = tempdir().unwrap();
        let p = d.path().join("a");
        fs::write(&p, b"same").unwrap();
        let plan =
            CleanupService::plan(&result(p.clone()), CleanupAction::PermanentDelete).unwrap();
        let out = CleanupService::execute(CleanupService::preflight(plan)).unwrap();
        assert!(!p.exists());
        assert_eq!(out.recovered_bytes, 4);
    }
}
