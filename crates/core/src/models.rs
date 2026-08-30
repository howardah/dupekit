use std::{collections::BTreeSet, path::PathBuf, time::SystemTime};

#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd, Hash)]
pub struct GroupId(pub u64);

#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd, Hash)]
pub struct DuplicateFileId(pub u64);

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ScanPath {
    pub path: PathBuf,
    pub preferred: bool,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ScanConfig {
    pub paths: Vec<ScanPath>,
    pub min_size: Option<u64>,
    pub max_size: Option<u64>,
    pub cache: bool,
}

impl Default for ScanConfig {
    fn default() -> Self {
        Self {
            paths: vec![],
            min_size: Some(1),
            max_size: None,
            cache: true,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DuplicateFile {
    pub id: DuplicateFileId,
    pub path: PathBuf,
    pub size: u64,
    pub modified: Option<SystemTime>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DuplicateGroup {
    pub id: GroupId,
    pub file_size: u64,
    pub files: Vec<DuplicateFile>,
    selected: BTreeSet<DuplicateFileId>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SelectionPolicy {
    KeepFirst,
    KeepNewest,
    KeepOldest,
    PreferPreferredDirectories,
    ClearSelection,
}

#[derive(Debug, thiserror::Error, Eq, PartialEq)]
pub enum SelectionError {
    #[error("file {0:?} is not in this duplicate group")]
    UnknownFile(DuplicateFileId),
    #[error("a duplicate group must retain at least one file")]
    WouldSelectEveryCopy,
    #[error("a duplicate group needs at least two files")]
    NotDuplicate,
}

impl DuplicateGroup {
    pub fn new(
        id: GroupId,
        file_size: u64,
        files: Vec<DuplicateFile>,
    ) -> Result<Self, SelectionError> {
        if files.len() < 2 {
            return Err(SelectionError::NotDuplicate);
        }
        Ok(Self {
            id,
            file_size,
            files,
            selected: BTreeSet::new(),
        })
    }
    pub fn selected_ids(&self) -> &BTreeSet<DuplicateFileId> {
        &self.selected
    }
    pub fn selected_files(&self) -> impl Iterator<Item = &DuplicateFile> {
        self.files.iter().filter(|f| self.selected.contains(&f.id))
    }
    pub fn selected_bytes(&self) -> u64 {
        self.selected.len() as u64 * self.file_size
    }
    pub fn is_selected(&self, id: DuplicateFileId) -> bool {
        self.selected.contains(&id)
    }
    pub fn set_selected(
        &mut self,
        id: DuplicateFileId,
        selected: bool,
    ) -> Result<(), SelectionError> {
        if !self.files.iter().any(|file| file.id == id) {
            return Err(SelectionError::UnknownFile(id));
        }
        if selected && self.selected.len() + 1 == self.files.len() && !self.selected.contains(&id) {
            return Err(SelectionError::WouldSelectEveryCopy);
        }
        if selected {
            self.selected.insert(id);
        } else {
            self.selected.remove(&id);
        }
        Ok(())
    }
    pub fn apply_selection(&mut self, policy: SelectionPolicy, preferred: &[PathBuf]) {
        self.selected.clear();
        if policy == SelectionPolicy::ClearSelection {
            return;
        }
        let keep_index = match policy {
            SelectionPolicy::KeepFirst => 0,
            SelectionPolicy::KeepNewest => self
                .files
                .iter()
                .enumerate()
                .max_by_key(|(_, f)| f.modified)
                .map(|(i, _)| i)
                .unwrap_or(0),
            SelectionPolicy::KeepOldest => self
                .files
                .iter()
                .enumerate()
                .min_by_key(|(_, f)| f.modified)
                .map(|(i, _)| i)
                .unwrap_or(0),
            SelectionPolicy::PreferPreferredDirectories => self
                .files
                .iter()
                .position(|f| preferred.iter().any(|dir| f.path.starts_with(dir)))
                .unwrap_or(0),
            SelectionPolicy::ClearSelection => unreachable!(),
        };
        self.selected.extend(
            self.files
                .iter()
                .enumerate()
                .filter_map(|(i, f)| (i != keep_index).then_some(f.id)),
        );
    }
    pub fn validate_selection(&self) -> Result<(), SelectionError> {
        if self.selected.len() >= self.files.len() {
            Err(SelectionError::WouldSelectEveryCopy)
        } else {
            Ok(())
        }
    }
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct ScanSummary {
    pub duplicate_groups: u64,
    pub duplicate_files: u64,
    pub recoverable_bytes: u64,
}
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct ScanResult {
    pub groups: Vec<DuplicateGroup>,
    pub summary: ScanSummary,
}

impl ScanResult {
    pub fn from_groups(groups: Vec<DuplicateGroup>) -> Self {
        let summary = ScanSummary {
            duplicate_groups: groups.len() as u64,
            duplicate_files: groups.iter().map(|g| g.files.len() as u64).sum(),
            recoverable_bytes: groups.iter().map(DuplicateGroup::selected_bytes).sum(),
        };
        Self { groups, summary }
    }
    pub fn validate_selection(&self) -> Result<(), SelectionError> {
        self.groups
            .iter()
            .try_for_each(DuplicateGroup::validate_selection)
    }
    /// Totals derived from the current selection. Use this after a user changes
    /// checkboxes; `summary` describes the scan's complete result set.
    pub fn selected_file_count(&self) -> u64 {
        self.groups
            .iter()
            .map(|g| g.selected_ids().len() as u64)
            .sum()
    }
    pub fn selected_bytes(&self) -> u64 {
        self.groups.iter().map(DuplicateGroup::selected_bytes).sum()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;
    fn group() -> DuplicateGroup {
        DuplicateGroup::new(
            GroupId(1),
            10,
            vec![
                DuplicateFile {
                    id: DuplicateFileId(1),
                    path: "/normal/a".into(),
                    size: 10,
                    modified: Some(SystemTime::UNIX_EPOCH + Duration::from_secs(5)),
                },
                DuplicateFile {
                    id: DuplicateFileId(2),
                    path: "/preferred/b".into(),
                    size: 10,
                    modified: Some(SystemTime::UNIX_EPOCH + Duration::from_secs(10)),
                },
                DuplicateFile {
                    id: DuplicateFileId(3),
                    path: "/normal/c".into(),
                    size: 10,
                    modified: None,
                },
            ],
        )
        .unwrap()
    }
    #[test]
    fn cannot_select_all_copies() {
        let mut g = group();
        g.set_selected(DuplicateFileId(1), true).unwrap();
        g.set_selected(DuplicateFileId(2), true).unwrap();
        assert_eq!(
            g.set_selected(DuplicateFileId(3), true),
            Err(SelectionError::WouldSelectEveryCopy)
        );
    }
    #[test]
    fn preferred_policy_keeps_preferred_copy() {
        let mut g = group();
        g.apply_selection(
            SelectionPolicy::PreferPreferredDirectories,
            &["/preferred".into()],
        );
        assert!(!g.is_selected(DuplicateFileId(2)));
        assert_eq!(g.selected_ids().len(), 2);
    }
    #[test]
    fn newest_and_oldest_are_deterministic_with_missing_times() {
        let mut g = group();
        g.apply_selection(SelectionPolicy::KeepNewest, &[]);
        assert!(!g.is_selected(DuplicateFileId(2)));
        g.apply_selection(SelectionPolicy::KeepOldest, &[]);
        assert!(!g.is_selected(DuplicateFileId(3)));
    }
}
