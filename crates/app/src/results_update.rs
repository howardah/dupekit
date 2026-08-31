use crate::utilities::*;
use crate::*;
use iced::Task;

pub(super) fn handle(app: &mut App, message: Message) -> Task<Message> {
    match message {
        Message::ToggleFile(id) => {
            if let Screen::Results(r) = &mut app.screen {
                let before = r.groups.clone();
                let was_selected = r
                    .groups
                    .iter()
                    .find(|group| group.files.iter().any(|file| file.id == id))
                    .is_some_and(|group| group.is_selected(id));
                toggle_file(&mut r.groups, id);
                let is_selected = r
                    .groups
                    .iter()
                    .find(|group| group.files.iter().any(|file| file.id == id))
                    .is_some_and(|group| group.is_selected(id));
                if was_selected && is_selected {
                    app.notice = Some("Keep at least one copy in each group.".into());
                }
                if let Some(group) = r
                    .groups
                    .iter()
                    .find(|group| group.files.iter().any(|file| file.id == id))
                    && app.active_scan_id.is_some()
                    && let Err(error) = app.db.set_selected(id, group.is_selected(id))
                {
                    r.groups = before;
                    app.notice = Some(format!("Selection was not saved and was reverted: {error}"));
                }
                app.latest_review = Some(r.clone());
            }
        }
        Message::ApplyPolicy(p) => {
            if let Screen::Results(r) = &mut app.screen {
                let before = r.groups.clone();
                apply_policy(&mut r.groups, p, &app.paths);
                // Clear persisted selections first, then apply the new safe set.
                let all_files = r
                    .groups
                    .iter()
                    .flat_map(|group| group.files.iter().map(|file| file.id))
                    .collect::<Vec<_>>();
                let selected_files = r
                    .groups
                    .iter()
                    .flat_map(|group| group.selected_files().map(|file| file.id))
                    .collect::<Vec<_>>();
                for file in all_files {
                    if app.active_scan_id.is_some()
                        && let Err(error) = app.db.set_selected(file, false)
                    {
                        r.groups = before;
                        app.notice = Some(format!(
                            "Selection policy was not saved and was reverted: {error}"
                        ));
                        app.latest_review = Some(r.clone());
                        return Task::none();
                    }
                }
                for file in selected_files {
                    if app.active_scan_id.is_some()
                        && let Err(error) = app.db.set_selected(file, true)
                    {
                        r.groups = before;
                        app.notice = Some(format!(
                            "Selection policy was not saved and was reverted: {error}"
                        ));
                        app.latest_review = Some(r.clone());
                        return Task::none();
                    }
                }
                app.latest_review = Some(r.clone());
            }
        }
        Message::PageBack => {
            if let Screen::Results(r) = &mut app.screen {
                r.page = r.page.saturating_sub(1);
            }
        }
        Message::PageForward => {
            if let Screen::Results(r) = &mut app.screen {
                r.page = (r.page + 1).min(r.groups.len().saturating_sub(1) / GROUPS_PER_PAGE);
            }
        }
        Message::AskTrash | Message::AskDelete => {
            if let Screen::Results(r) = &app.screen {
                let (count, bytes) = totals(&r.groups);
                app.screen = Screen::Confirm {
                    permanent: matches!(message, Message::AskDelete),
                    count,
                    bytes,
                    acknowledged: false,
                };
            }
        }
        Message::CancelConfirm => {
            if let Some(review) = &app.latest_review {
                app.screen = Screen::Results(review.clone());
            } else {
                app.screen = Screen::Home;
            }
        }
        Message::TogglePermanentAcknowledgement(value) => {
            if let Screen::Confirm { acknowledged, .. } = &mut app.screen {
                *acknowledged = value;
            }
        }
        _ => {}
    };
    Task::none()
}
