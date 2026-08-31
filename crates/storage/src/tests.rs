use super::*;
use rusqlite::Connection;
use tempfile::NamedTempFile;

fn db() -> (NamedTempFile, Database) {
    let file = NamedTempFile::new().unwrap();
    let db = Database::open(file.path()).unwrap();
    (file, db)
}
fn at(ms: u64) -> SystemTime {
    UNIX_EPOCH + std::time::Duration::from_millis(ms)
}
fn scan(path: PathBuf) -> NewScan {
    NewScan {
        name: Some("Pictures scan".into()),
        started_at: at(1_000),
        paths: vec![ScanPath {
            path,
            preferred: true,
        }],
        settings: ScanSettings::default(),
    }
}
fn results() -> Vec<DuplicateGroup> {
    let mut group = DuplicateGroup::new(
        GroupId(1),
        20,
        vec![
            DuplicateFile {
                id: DuplicateFileId(1),
                path: PathBuf::from("/photos/été a.jpg"),
                size: 20,
                modified: Some(at(2_000)),
            },
            DuplicateFile {
                id: DuplicateFileId(2),
                path: PathBuf::from("/backup/été a.jpg"),
                size: 20,
                modified: None,
            },
        ],
    )
    .unwrap();
    group.set_selected(DuplicateFileId(2), true).unwrap();
    vec![group]
}

#[test]
fn persists_complete_scan_groups_selection_and_history() {
    let (_file, mut db) = db();
    let id = db
        .create_scan(&scan(PathBuf::from("/with spaces/日本語")))
        .unwrap();
    db.replace_results(
        id,
        &results(),
        &ScanSummary {
            duplicate_groups: 1,
            recoverable_bytes: 20,
            duplicate_files: 2,
        },
        at(3_000),
    )
    .unwrap();
    db.record_cleanup(
        id,
        &NewCleanupAction {
            created_at: at(4_000),
            action: "trash".into(),
            affected_files: 1,
            recovered_bytes: 20,
        },
    )
    .unwrap();

    let stored = db.scan(id).unwrap();
    assert_eq!(stored.status, ScanStatus::Completed);
    assert_eq!(stored.paths[0].path, PathBuf::from("/with spaces/日本語"));
    assert_eq!(
        stored.summary,
        Some(ScanSummary {
            duplicate_groups: 1,
            recoverable_bytes: 20,
            duplicate_files: 2
        })
    );
    let group = db.groups(id).unwrap().pop().unwrap();
    assert_eq!(group.files[0].path, PathBuf::from("/photos/été a.jpg"));
    assert!(group.is_selected(group.files[1].id));
    assert_eq!(db.cleanup_history(id).unwrap()[0].action, "trash");
}

#[test]
fn selection_cannot_remove_all_copies() {
    let (_file, mut db) = db();
    let id = db.create_scan(&scan(PathBuf::from("/a"))).unwrap();
    db.replace_results(
        id,
        &results(),
        &ScanSummary {
            duplicate_groups: 1,
            recoverable_bytes: 20,
            duplicate_files: 2,
        },
        at(3),
    )
    .unwrap();
    let first = db.groups(id).unwrap()[0].files[0].id;
    assert!(matches!(
        db.set_selected(first, true),
        Err(StorageError::LastCopySelected { .. })
    ));
    db.set_selected(first, false).unwrap();
}

#[test]
fn replacement_returns_database_owned_ids_with_selection_intact() {
    let (_file, mut db) = db();
    let id = db.create_scan(&scan(PathBuf::from("/a"))).unwrap();
    let precise_modified = at(2_000) + std::time::Duration::from_nanos(123_456_789);
    db.replace_results(
        id,
        &results(),
        &ScanSummary {
            duplicate_groups: 1,
            duplicate_files: 2,
            recoverable_bytes: 20,
        },
        at(2),
    )
    .unwrap();

    let mut replacement = DuplicateGroup::new(
        GroupId(900),
        20,
        vec![
            DuplicateFile {
                id: DuplicateFileId(901),
                path: PathBuf::from("/photos/été a.jpg"),
                size: 20,
                modified: Some(precise_modified),
            },
            DuplicateFile {
                id: DuplicateFileId(902),
                path: PathBuf::from("/backup/été a.jpg"),
                size: 20,
                modified: None,
            },
        ],
    )
    .unwrap();
    replacement
        .set_selected(DuplicateFileId(902), true)
        .unwrap();
    let replacement = vec![replacement];
    let refreshed_settings = ScanSettings {
        min_size: Some(42),
        max_size: Some(4_200),
        cache: false,
    };
    let loaded = db
        .replace_results_and_load_with_settings(
            id,
            &replacement,
            &ScanSummary {
                duplicate_groups: 1,
                duplicate_files: 2,
                recoverable_bytes: 20,
            },
            at(3),
            Some(refreshed_settings),
        )
        .unwrap();

    assert_ne!(loaded[0].id, replacement[0].id);
    assert_ne!(loaded[0].files[0].id, replacement[0].files[0].id);
    assert!(loaded[0].is_selected(loaded[0].files[1].id));
    assert_eq!(loaded[0].files[0].modified, Some(precise_modified));
    assert_eq!(loaded, db.groups(id).unwrap());
    assert_eq!(db.scan(id).unwrap().settings, refreshed_settings);
    assert!(db.scan(id).unwrap().settings_recorded);
}

#[test]
fn legacy_scans_are_marked_when_settings_were_not_recorded() {
    let file = NamedTempFile::new().unwrap();
    {
        let connection = Connection::open(file.path()).unwrap();
        connection
                .execute_batch(
                    "CREATE TABLE schema_migrations (version INTEGER PRIMARY KEY);
                     INSERT INTO schema_migrations(version) VALUES(4);
                     CREATE TABLE scans (id INTEGER PRIMARY KEY, name TEXT, started_at INTEGER NOT NULL, finished_at INTEGER, status TEXT NOT NULL, duplicate_bytes INTEGER, duplicate_files INTEGER, duplicate_groups INTEGER);
                     INSERT INTO scans(id,name,started_at,status) VALUES(1,'Legacy',0,'completed');
                     CREATE TABLE duplicate_files (id INTEGER PRIMARY KEY, group_id INTEGER NOT NULL, path BLOB NOT NULL, size INTEGER NOT NULL, modified_at INTEGER, selected INTEGER NOT NULL DEFAULT 0, modified_at_nanos INTEGER);",
                )
                .unwrap();
    }

    let db = Database::open(file.path()).unwrap();
    let scan = db.scan(1).unwrap();
    assert_eq!(scan.settings, ScanSettings::default());
    assert!(!scan.settings_recorded);
}

#[test]
fn deletion_cascades_and_old_scans_can_be_reopened() {
    let (_file, mut db) = db();
    let old = db.create_scan(&scan(PathBuf::from("/old"))).unwrap();
    db.finish_scan(old, ScanStatus::Cancelled, at(2)).unwrap();
    let new = db.create_scan(&scan(PathBuf::from("/new"))).unwrap();
    assert_eq!(
        db.scans().unwrap().iter().map(|s| s.id).collect::<Vec<_>>(),
        vec![new, old]
    );
    assert_eq!(db.scan(old).unwrap().status, ScanStatus::Cancelled);
    db.delete_scan(old).unwrap();
    assert!(matches!(db.scan(old), Err(StorageError::ScanNotFound(_))));
}

#[test]
fn detailed_cleanup_history_preserves_partial_failures() {
    let (_file, mut db) = db();
    let id = db.create_scan(&scan(PathBuf::from("/a"))).unwrap();
    let outcome = CleanupOutcome {
        action: dupekit_core::CleanupAction::PermanentDelete,
        removed: vec![PathBuf::from("/a/removed")],
        recovered_bytes: 20,
        failures: vec![dupekit_core::CleanupFailure {
            path: PathBuf::from("/a/changed"),
            message: "file changed since it was scanned".into(),
        }],
    };
    db.record_cleanup_outcome(id, "permanent delete", at(2), &outcome)
        .unwrap();

    let history = db.cleanup_history_details(id).unwrap();
    assert_eq!(history.len(), 1);
    assert_eq!(history[0].attempted_files, 2);
    assert_eq!(history[0].action.affected_files, 1);
    assert_eq!(history[0].failures[0].path, PathBuf::from("/a/changed"));
}

#[test]
fn reconciliation_removes_cleaned_files_and_empty_duplicate_groups() {
    let (_file, mut db) = db();
    let id = db.create_scan(&scan(PathBuf::from("/a"))).unwrap();
    db.replace_results(
        id,
        &results(),
        &ScanSummary {
            duplicate_groups: 1,
            duplicate_files: 2,
            recoverable_bytes: 20,
        },
        at(2),
    )
    .unwrap();
    db.reconcile_removed_files(id, &[PathBuf::from("/backup/été a.jpg")])
        .unwrap();

    assert!(db.groups(id).unwrap().is_empty());
    assert_eq!(
        db.scan(id).unwrap().summary,
        Some(ScanSummary {
            duplicate_groups: 0,
            duplicate_files: 0,
            recoverable_bytes: 0,
        })
    );
}

#[cfg(unix)]
#[test]
fn preserves_non_utf8_unix_paths() {
    use std::{ffi::OsStr, os::unix::ffi::OsStrExt};
    let path = PathBuf::from(OsStr::from_bytes(b"/tmp/a bad \xff name"));
    let (_file, mut db) = db();
    let id = db.create_scan(&scan(path.clone())).unwrap();
    let mut group = DuplicateGroup::new(
        GroupId(1),
        1,
        vec![
            DuplicateFile {
                id: DuplicateFileId(1),
                path: path.clone(),
                size: 1,
                modified: None,
            },
            DuplicateFile {
                id: DuplicateFileId(2),
                path: PathBuf::from("/tmp/other"),
                size: 1,
                modified: None,
            },
        ],
    )
    .unwrap();
    group.set_selected(DuplicateFileId(2), true).unwrap();
    db.replace_results(
        id,
        &[group],
        &ScanSummary {
            duplicate_groups: 1,
            recoverable_bytes: 1,
            duplicate_files: 2,
        },
        at(2),
    )
    .unwrap();
    assert_eq!(
        db.scan(id).unwrap().paths[0].path.as_os_str().as_bytes(),
        path.as_os_str().as_bytes()
    );
    assert_eq!(
        db.groups(id).unwrap()[0].files[0]
            .path
            .as_os_str()
            .as_bytes(),
        path.as_os_str().as_bytes()
    );
}
