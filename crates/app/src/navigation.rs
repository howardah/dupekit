use crate::*;
use iced::Task;

pub(super) fn handle(app: &mut App, message: Message) -> Task<Message> {
    match message {
        Message::Home => {
            if !app.scan_worker_active() && app.active_cleanup_run.is_none() {
                app.screen = Screen::Home;
            }
        }
        Message::OpenHistory => {
            if !app.scan_worker_active() && app.active_cleanup_run.is_none() {
                app.screen = Screen::History;
            }
        }
        Message::DismissNotice => app.notice = None,
        Message::Reopen(id) => {
            if app.scan_worker_active() || app.active_cleanup_run.is_some() {
                return Task::none();
            }
            match app.db.scan(id) {
                Err(error) => app.notice = Some(format!("Could not open saved scan: {error}")),
                Ok(scan) if scan.status != ScanStatus::Completed => {
                    app.notice =
                        Some("Only completed scans have results that can be reopened.".into());
                }
                Ok(scan) => match app.db.groups(id) {
                    Err(error) => {
                        app.notice = Some(format!("Could not load saved results: {error}"))
                    }
                    Ok(groups) => {
                        app.paths = scan.paths;
                        app.active_scan_id = Some(id);
                        let review = ScanResults {
                            groups,
                            page: 0,
                            scan_name: scan.name.unwrap_or_else(|| "Saved scan".into()),
                            refresh_settings: RefreshSettings::from_scan(
                                scan.settings,
                                scan.settings_recorded,
                            ),
                        };
                        app.latest_review = Some(review.clone());
                        app.screen = Screen::Results(review);
                    }
                },
            }
        }
        _ => {}
    };
    Task::none()
}
