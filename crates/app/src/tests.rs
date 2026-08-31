use super::*;
use crate::utilities::*;
use dupekit_core::{DuplicateFile, GroupId};

fn duplicate_group(id: u64, paths: &[&str], selected: &[usize]) -> DuplicateGroup {
    let files = paths
        .iter()
        .enumerate()
        .map(|(index, path)| DuplicateFile {
            id: DuplicateFileId(id * 100 + index as u64),
            path: PathBuf::from(path),
            size: 42,
            modified: None,
        })
        .collect();
    let mut group = DuplicateGroup::new(GroupId(id), 42, files).unwrap();
    for &index in selected {
        group.set_selected(group.files[index].id, true).unwrap();
    }
    group
}

fn completed_scan(db: &mut Database) -> ScanId {
    let id = db
        .create_scan(&NewScan {
            name: Some("Saved scan".into()),
            started_at: std::time::SystemTime::now(),
            paths: vec![],
            settings: ScanSettings::default(),
        })
        .unwrap();
    db.finish_scan(id, ScanStatus::Completed, std::time::SystemTime::now())
        .unwrap();
    id
}
#[test]
fn parses_human_sizes() {
    assert_eq!(parse_size_input("1 MB", "Minimum"), Ok(Some(1_048_576)));
    assert_eq!(
        parse_size_input("2.5 gb", "Minimum"),
        Ok(Some(2_684_354_560))
    );
    assert_eq!(parse_size_input("", "Minimum"), Ok(None));
    for bytes in [1, 1_024, 1_310_720, 1_048_576, 2_684_354_560] {
        assert_eq!(
            parse_size_input(&size_input_value(bytes), "Minimum"),
            Ok(Some(bytes))
        );
    }
}
#[test]
fn rejects_malformed_or_out_of_range_size_inputs() {
    assert!(parse_size_input("1 MB extra", "Minimum").is_err());
    assert!(parse_size_input("NaN MB", "Minimum").is_err());
    assert!(parse_size_input("-1 MB", "Minimum").is_err());
    assert!(parse_size_input("1 zebibyte", "Minimum").is_err());
    assert!(parse_size_input("999999999999999999999 TB", "Minimum").is_err());
    let min = parse_size_input("2 MB", "Minimum").unwrap();
    let max = parse_size_input("1 MB", "Maximum").unwrap();
    assert!(min.zip(max).is_some_and(|(min, max)| min > max));
}
#[test]
fn progress_reducer_only_uses_scanner_totals() {
    let mut progress = ScanProgress::default();
    progress.apply(&ScanEvent::PhaseStarted {
        name: "Full hashing".into(),
        total: Some(100),
    });
    progress.apply(&ScanEvent::Progress {
        processed: 25,
        total: Some(100),
    });
    assert_eq!(progress.phase, "Full hashing");
    assert_eq!(progress.fraction(), Some(0.25));
    progress.apply(&ScanEvent::PhaseStarted {
        name: "Finalizing".into(),
        total: None,
    });
    assert_eq!(progress.fraction(), None);
}
#[test]
fn stale_run_messages_are_rejected() {
    let mut app = App {
        screen: Screen::Scanning(ScanProgress::default()),
        paths: vec![],
        min_size: String::new(),
        max_size: String::new(),
        cache: false,
        history: vec![],
        scan_cancel: None,
        scan_events: None,
        next_scan_run: 2,
        active_scan_run: Some(2),
        running_scan_id: None,
        scan_mode: None,
        next_cleanup_run: 0,
        active_cleanup_run: None,
        cleanup_events: None,
        latest_review: None,
        db: Database::open_in_memory().unwrap(),
        active_scan_id: None,
        notice: None,
    };
    let task = update(
        &mut app,
        Message::ScanCompleted {
            run: 1,
            result: Err("old scan".into()),
        },
    );
    drop(task);
    assert_eq!(app.active_scan_run, Some(2));
    assert!(matches!(app.screen, Screen::Scanning(_)));
}

#[test]
fn cancelling_scan_blocks_a_new_scan_until_its_worker_returns() {
    let mut db = Database::open_in_memory().unwrap();
    let scan_id = db
        .create_scan(&NewScan {
            name: Some("Scan".into()),
            started_at: std::time::SystemTime::now(),
            paths: vec![],
            settings: ScanSettings::default(),
        })
        .unwrap();
    let cancellation = CancellationToken::default();
    let mut app = App {
        screen: Screen::Scanning(ScanProgress::default()),
        paths: vec![],
        min_size: String::new(),
        max_size: String::new(),
        cache: false,
        history: vec![],
        scan_cancel: Some(cancellation.clone()),
        scan_events: None,
        next_scan_run: 5,
        active_scan_run: Some(5),
        running_scan_id: Some(scan_id),
        scan_mode: Some(ScanMode::Initial {
            settings: RefreshSettings::from_scan(ScanSettings::default(), true),
        }),
        next_cleanup_run: 0,
        active_cleanup_run: None,
        cleanup_events: None,
        latest_review: None,
        db,
        active_scan_id: Some(scan_id),
        notice: None,
    };

    drop(update(&mut app, Message::CancelScan));
    assert!(cancellation.is_cancelled());
    assert!(matches!(app.screen, Screen::Cancelling));
    assert_eq!(app.active_scan_run, Some(5));
    assert!(app.scan_cancel.is_some());
    assert_eq!(app.running_scan_id, None);
    assert_eq!(app.db.scan(scan_id).unwrap().status, ScanStatus::Cancelled);

    // This is the reducer equivalent of immediately clicking "Find
    // duplicates" again. It must not launch a worker using the locked
    // fclones cache database.
    drop(update(&mut app, Message::StartScan));
    assert_eq!(app.active_scan_run, Some(5));
    assert!(matches!(app.screen, Screen::Cancelling));

    // The old worker has now returned, so its fclones resources (and the
    // cache lock) have been dropped. Its error remains cancellation, not
    // a failed history record.
    drop(update(
        &mut app,
        Message::ScanCompleted {
            run: 5,
            result: Err("scan cancelled".into()),
        },
    ));
    assert!(matches!(app.screen, Screen::Home));
    assert_eq!(app.active_scan_run, None);
    assert!(app.scan_cancel.is_none());
    assert_eq!(app.db.scan(scan_id).unwrap().status, ScanStatus::Cancelled);
}

#[test]
fn cancelled_worker_success_is_discarded_after_releasing_the_lock() {
    let mut db = Database::open_in_memory().unwrap();
    let scan_id = db
        .create_scan(&NewScan {
            name: Some("Scan".into()),
            started_at: std::time::SystemTime::now(),
            paths: vec![],
            settings: ScanSettings::default(),
        })
        .unwrap();
    db.finish_scan(scan_id, ScanStatus::Cancelled, std::time::SystemTime::now())
        .unwrap();
    let cancellation = CancellationToken::default();
    cancellation.cancel();
    let mut app = App {
        screen: Screen::Cancelling,
        paths: vec![],
        min_size: String::new(),
        max_size: String::new(),
        cache: false,
        history: vec![],
        scan_cancel: Some(cancellation),
        scan_events: None,
        next_scan_run: 6,
        active_scan_run: Some(6),
        running_scan_id: None,
        scan_mode: Some(ScanMode::Initial {
            settings: RefreshSettings::from_scan(ScanSettings::default(), true),
        }),
        next_cleanup_run: 0,
        active_cleanup_run: None,
        cleanup_events: None,
        latest_review: None,
        db,
        active_scan_id: None,
        notice: None,
    };
    drop(update(
        &mut app,
        Message::ScanCompleted {
            run: 6,
            result: Ok(ScanResult::from_groups(vec![])),
        },
    ));
    assert!(matches!(app.screen, Screen::Home));
    assert!(app.latest_review.is_none());
    assert_eq!(app.db.scan(scan_id).unwrap().status, ScanStatus::Cancelled);
}

#[test]
fn stale_cleanup_completion_cannot_change_the_current_screen() {
    let mut app = App {
        screen: Screen::Home,
        paths: vec![],
        min_size: String::new(),
        max_size: String::new(),
        cache: false,
        history: vec![],
        scan_cancel: None,
        scan_events: None,
        next_scan_run: 0,
        active_scan_run: None,
        running_scan_id: None,
        scan_mode: None,
        next_cleanup_run: 2,
        active_cleanup_run: Some(2),
        cleanup_events: None,
        latest_review: None,
        db: Database::open_in_memory().unwrap(),
        active_scan_id: None,
        notice: None,
    };
    let task = update(
        &mut app,
        Message::CleanupCompleted {
            run: 1,
            scan_id: None,
            result: Ok(dupekit_core::CleanupOutcome {
                action: CleanupAction::Trash,
                removed: vec![],
                recovered_bytes: 0,
                failures: vec![],
            }),
        },
    );
    drop(task);
    assert!(matches!(app.screen, Screen::Home));
    assert_eq!(app.active_cleanup_run, Some(2));
}
#[test]
fn result_page_is_bounded_for_very_large_scans() {
    let total_groups = 250_000usize;
    let page = 20_833usize;
    let start = page * GROUPS_PER_PAGE;
    let end = (start + GROUPS_PER_PAGE).min(total_groups);
    assert!(end - start <= GROUPS_PER_PAGE);
    assert_eq!(GROUPS_PER_PAGE, 12);
}

#[test]
fn file_row_toggle_preserves_a_copy_in_its_group() {
    let first = DuplicateFileId(1);
    let second = DuplicateFileId(2);
    let group = DuplicateGroup::new(
        GroupId(1),
        42,
        vec![
            DuplicateFile {
                id: first,
                path: "first".into(),
                size: 42,
                modified: None,
            },
            DuplicateFile {
                id: second,
                path: "second".into(),
                size: 42,
                modified: None,
            },
        ],
    )
    .unwrap();
    let mut groups = vec![group];
    toggle_file(&mut groups, first);
    assert!(groups[0].is_selected(first));
    // Selecting the other row would select every copy, so the core rejects it.
    toggle_file(&mut groups, second);
    assert!(groups[0].is_selected(first));
    assert!(!groups[0].is_selected(second));
}

#[test]
fn refresh_restores_known_choices_by_path_and_keeps_new_group_defaults() {
    let previous = vec![duplicate_group(1, &["/a", "/b"], &[1])];
    let selections = selection_by_path(&previous);
    let mut refreshed = vec![
        // The scanner selected /c by default. The prior explicit choice
        // for /b and this new path's default are both retained.
        duplicate_group(2, &["/a", "/b", "/c"], &[2]),
        // This entirely new group must remain as the scanner supplied it.
        duplicate_group(3, &["/d", "/e"], &[1]),
    ];

    restore_selection_by_path(&mut refreshed, &selections);

    assert!(!refreshed[0].is_selected(refreshed[0].files[0].id));
    assert!(refreshed[0].is_selected(refreshed[0].files[1].id));
    assert!(refreshed[0].is_selected(refreshed[0].files[2].id));
    assert!(refreshed[1].is_selected(refreshed[1].files[1].id));
}

#[test]
fn refresh_regrouping_never_selects_every_copy() {
    // These two selected paths came from different former groups. They
    // can become one group after content changes or a newly found match.
    let previous = vec![
        duplicate_group(1, &["/keep-a", "/remove-a"], &[1]),
        duplicate_group(2, &["/keep-b", "/remove-b"], &[1]),
    ];
    let selections = selection_by_path(&previous);
    let mut regrouped = vec![duplicate_group(3, &["/remove-a", "/remove-b"], &[1])];

    restore_selection_by_path(&mut regrouped, &selections);

    assert_eq!(regrouped[0].selected_ids().len(), 1);
    assert!(regrouped[0].validate_selection().is_ok());
    // The scanner's formerly kept first file is retained as the safe tie-breaker.
    assert!(!regrouped[0].is_selected(regrouped[0].files[0].id));
}

#[test]
fn refresh_preserves_a_clear_selection_for_known_paths() {
    let previous = vec![duplicate_group(1, &["/a", "/b"], &[])];
    let selections = selection_by_path(&previous);
    let mut refreshed = vec![duplicate_group(2, &["/a", "/b"], &[1])];

    restore_selection_by_path(&mut refreshed, &selections);

    assert!(refreshed[0].selected_ids().is_empty());
}

#[test]
fn failed_refresh_restores_results_and_keeps_completed_history() {
    let previous = ScanResults {
        groups: vec![duplicate_group(1, &["/a", "/b"], &[1])],
        page: 0,
        scan_name: "Saved scan".into(),
        refresh_settings: RefreshSettings::from_scan(ScanSettings::default(), true),
    };
    let mut db = Database::open_in_memory().unwrap();
    let scan_id = completed_scan(&mut db);
    let mut app = App {
        screen: Screen::Scanning(ScanProgress::default()),
        paths: vec![],
        min_size: String::new(),
        max_size: String::new(),
        cache: false,
        history: vec![],
        scan_cancel: Some(CancellationToken::default()),
        scan_events: None,
        next_scan_run: 1,
        active_scan_run: Some(1),
        running_scan_id: Some(scan_id),
        scan_mode: Some(ScanMode::Refresh {
            selections: selection_by_path(&previous.groups),
            previous: previous.clone(),
        }),
        next_cleanup_run: 0,
        active_cleanup_run: None,
        cleanup_events: None,
        latest_review: None,
        db,
        active_scan_id: Some(scan_id),
        notice: None,
    };

    drop(update(
        &mut app,
        Message::ScanCompleted {
            run: 1,
            result: Err("scanner unavailable".into()),
        },
    ));

    assert_eq!(app.db.scan(scan_id).unwrap().status, ScanStatus::Completed);
    assert_eq!(app.active_scan_id, Some(scan_id));
    assert!(matches!(&app.screen, Screen::Results(results) if results.groups == previous.groups));
}

#[test]
fn cancelled_refresh_restores_results_and_keeps_completed_history() {
    let previous = ScanResults {
        groups: vec![duplicate_group(1, &["/a", "/b"], &[1])],
        page: 0,
        scan_name: "Saved scan".into(),
        refresh_settings: RefreshSettings::from_scan(ScanSettings::default(), true),
    };
    let mut db = Database::open_in_memory().unwrap();
    let scan_id = completed_scan(&mut db);
    let cancellation = CancellationToken::default();
    let mut app = App {
        screen: Screen::Scanning(ScanProgress::default()),
        paths: vec![],
        min_size: String::new(),
        max_size: String::new(),
        cache: false,
        history: vec![],
        scan_cancel: Some(cancellation.clone()),
        scan_events: None,
        next_scan_run: 1,
        active_scan_run: Some(1),
        running_scan_id: Some(scan_id),
        scan_mode: Some(ScanMode::Refresh {
            selections: selection_by_path(&previous.groups),
            previous: previous.clone(),
        }),
        next_cleanup_run: 0,
        active_cleanup_run: None,
        cleanup_events: None,
        latest_review: None,
        db,
        active_scan_id: Some(scan_id),
        notice: None,
    };

    drop(update(&mut app, Message::CancelScan));
    assert!(cancellation.is_cancelled());
    assert_eq!(app.db.scan(scan_id).unwrap().status, ScanStatus::Completed);
    drop(update(
        &mut app,
        Message::ScanCompleted {
            run: 1,
            result: Err("cancelled".into()),
        },
    ));

    assert_eq!(app.db.scan(scan_id).unwrap().status, ScanStatus::Completed);
    assert_eq!(app.active_scan_id, Some(scan_id));
    assert!(matches!(&app.screen, Screen::Results(results) if results.groups == previous.groups));
}
