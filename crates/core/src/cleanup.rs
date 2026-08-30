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
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CleanupProgressPhase {
    Checking,
    Removing,
}
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CleanupProgressUpdate {
    pub phase: CleanupProgressPhase,
    pub processed: usize,
    pub total: usize,
    pub path: PathBuf,
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
        Self::execute_with_progress(preflight, |_, _, _| {})
    }
    pub fn execute_with_progress(
        preflight: CleanupPreflight,
        mut progress: impl FnMut(usize, usize, &std::path::Path),
    ) -> Result<CleanupOutcome, CleanupError> {
        Self::execute_with_updates(preflight, |update| {
            progress(update.processed, update.total, &update.path);
        })
    }
    pub fn execute_with_updates(
        preflight: CleanupPreflight,
        mut progress: impl FnMut(CleanupProgressUpdate),
    ) -> Result<CleanupOutcome, CleanupError> {
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
        let total = preflight.plan.files.len();
        let action = preflight.plan.action;
        let mut ready = Vec::with_capacity(total);
        for (index, file) in preflight.plan.files.into_iter().enumerate() {
            let path = file.path.clone();
            // The preflight check is only a preview.  A file can be replaced,
            // modified, or removed while the confirmation dialog is open (or
            // between two deletes), so check the exact snapshot again at the
            // last safe point before handing it to the operating system.
            if let Err(message) = validate_snapshot(&file) {
                out.failures.push(CleanupFailure {
                    path: file.path,
                    message,
                });
                progress(CleanupProgressUpdate {
                    phase: CleanupProgressPhase::Checking,
                    processed: index + 1,
                    total,
                    path,
                });
                continue;
            }
            if action == CleanupAction::Trash {
                ready.push(file);
                progress(CleanupProgressUpdate {
                    phase: CleanupProgressPhase::Checking,
                    processed: index + 1,
                    total,
                    path,
                });
                continue;
            }
            match remove(&file.path, action) {
                Ok(()) => {
                    out.recovered_bytes += file.size;
                    out.removed.push(file.path)
                }
                Err(error) => out.failures.push(CleanupFailure {
                    path: file.path,
                    message: error.to_string(),
                }),
            }
            progress(CleanupProgressUpdate {
                phase: CleanupProgressPhase::Removing,
                processed: index + 1,
                total,
                path,
            });
        }
        if action == CleanupAction::Trash && !ready.is_empty() {
            match trash::delete_all(ready.iter().map(|file| &file.path)) {
                Ok(()) => {
                    for file in ready {
                        let path = file.path.clone();
                        out.recovered_bytes += file.size;
                        out.removed.push(file.path);
                        progress(CleanupProgressUpdate {
                            phase: CleanupProgressPhase::Removing,
                            processed: out.removed.len() + out.failures.len(),
                            total,
                            path,
                        });
                    }
                }
                Err(error) => {
                    let message = error.to_string();
                    for file in ready {
                        out.failures.push(CleanupFailure {
                            path: file.path,
                            message: message.clone(),
                        });
                    }
                }
            }
        }
        Ok(out)
    }
}

fn validate_snapshot(file: &FileSnapshot) -> Result<(), String> {
    match fs::metadata(&file.path) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            Err("file no longer exists".to_owned())
        }
        Err(error) => Err(format!("could not inspect file before cleanup: {error}")),
        Ok(metadata)
            if metadata.len() != file.size || metadata.modified().ok() != file.modified =>
        {
            Err("file changed since it was scanned".to_owned())
        }
        Ok(_) => Ok(()),
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

    #[test]
    fn execute_skips_a_file_changed_after_preflight() {
        let d = tempdir().unwrap();
        let p = d.path().join("a");
        fs::write(&p, b"same").unwrap();
        let plan =
            CleanupService::plan(&result(p.clone()), CleanupAction::PermanentDelete).unwrap();
        let preflight = CleanupService::preflight(plan);

        fs::write(&p, b"replacement with a different size").unwrap();
        let out = CleanupService::execute(preflight).unwrap();

        assert!(p.exists());
        assert_eq!(out.removed, Vec::<PathBuf>::new());
        assert_eq!(out.failures.len(), 1);
        assert_eq!(out.failures[0].path, p);
        assert!(out.failures[0].message.contains("changed"));
    }

    #[test]
    fn execute_reports_partial_outcomes_and_keeps_unsafe_targets() {
        let d = tempdir().unwrap();
        let removed = d.path().join("removed");
        let changed = d.path().join("changed");
        fs::write(&removed, b"same").unwrap();
        fs::write(&changed, b"same").unwrap();
        let plan = CleanupPlan {
            action: CleanupAction::PermanentDelete,
            files: vec![
                FileSnapshot {
                    path: removed.clone(),
                    size: 4,
                    modified: fs::metadata(&removed).unwrap().modified().ok(),
                },
                FileSnapshot {
                    path: changed.clone(),
                    size: 4,
                    modified: fs::metadata(&changed).unwrap().modified().ok(),
                },
            ],
            bytes: 8,
        };
        let preflight = CleanupService::preflight(plan);
        fs::write(&changed, b"changed after preflight").unwrap();

        let mut progress = Vec::new();
        let out = CleanupService::execute_with_progress(preflight, |processed, total, path| {
            progress.push((processed, total, path.to_path_buf()));
        })
        .unwrap();
        assert_eq!(out.removed, vec![removed.clone()]);
        assert_eq!(out.recovered_bytes, 4);
        assert_eq!(out.failures.len(), 1);
        assert_eq!(out.failures[0].path, changed);
        assert!(!removed.exists());
        assert!(changed.exists());
        assert_eq!(progress, vec![(1, 2, removed), (2, 2, changed)]);
    }
}
