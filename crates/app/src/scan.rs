use crate::utilities::*;
use crate::*;
use dupekit_core::{DuplicateScanner, FclonesScanner, ScanConfig};
use iced::Task;

pub(super) fn handle(app: &mut App, message: Message) -> Task<Message> {
    match message {
        Message::StartScan => {
            if app.active_scan_run.is_some() {
                app.notice = Some(
                    "A scan is already running. Cancel or wait for it to finish first.".into(),
                );
                return Task::none();
            }
            if app.paths.is_empty() {
                app.notice = Some("Add at least one folder before starting a scan.".into());
            } else {
                let settings = RefreshSettings {
                    min_size: app.min_size.clone(),
                    max_size: app.max_size.clone(),
                    cache: app.cache,
                    expanded: false,
                    error: None,
                    legacy_unrecorded: false,
                };
                let stored = match settings.stored() {
                    Ok(value) => value,
                    Err(error) => {
                        app.notice = Some(error);
                        return Task::none();
                    }
                };
                return begin_scan(
                    app,
                    ScanConfig {
                        paths: app.paths.clone(),
                        min_size: stored.min_size,
                        max_size: stored.max_size,
                        cache: stored.cache,
                    },
                    ScanMode::Initial { settings },
                );
            }
        }
        Message::RefreshResults => {
            if app.active_scan_run.is_some() || app.active_cleanup_run.is_some() {
                return Task::none();
            }
            let Screen::Results(previous) = &mut app.screen else {
                return Task::none();
            };
            let stored = match previous.refresh_settings.stored() {
                Ok(value) => value,
                Err(error) => {
                    previous.refresh_settings.error = Some(error);
                    return Task::none();
                }
            };
            previous.refresh_settings.error = None;
            let selections = selection_by_path(&previous.groups);
            let previous = previous.clone();
            return begin_scan(
                app,
                ScanConfig {
                    paths: app.paths.clone(),
                    min_size: stored.min_size,
                    max_size: stored.max_size,
                    cache: stored.cache,
                },
                ScanMode::Refresh {
                    previous,
                    selections,
                },
            );
        }
        Message::ScanCompleted { run, result } => {
            if app.active_scan_run != Some(run) {
                return Task::none();
            }
            let Some(mode) = app.scan_mode.take() else {
                app.notice = Some("Scan finished without its saved configuration.".into());
                return Task::none();
            };
            let was_cancelling = matches!(app.screen, Screen::Cancelling)
                || app
                    .scan_cancel
                    .as_ref()
                    .is_some_and(CancellationToken::is_cancelled);
            app.scan_events = None;
            app.scan_cancel = None;
            app.active_scan_run = None;
            // Cancellation is only complete once this task returns: that is
            // when fclones has dropped its cache and released its OS lock.
            // Its result (or its eventual cancellation error) must never be
            // persisted or shown as a failed/completed scan.
            if was_cancelling {
                app.running_scan_id = None;
                match mode {
                    ScanMode::Initial { .. } => {
                        app.active_scan_id = None;
                        app.screen = Screen::Home;
                    }
                    ScanMode::Refresh { previous, .. } => {
                        app.latest_review = Some(previous.clone());
                        app.screen = Screen::Results(previous);
                        append_notice(
                            app,
                            "Refresh cancelled; previous results are unchanged.".into(),
                        );
                    }
                }
                return Task::none();
            }
            match result {
                Ok(result) => {
                    let mut displayed_result = result;
                    if let ScanMode::Refresh { selections, .. } = &mode {
                        restore_selection_by_path(&mut displayed_result.groups, selections);
                        displayed_result = ScanResult::from_groups(displayed_result.groups);
                    }
                    if let Some(id) = app.running_scan_id.take() {
                        let stored_settings = match &mode {
                            ScanMode::Initial { settings } => settings.stored(),
                            ScanMode::Refresh { previous, .. } => {
                                previous.refresh_settings.stored()
                            }
                        }
                        .expect("scan settings were validated before the worker started");
                        let persisted = match app.db.replace_results_and_load_with_settings(
                            id,
                            &displayed_result.groups,
                            &displayed_result.summary,
                            std::time::SystemTime::now(),
                            Some(stored_settings),
                        ) {
                            // Database-owned IDs are returned by the same
                            // transaction that writes the new result set.
                            Ok(groups) => {
                                displayed_result = ScanResult::from_groups(groups);
                                true
                            }
                            Err(error) => {
                                append_notice(
                                    app,
                                    format!(
                                        "Scan finished, but results could not be saved: {error}"
                                    ),
                                );
                                // Do not let scanner-owned IDs masquerade as database IDs.
                                false
                            }
                        };
                        refresh_history(app);
                        if persisted && matches!(app.screen, Screen::Scanning(_)) {
                            app.active_scan_id = Some(id);
                        } else if !persisted {
                            match &mode {
                                ScanMode::Initial { .. } => app.active_scan_id = None,
                                ScanMode::Refresh { previous, .. } => {
                                    app.latest_review = Some(previous.clone());
                                    app.screen = Screen::Results(previous.clone());
                                    return Task::none();
                                }
                            }
                        }
                    }
                    if matches!(app.screen, Screen::Scanning(_)) {
                        let (scan_name, mut refresh_settings) = match &mode {
                            ScanMode::Initial { settings } => {
                                ("Current scan".into(), settings.clone())
                            }
                            ScanMode::Refresh { previous, .. } => (
                                previous.scan_name.clone(),
                                previous.refresh_settings.clone(),
                            ),
                        };
                        refresh_settings.expanded = false;
                        refresh_settings.error = None;
                        refresh_settings.legacy_unrecorded = false;
                        let review = ScanResults {
                            groups: displayed_result.groups,
                            page: 0,
                            scan_name,
                            refresh_settings,
                        };
                        app.latest_review = Some(review.clone());
                        app.screen = Screen::Results(review);
                    }
                }
                Err(error) => {
                    if matches!(mode, ScanMode::Initial { .. })
                        && let Some(id) = app.running_scan_id.take()
                    {
                        if let Err(persist_error) = app.db.finish_scan(
                            id,
                            dupekit_storage::ScanStatus::Failed,
                            std::time::SystemTime::now(),
                        ) {
                            append_notice(
                                app,
                                format!(
                                    "Scan failed and history could not be updated: {persist_error}"
                                ),
                            );
                        }
                        refresh_history(app);
                        if app.active_scan_id == Some(id) {
                            app.active_scan_id = None;
                        }
                    }
                    if matches!(app.screen, Screen::Scanning(_)) {
                        match mode {
                            ScanMode::Initial { .. } => {
                                append_notice(app, format!("Scan failed: {error}"));
                                app.screen = Screen::Home;
                            }
                            ScanMode::Refresh { previous, .. } => {
                                app.running_scan_id = None;
                                app.latest_review = Some(previous.clone());
                                app.screen = Screen::Results(previous);
                                append_notice(
                                    app,
                                    format!(
                                        "Refresh failed; previous results are unchanged: {error}"
                                    ),
                                );
                            }
                        }
                    }
                }
            }
        }
        Message::Tick => {
            if let Screen::Scanning(progress) = &mut app.screen {
                if let Some(receiver) = &app.scan_events {
                    // The scanner itself coalesces progress events. Keep this
                    // cap as a second guard against a UI stall if another
                    // backend is ever noisier.
                    for _ in 0..256 {
                        let Ok(event) = receiver.try_recv() else {
                            break;
                        };
                        progress.apply(&event);
                    }
                }
                // The pulse communicates ongoing work only when fclones did
                // not provide a total; it is never presented as completion.
                progress.pulse = (progress.pulse + 0.08) % 1.0;
            } else if let Screen::Cleaning(progress) = &mut app.screen
                && let Some(receiver) = &app.cleanup_events
            {
                while let Ok(event) = receiver.try_recv() {
                    progress.processed = event.processed;
                    progress.total = event.total;
                    progress.phase = event.phase;
                    progress.current = Some(event.path);
                }
            }
        }
        Message::CancelScan => {
            if !matches!(app.screen, Screen::Scanning(_)) {
                return Task::none();
            }
            if let Some(cancel) = &app.scan_cancel {
                cancel.cancel();
            }
            let cancelled_scan_id = app.running_scan_id.take();
            if matches!(app.scan_mode, Some(ScanMode::Initial { .. }))
                && let Some(id) = cancelled_scan_id
            {
                if let Err(error) = app.db.finish_scan(
                    id,
                    dupekit_storage::ScanStatus::Cancelled,
                    std::time::SystemTime::now(),
                ) {
                    append_notice(
                        app,
                        format!("Scan cancellation could not be saved: {error}"),
                    );
                }
                refresh_history(app);
            }
            // Do not clear the worker bookkeeping here. The synchronous
            // fclones call may still own ~/.cache/fclones/db for some time.
            // ScanCompleted performs that cleanup after it has returned.
            if matches!(app.scan_mode, Some(ScanMode::Initial { .. }))
                && app.active_scan_id == cancelled_scan_id
            {
                app.active_scan_id = None;
            }
            app.screen = Screen::Cancelling
        }
        _ => {}
    };
    Task::none()
}

fn begin_scan(app: &mut App, config: ScanConfig, mode: ScanMode) -> Task<Message> {
    app.notice = None;
    app.next_scan_run = app.next_scan_run.wrapping_add(1);
    let run = app.next_scan_run;
    let cancellation = CancellationToken::default();
    let worker_cancellation = cancellation.clone();
    let (events, event_receiver) = std::sync::mpsc::channel();

    if matches!(mode, ScanMode::Initial { .. }) {
        app.active_scan_id = match app.db.create_scan(&NewScan {
            name: Some("Scan".into()),
            started_at: std::time::SystemTime::now(),
            paths: app.paths.clone(),
            settings: ScanSettings {
                min_size: config.min_size,
                max_size: config.max_size,
                cache: config.cache,
            },
        }) {
            Ok(id) => Some(id),
            Err(error) => {
                app.notice = Some(format!("Could not save scan history: {error}"));
                None
            }
        };
    }
    // A refresh updates the same saved scan atomically, keeping its cleanup
    // audit and history entry instead of creating a misleading duplicate row.
    app.running_scan_id = app.active_scan_id;
    app.scan_mode = Some(mode);
    app.scan_cancel = Some(cancellation);
    app.scan_events = Some(event_receiver);
    app.active_scan_run = Some(run);
    app.screen = Screen::Scanning(ScanProgress::default());
    Task::perform(
        async move {
            let result = FclonesScanner
                .scan(config, events, worker_cancellation)
                .map_err(|error| error.to_string());
            Message::ScanCompleted { run, result }
        },
        |message| message,
    )
}
