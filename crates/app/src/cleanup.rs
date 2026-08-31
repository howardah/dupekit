use crate::utilities::*;
use crate::*;
use dupekit_core::CleanupService;
use iced::Task;

pub(super) fn handle(app: &mut App, message: Message) -> Task<Message> {
    match message {
        Message::ConfirmCleanup => {
            if let (
                Screen::Confirm {
                    permanent,
                    acknowledged,
                    ..
                },
                Some(review),
            ) = (app.screen.clone(), app.latest_review.clone())
            {
                // A cleanup is destructive. Ignore repeat clicks and do not
                // begin a second operation until this one has reported back.
                if app.active_cleanup_run.is_some() {
                    return Task::none();
                }
                if permanent && !acknowledged {
                    app.notice = Some("Acknowledge permanent deletion before continuing.".into());
                    return Task::none();
                }
                let action = if permanent {
                    CleanupAction::PermanentDelete
                } else {
                    CleanupAction::Trash
                };
                app.next_cleanup_run = app.next_cleanup_run.wrapping_add(1);
                let run = app.next_cleanup_run;
                let scan_id = app.active_scan_id;
                let result = ScanResult::from_groups(review.groups);
                let plan = match CleanupService::plan(&result, action) {
                    Ok(plan) => plan,
                    Err(error) => {
                        app.notice = Some(format!("Cleanup could not start: {error}"));
                        return Task::none();
                    }
                };
                let total = plan.files.len();
                let (progress_sender, progress_receiver) = std::sync::mpsc::channel();
                app.active_cleanup_run = Some(run);
                app.cleanup_events = Some(progress_receiver);
                app.screen = Screen::Cleaning(CleanupProgress {
                    action,
                    phase: CleanupProgressPhase::Checking,
                    processed: 0,
                    total,
                    current: None,
                });
                return Task::perform(
                    async move {
                        let preflight = CleanupService::preflight(plan);
                        let result = if preflight.missing.is_empty() && preflight.changed.is_empty()
                        {
                            CleanupService::execute_with_updates(preflight, |update| {
                                let _ = progress_sender.send(CleanupProgressEvent {
                                    phase: update.phase,
                                    processed: update.processed,
                                    total: update.total,
                                    path: update.path,
                                });
                            })
                            .map_err(|error| error.to_string())
                        } else {
                            Err(preflight_failure_message(&preflight))
                        };
                        Message::CleanupCompleted {
                            run,
                            scan_id,
                            result,
                        }
                    },
                    |message| message,
                );
            }
        }
        Message::CleanupCompleted {
            run,
            scan_id,
            result: Ok(outcome),
        } => {
            if app.active_cleanup_run != Some(run) {
                return Task::none();
            }
            app.active_cleanup_run = None;
            app.cleanup_events = None;
            if let Some(scan_id) = scan_id {
                let action_name = if outcome.action == CleanupAction::Trash {
                    "trash"
                } else {
                    "permanent_delete"
                };
                if let Err(error) = app.db.record_cleanup_outcome(
                    scan_id,
                    action_name,
                    std::time::SystemTime::now(),
                    &outcome,
                ) {
                    append_notice(
                        app,
                        format!("Files were cleaned, but the cleanup audit was not saved: {error}"),
                    );
                }
                if let Err(error) = app.db.reconcile_removed_files(scan_id, &outcome.removed) {
                    append_notice(
                        app,
                        format!("Files were cleaned, but results could not be reconciled: {error}"),
                    );
                } else {
                    if app.active_scan_id == Some(scan_id) {
                        match app.db.groups(scan_id) {
                            Ok(groups) => {
                                if let Some(review) = &mut app.latest_review {
                                    review.groups = groups;
                                    review.page = review.page.min(
                                        review.groups.len().saturating_sub(1) / GROUPS_PER_PAGE,
                                    );
                                }
                            }
                            Err(error) => append_notice(
                                app,
                                format!(
                                    "Files were cleaned, but refreshed results could not be loaded: {error}"
                                ),
                            ),
                        }
                    }
                    refresh_history(app);
                }
            }
            // Do not pull a user away from a screen they navigated to while
            // cleanup was running; the audit above still belongs to the scan
            // that initiated the operation.
            if matches!(app.screen, Screen::Cleaning(_)) {
                app.screen = Screen::CleanupDone {
                    permanent: outcome.action == CleanupAction::PermanentDelete,
                    count: outcome.removed.len(),
                    bytes: outcome.recovered_bytes,
                };
            }
            if !outcome.failures.is_empty() {
                append_notice(
                    app,
                    format!(
                        "{} file(s) could not be cleaned up: {}",
                        outcome.failures.len(),
                        outcome
                            .failures
                            .iter()
                            .map(|failure| format!(
                                "{} ({})",
                                failure.path.display(),
                                failure.message
                            ))
                            .collect::<Vec<_>>()
                            .join(", ")
                    ),
                );
            }
        }
        Message::CleanupCompleted {
            run,
            scan_id: _,
            result: Err(error),
        } => {
            if app.active_cleanup_run != Some(run) {
                return Task::none();
            }
            app.active_cleanup_run = None;
            app.cleanup_events = None;
            app.notice = Some(error);
            if let Some(review) = &app.latest_review {
                app.screen = Screen::Results(review.clone());
            } else {
                app.screen = Screen::Home;
            }
        }
        _ => {}
    };
    Task::none()
}
