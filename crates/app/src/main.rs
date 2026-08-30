//! Native Iced MVP. Results are paged, so the widget tree remains bounded.
use dupekit_core::{
    CancellationToken, CleanupAction, CleanupService, DuplicateFile, DuplicateFileId,
    DuplicateGroup, DuplicateScanner, FclonesScanner, ScanConfig, ScanPath, ScanResult,
    SelectionPolicy as CoreSelectionPolicy,
};
use dupekit_storage::{Database, NewCleanupAction, NewScan, ScanId};
use iced::widget::{
    Space, button, checkbox, column, container, horizontal_rule, pick_list, progress_bar, row,
    scrollable, text, text_input,
};
use iced::{Element, Length, Subscription, Task, Theme, alignment, time};
use std::{path::PathBuf, time::Duration};

const GROUPS_PER_PAGE: usize = 12;

fn main() -> iced::Result {
    iced::application("Dupekit", update, view)
        .subscription(subscription)
        .theme(|_| Theme::TokyoNight)
        .window_size((1040.0, 760.0))
        .run_with(|| (App::new(), Task::none()))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum UiPolicy {
    KeepFirst,
    KeepNewest,
    KeepOldest,
    PreferPreferred,
    Clear,
}
impl UiPolicy {
    const ALL: [Self; 5] = [
        Self::KeepFirst,
        Self::KeepNewest,
        Self::KeepOldest,
        Self::PreferPreferred,
        Self::Clear,
    ];
    fn core(self) -> CoreSelectionPolicy {
        match self {
            Self::KeepFirst => CoreSelectionPolicy::KeepFirst,
            Self::KeepNewest => CoreSelectionPolicy::KeepNewest,
            Self::KeepOldest => CoreSelectionPolicy::KeepOldest,
            Self::PreferPreferred => CoreSelectionPolicy::PreferPreferredDirectories,
            Self::Clear => CoreSelectionPolicy::ClearSelection,
        }
    }
}
impl std::fmt::Display for UiPolicy {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::KeepFirst => "Keep first",
            Self::KeepNewest => "Keep newest",
            Self::KeepOldest => "Keep oldest",
            Self::PreferPreferred => "Prefer preferred directories",
            Self::Clear => "Clear selection",
        })
    }
}
#[derive(Debug, Clone)]
struct ScanResults {
    groups: Vec<DuplicateGroup>,
    page: usize,
    scan_name: String,
}
#[derive(Debug, Clone)]
struct HistoryItem {
    id: ScanId,
    name: String,
    date: String,
    groups: usize,
    bytes: u64,
}
#[derive(Debug, Clone)]
enum Screen {
    Home,
    Scanning {
        progress: f32,
        phase: usize,
    },
    Results(ScanResults),
    Confirm {
        permanent: bool,
        count: usize,
        bytes: u64,
    },
    CleanupDone {
        permanent: bool,
        count: usize,
        bytes: u64,
    },
    History,
}
struct App {
    screen: Screen,
    paths: Vec<ScanPath>,
    min_size: String,
    max_size: String,
    cache: bool,
    history: Vec<HistoryItem>,
    scan_cancel: Option<CancellationToken>,
    latest_result: Option<ScanResult>,
    db: Database,
    active_scan_id: Option<ScanId>,
    notice: Option<String>,
}
impl App {
    fn new() -> Self {
        let db = Database::open("dupekit.sqlite3")
            .unwrap_or_else(|_| Database::open_in_memory().expect("SQLite must be available"));
        let history = history_items(&db);
        Self {
            screen: Screen::Home,
            paths: vec![],
            min_size: "1 MB".into(),
            max_size: String::new(),
            cache: true,
            history,
            scan_cancel: None,
            latest_result: None,
            db,
            active_scan_id: None,
            notice: None,
        }
    }
}
#[derive(Debug, Clone)]
enum Message {
    AddDirectory,
    DirectoryPicked(Option<PathBuf>),
    RemovePath(usize),
    TogglePreferred(usize),
    MinSize(String),
    MaxSize(String),
    ToggleCache(bool),
    StartScan,
    ScanCompleted(Result<dupekit_core::ScanResult, String>),
    CleanupCompleted(Result<dupekit_core::CleanupOutcome, String>),
    Tick,
    CancelScan,
    ToggleFile(DuplicateFileId),
    ApplyPolicy(UiPolicy),
    PageBack,
    PageForward,
    AskTrash,
    AskDelete,
    CancelConfirm,
    ConfirmCleanup,
    Home,
    OpenHistory,
    Reopen(ScanId),
}

fn subscription(app: &App) -> Subscription<Message> {
    match app.screen {
        Screen::Scanning { .. } => time::every(Duration::from_millis(120)).map(|_| Message::Tick),
        _ => Subscription::none(),
    }
}
fn update(app: &mut App, message: Message) -> Task<Message> {
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
        Message::StartScan => {
            if !app.paths.is_empty() {
                app.notice = None;
                let config = ScanConfig {
                    paths: app.paths.clone(),
                    min_size: parse_size(&app.min_size),
                    max_size: parse_size(&app.max_size),
                    cache: app.cache,
                };
                let cancellation = CancellationToken::default();
                let worker_cancellation = cancellation.clone();
                app.active_scan_id = match app.db.create_scan(&NewScan {
                    name: Some("Scan".into()),
                    started_at: std::time::SystemTime::now(),
                    paths: app.paths.clone(),
                }) {
                    Ok(id) => Some(id),
                    Err(error) => {
                        app.notice = Some(format!("Could not save scan history: {error}"));
                        None
                    }
                };
                app.scan_cancel = Some(cancellation);
                app.screen = Screen::Scanning {
                    progress: 0.0,
                    phase: 0,
                };
                return Task::perform(
                    async move {
                        let (events, _ignored_events) = std::sync::mpsc::channel();
                        FclonesScanner
                            .scan(config, events, worker_cancellation)
                            .map_err(|error| error.to_string())
                    },
                    Message::ScanCompleted,
                );
            }
        }
        Message::ScanCompleted(Ok(result)) => {
            app.scan_cancel = None;
            let mut displayed_result = result;
            if let Some(id) = app.active_scan_id {
                let _ = app.db.replace_results(
                    id,
                    &displayed_result.groups,
                    &displayed_result.summary,
                    std::time::SystemTime::now(),
                );
                // Reload database-owned IDs so subsequent checkbox updates target
                // the correct persisted files, including after multiple scans.
                if let Ok(groups) = app.db.groups(id) {
                    displayed_result = ScanResult::from_groups(groups);
                }
                app.history = history_items(&app.db);
            }
            if matches!(app.screen, Screen::Scanning { .. }) {
                app.latest_result = Some(displayed_result.clone());
                app.screen = Screen::Results(ScanResults {
                    groups: displayed_result.groups,
                    page: 0,
                    scan_name: "Current scan".into(),
                });
            }
        }
        Message::ScanCompleted(Err(error)) => {
            app.scan_cancel = None;
            if let Some(id) = app.active_scan_id {
                let _ = app.db.finish_scan(
                    id,
                    dupekit_storage::ScanStatus::Failed,
                    std::time::SystemTime::now(),
                );
                app.history = history_items(&app.db);
            }
            if matches!(app.screen, Screen::Scanning { .. }) {
                app.notice = Some(format!("Scan failed: {error}"));
                app.screen = Screen::Home;
            }
        }
        Message::Tick => {
            if let Screen::Scanning { progress, phase } = &mut app.screen {
                *progress = (*progress + 0.018).min(1.0);
                *phase = ((*progress * 4.0) as usize).min(3);
                if *progress >= 1.0 {
                    // Actual results arrive through `ScanCompleted`; this timer only keeps
                    // the phase indicator responsive while fclones runs on Iced's executor.
                }
            }
        }
        Message::CancelScan => {
            if let Some(cancel) = &app.scan_cancel {
                cancel.cancel();
            }
            if let Some(id) = app.active_scan_id {
                let _ = app.db.finish_scan(
                    id,
                    dupekit_storage::ScanStatus::Cancelled,
                    std::time::SystemTime::now(),
                );
                app.history = history_items(&app.db);
            }
            app.screen = Screen::Home
        }
        Message::ToggleFile(id) => {
            if let Screen::Results(r) = &mut app.screen {
                toggle_file(&mut r.groups, id);
                if let Some(group) = r
                    .groups
                    .iter()
                    .find(|group| group.files.iter().any(|file| file.id == id))
                {
                    let _ = app.db.set_selected(id, group.is_selected(id));
                }
                app.latest_result = Some(ScanResult::from_groups(r.groups.clone()));
            }
        }
        Message::ApplyPolicy(p) => {
            if let Screen::Results(r) = &mut app.screen {
                apply_policy(&mut r.groups, p, &app.paths);
                // Clear persisted selections first, then apply the new safe set.
                for group in &r.groups {
                    for file in &group.files {
                        let _ = app.db.set_selected(file.id, false);
                    }
                }
                for group in &r.groups {
                    for file in group.selected_files() {
                        let _ = app.db.set_selected(file.id, true);
                    }
                }
                app.latest_result = Some(ScanResult::from_groups(r.groups.clone()));
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
                };
            }
        }
        Message::CancelConfirm => {
            if let Some(result) = &app.latest_result {
                app.screen = Screen::Results(ScanResults {
                    groups: result.groups.clone(),
                    page: 0,
                    scan_name: "Current scan".into(),
                });
            } else {
                app.screen = Screen::Home;
            }
        }
        Message::ConfirmCleanup => {
            if let (Screen::Confirm { permanent, .. }, Some(result)) =
                (app.screen.clone(), app.latest_result.clone())
            {
                let action = if permanent {
                    CleanupAction::PermanentDelete
                } else {
                    CleanupAction::Trash
                };
                return Task::perform(
                    async move {
                        CleanupService::plan(&result, action)
                            .map(CleanupService::preflight)
                            .and_then(CleanupService::execute)
                            .map_err(|error| error.to_string())
                    },
                    Message::CleanupCompleted,
                );
            }
        }
        Message::CleanupCompleted(Ok(outcome)) => {
            if let Some(scan_id) = app.active_scan_id {
                let _ = app.db.record_cleanup(
                    scan_id,
                    &NewCleanupAction {
                        created_at: std::time::SystemTime::now(),
                        action: if outcome.action == CleanupAction::Trash {
                            "trash".into()
                        } else {
                            "permanent_delete".into()
                        },
                        affected_files: outcome.removed.len() as u64,
                        recovered_bytes: outcome.recovered_bytes,
                    },
                );
            }
            app.screen = Screen::CleanupDone {
                permanent: outcome.action == CleanupAction::PermanentDelete,
                count: outcome.removed.len(),
                bytes: outcome.recovered_bytes,
            };
            if !outcome.failures.is_empty() {
                app.notice = Some(format!(
                    "{} file(s) could not be cleaned up: {}",
                    outcome.failures.len(),
                    outcome
                        .failures
                        .iter()
                        .map(|failure| format!("{} ({})", failure.path.display(), failure.message))
                        .collect::<Vec<_>>()
                        .join(", ")
                ));
            }
        }
        Message::CleanupCompleted(Err(error)) => {
            app.notice = Some(format!("Cleanup was not performed: {error}"));
            if let Some(result) = &app.latest_result {
                app.screen = Screen::Results(ScanResults {
                    groups: result.groups.clone(),
                    page: 0,
                    scan_name: "Current scan".into(),
                });
            } else {
                app.screen = Screen::Home;
            }
        }
        Message::Home => app.screen = Screen::Home,
        Message::OpenHistory => app.screen = Screen::History,
        Message::Reopen(id) => {
            if let Ok(groups) = app.db.groups(id) {
                app.active_scan_id = Some(id);
                app.latest_result = Some(ScanResult::from_groups(groups.clone()));
                app.screen = Screen::Results(ScanResults {
                    groups,
                    page: 0,
                    scan_name: "Saved scan".into(),
                });
            }
        }
    };
    Task::none()
}
fn toggle_file(groups: &mut [DuplicateGroup], id: DuplicateFileId) {
    for group in groups {
        if group.files.iter().any(|file| file.id == id) {
            let _ = group.set_selected(id, !group.is_selected(id));
            return;
        }
    }
}
fn apply_policy(groups: &mut [DuplicateGroup], policy: UiPolicy, preferred: &[ScanPath]) {
    let preferred = preferred
        .iter()
        .filter(|p| p.preferred)
        .map(|p| p.path.clone())
        .collect::<Vec<_>>();
    for group in groups {
        group.apply_selection(policy.core(), &preferred);
    }
}
fn totals(groups: &[DuplicateGroup]) -> (usize, u64) {
    (
        groups.iter().map(|g| g.selected_ids().len()).sum(),
        groups.iter().map(DuplicateGroup::selected_bytes).sum(),
    )
}
fn history_items(db: &Database) -> Vec<HistoryItem> {
    db.scans()
        .unwrap_or_default()
        .into_iter()
        .map(|scan| {
            let summary = scan.summary.unwrap_or_default();
            HistoryItem {
                id: scan.id,
                name: scan.name.unwrap_or_else(|| "Untitled scan".into()),
                date: "Saved scan".into(),
                groups: summary.duplicate_groups as usize,
                bytes: summary.recoverable_bytes,
            }
        })
        .collect()
}
fn parse_size(value: &str) -> Option<u64> {
    let value = value.trim();
    if value.is_empty() {
        return None;
    }
    let mut parts = value.split_whitespace();
    let number = parts.next()?.parse::<f64>().ok()?;
    let unit = parts.next().unwrap_or("B").to_ascii_uppercase();
    let multiplier = match unit.as_str() {
        "B" | "BYTE" | "BYTES" => 1.0,
        "K" | "KB" | "KIB" => 1024.0,
        "M" | "MB" | "MIB" => 1024.0 * 1024.0,
        "G" | "GB" | "GIB" => 1024.0 * 1024.0 * 1024.0,
        "T" | "TB" | "TIB" => 1024.0 * 1024.0 * 1024.0 * 1024.0,
        _ => return None,
    };
    (number >= 0.0).then_some((number * multiplier) as u64)
}

fn view(app: &App) -> Element<'_, Message> {
    let body = match &app.screen {
        Screen::Home => home(app),
        Screen::Scanning { progress, phase } => scanning(*progress, *phase),
        Screen::Results(r) => results(r),
        Screen::Confirm {
            permanent,
            count,
            bytes,
        } => confirmation(*permanent, *count, *bytes),
        Screen::CleanupDone {
            permanent,
            count,
            bytes,
        } => cleanup_done(*permanent, *count, *bytes),
        Screen::History => history(app),
    };
    let mut content = column![header()].spacing(18).padding(28);
    if let Some(notice) = &app.notice {
        content = content.push(container(text(notice)).padding(12).width(Length::Fill));
    }
    container(content.push(body))
        .width(Length::Fill)
        .height(Length::Fill)
        .into()
}
fn header() -> Element<'static, Message> {
    row![
        column![
            text("dupekit").size(28),
            text("safe duplicate cleanup").size(13)
        ],
        Space::with_width(Length::Fill),
        button("History").on_press(Message::OpenHistory),
        button("New scan").on_press(Message::Home)
    ]
    .align_y(alignment::Vertical::Center)
    .into()
}
fn home(app: &App) -> Element<'_, Message> {
    let mut dirs = column![
        text("Directories").size(22),
        text("Add folders to compare. Preferred folders are kept by default.").size(14)
    ]
    .spacing(10);
    if app.paths.is_empty() {
        dirs = dirs.push(
            container(text("No folders selected yet").size(16))
                .padding(22)
                .width(Length::Fill),
        );
    }
    for (i, entry) in app.paths.iter().enumerate() {
        dirs = dirs.push(
            row![
                checkbox("Preferred", entry.preferred)
                    .on_toggle(move |_| Message::TogglePreferred(i))
                    .width(110),
                text(entry.path.to_string_lossy()).width(Length::Fill),
                button("Remove").on_press(Message::RemovePath(i))
            ]
            .align_y(alignment::Vertical::Center)
            .spacing(12),
        );
    }
    dirs = dirs.push(button("+ Add directory").on_press(Message::AddDirectory));
    let options = column![
        text("Scan options").size(22),
        row![
            column![
                text("Minimum file size"),
                text_input("e.g. 1 MB", &app.min_size).on_input(Message::MinSize)
            ],
            column![
                text("Maximum file size (optional)"),
                text_input("No limit", &app.max_size).on_input(Message::MaxSize)
            ]
        ]
        .spacing(22),
        checkbox("Use fclones hash cache", app.cache).on_toggle(Message::ToggleCache),
        text(
            "fclones manages its own cache; Dupekit never duplicates file hashes in its database."
        )
        .size(13)
    ]
    .spacing(10);
    column![
        dirs,
        horizontal_rule(1),
        options,
        Space::with_height(Length::Fill),
        row![
            Space::with_width(Length::Fill),
            button(text("Find duplicates").size(17))
                .padding([12, 22])
                .on_press_maybe((!app.paths.is_empty()).then_some(Message::StartScan))
        ]
    ]
    .spacing(18)
    .into()
}
fn scanning(progress: f32, phase: usize) -> Element<'static, Message> {
    let labels = [
        "Discovering files",
        "Grouping by size",
        "Partial hashing",
        "Full hashing",
    ];
    let mut body = column![
        text("Scanning your folders").size(28),
        text("Dupekit is working in the background. You can cancel safely at any time.").size(15),
        Space::with_height(20)
    ]
    .spacing(12);
    for (i, label) in labels.iter().enumerate() {
        body = body
            .push(row![
                text(*label).width(Length::Fill),
                text(if i < phase {
                    "Complete"
                } else if i == phase {
                    "Working"
                } else {
                    "Waiting"
                })
            ])
            .push(progress_bar(
                0.0..=1.0,
                if i < phase {
                    1.0
                } else if i == phase {
                    progress * 4.0 - phase as f32
                } else {
                    0.0
                },
            ));
    }
    body = body
        .push(Space::with_height(24))
        .push(text(format!("{:.0}% complete", progress * 100.0)).size(18))
        .push(Space::with_height(Length::Fill))
        .push(row![
            Space::with_width(Length::Fill),
            button("Cancel scan").on_press(Message::CancelScan)
        ]);
    container(body)
        .max_width(680)
        .center_x(Length::Fill)
        .height(Length::Fill)
        .into()
}
fn results(results: &ScanResults) -> Element<'_, Message> {
    let (count, bytes) = totals(&results.groups);
    let potential: u64 = results
        .groups
        .iter()
        .map(|g| g.file_size * (g.files.len() as u64 - 1))
        .sum();
    let start = results.page * GROUPS_PER_PAGE;
    let end = (start + GROUPS_PER_PAGE).min(results.groups.len());
    let mut groups = column![].spacing(10);
    for g in &results.groups[start..end] {
        groups = groups.push(group_view(g));
    }
    let pages = results.groups.len().div_ceil(GROUPS_PER_PAGE);
    column![
        row![
            column![
                text("Duplicates found").size(28),
                text(&results.scan_name).size(14)
            ],
            Space::with_width(Length::Fill),
            column![
                text(format!("{} selected · {}", count, bytes_label(bytes)))
                    .align_x(alignment::Horizontal::Right),
                text(format!("{} recoverable", bytes_label(potential)))
                    .align_x(alignment::Horizontal::Right)
            ]
        ],
        row![
            text(format!(
                "{} groups · {} files",
                results.groups.len(),
                results.groups.iter().map(|g| g.files.len()).sum::<usize>()
            ))
            .width(Length::Fill),
            pick_list(&UiPolicy::ALL[..], None::<UiPolicy>, Message::ApplyPolicy)
                .placeholder("Select duplicates…")
        ]
        .align_y(alignment::Vertical::Center),
        horizontal_rule(1),
        scrollable(groups).height(Length::Fill),
        row![
            button("‹ Previous").on_press_maybe((results.page > 0).then_some(Message::PageBack)),
            Space::with_width(Length::Fill),
            text(format!(
                "Page {} of {} · rendering {} groups",
                results.page + 1,
                pages.max(1),
                end - start
            )),
            Space::with_width(Length::Fill),
            button("Next ›")
                .on_press_maybe((end < results.groups.len()).then_some(Message::PageForward))
        ],
        row![
            button("Permanently delete…").on_press_maybe((count > 0).then_some(Message::AskDelete)),
            Space::with_width(Length::Fill),
            button("Move selected to Trash")
                .on_press_maybe((count > 0).then_some(Message::AskTrash))
        ]
        .align_y(alignment::Vertical::Center)
    ]
    .spacing(14)
    .height(Length::Fill)
    .into()
}
fn group_view(g: &DuplicateGroup) -> Element<'_, Message> {
    let mut body = column![row![
        text(format!("{} copies", g.files.len())).size(17),
        Space::with_width(Length::Fill),
        text(format!("{} each", bytes_label(g.file_size))).size(14)
    ]]
    .spacing(7);
    for f in &g.files {
        let id = f.id;
        body = body.push(
            row![
                checkbox("", g.is_selected(id)).on_toggle(move |_| Message::ToggleFile(id)),
                column![
                    text(f.path.to_string_lossy()).size(15),
                    text(format!(
                        "{} · modified {}",
                        bytes_label(g.file_size),
                        modified_label(f)
                    ))
                    .size(12)
                ]
                .width(Length::Fill)
            ]
            .align_y(alignment::Vertical::Center),
        );
    }
    container(body.padding(14)).width(Length::Fill).into()
}
fn confirmation(permanent: bool, count: usize, bytes: u64) -> Element<'static, Message> {
    let action = if permanent {
        "Permanently delete"
    } else {
        "Move to Trash"
    };
    container(
        column![
            text(format!("{} {} files?", action, count)).size(27),
            text(format!(
                "This will affect approximately {}.{}",
                bytes_label(bytes),
                if permanent {
                    " This cannot be undone."
                } else {
                    " A preflight check runs first."
                }
            )),
            text(if permanent {
                "Permanent deletion is irreversible. Review your selection."
            } else {
                "Files remain recoverable until the system trash is emptied."
            })
            .size(14),
            row![
                button("Cancel").on_press(Message::CancelConfirm),
                Space::with_width(Length::Fill),
                button(action).on_press(Message::ConfirmCleanup)
            ]
        ]
        .spacing(18)
        .padding(28),
    )
    .max_width(610)
    .center_x(Length::Fill)
    .center_y(Length::Fill)
    .into()
}
fn cleanup_done(permanent: bool, count: usize, bytes: u64) -> Element<'static, Message> {
    container(
        column![
            text(if permanent {
                "Files permanently deleted"
            } else {
                "Files moved to Trash"
            })
            .size(29),
            text(format!(
                "{} files affected · {} recovered",
                count,
                bytes_label(bytes)
            )),
            text("Filesystem failures are recorded in scan history.").size(14),
            button("Back to home").on_press(Message::Home)
        ]
        .spacing(16)
        .padding(28),
    )
    .center_x(Length::Fill)
    .center_y(Length::Fill)
    .into()
}
fn history(app: &App) -> Element<'_, Message> {
    let mut list = column![
        text("Scan history").size(28),
        text("Reopen a completed scan to review its duplicate groups.").size(14)
    ]
    .spacing(14);
    for scan in &app.history {
        list = list.push(
            row![
                column![
                    text(&scan.name).size(17),
                    text(format!(
                        "{} · {} groups · {} recoverable",
                        scan.date,
                        scan.groups,
                        bytes_label(scan.bytes)
                    ))
                    .size(13)
                ]
                .width(Length::Fill),
                button("Reopen").on_press(Message::Reopen(scan.id))
            ]
            .align_y(alignment::Vertical::Center),
        );
    }
    container(list).max_width(780).into()
}
fn bytes_label(bytes: u64) -> String {
    const U: [&str; 5] = ["B", "KB", "MB", "GB", "TB"];
    let (mut n, mut i) = (bytes as f64, 0);
    while n >= 1024.0 && i < U.len() - 1 {
        n /= 1024.0;
        i += 1;
    }
    if i == 0 {
        format!("{} {}", bytes, U[i])
    } else {
        format!("{n:.1} {}", U[i])
    }
}
fn modified_label(file: &DuplicateFile) -> String {
    file.modified
        .and_then(|t| t.duration_since(std::time::SystemTime::UNIX_EPOCH).ok())
        .map(|d| format!("Unix + {} days", d.as_secs() / 86_400))
        .unwrap_or_else(|| "unknown date".into())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn parses_human_sizes() {
        assert_eq!(parse_size("1 MB"), Some(1_048_576));
        assert_eq!(parse_size("2.5 gb"), Some(2_684_354_560));
        assert_eq!(parse_size(""), None);
        assert_eq!(parse_size("large"), None);
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
}
