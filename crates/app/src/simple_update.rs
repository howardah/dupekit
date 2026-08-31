use crate::*;
use iced::Task;

pub(super) fn handle(app: &mut App, message: Message) -> Task<Message> {
    match message {
        Message::AddDirectory => {
            return Task::perform(
                async {
                    rfd::AsyncFileDialog::new()
                        .pick_folder()
                        .await
                        .map(|h| h.path().to_path_buf())
                },
                Message::DirectoryPicked,
            );
        }
        Message::DirectoryPicked(Some(path)) => {
            if !app.paths.iter().any(|p| p.path == path) {
                app.paths.push(ScanPath {
                    path,
                    preferred: false,
                });
            }
        }
        Message::DirectoryPicked(None) => {}
        Message::RemovePath(i) => {
            app.paths.remove(i);
        }
        Message::TogglePreferred(i) => {
            if let Some(path) = app.paths.get_mut(i) {
                path.preferred = !path.preferred;
            }
        }
        Message::MinSize(v) => app.min_size = v,
        Message::MaxSize(v) => app.max_size = v,
        Message::ToggleCache(v) => app.cache = v,
        Message::ToggleRefreshSettings => {
            if let Screen::Results(results) = &mut app.screen {
                results.refresh_settings.expanded = !results.refresh_settings.expanded;
                results.refresh_settings.error = None;
                app.latest_review = Some(results.clone());
            }
        }
        Message::RefreshMinSize(value) => {
            if let Screen::Results(results) = &mut app.screen {
                results.refresh_settings.min_size = value;
                results.refresh_settings.error = None;
                app.latest_review = Some(results.clone());
            }
        }
        Message::RefreshMaxSize(value) => {
            if let Screen::Results(results) = &mut app.screen {
                results.refresh_settings.max_size = value;
                results.refresh_settings.error = None;
                app.latest_review = Some(results.clone());
            }
        }
        Message::ToggleRefreshCache(value) => {
            if let Screen::Results(results) = &mut app.screen {
                results.refresh_settings.cache = value;
                results.refresh_settings.error = None;
                app.latest_review = Some(results.clone());
            }
        }
        _ => {}
    };
    Task::none()
}
