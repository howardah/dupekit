//! Native Iced MVP. Results are paged, so the widget tree remains bounded.
use dupekit_core::{
    CancellationToken, CleanupAction, CleanupService, DuplicateFile, DuplicateFileId,
    DuplicateGroup, DuplicateScanner, FclonesScanner, ScanConfig, ScanPath, ScanResult,
    SelectionPolicy as CoreSelectionPolicy,
};
use dupekit_storage::{Database, NewCleanupAction, NewScan, ScanId};
use iced::widget::{
    Space, button, checkbox, column, container, pick_list, progress_bar, row, scrollable, text,
    text_input,
};
use iced::{
    Background, Border, Color, Element, Length, Shadow, Subscription, Task, Theme, Vector,
    alignment, time,
};
use std::{path::PathBuf, time::Duration};

const GROUPS_PER_PAGE: usize = 12;

fn main() -> iced::Result {
    iced::application("Dupekit", update, view)
        .subscription(subscription)
        .theme(|_| Theme::Dark)
        .window_size((1180.0, 800.0))
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
        acknowledged: bool,
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
    DismissNotice,
    ApplyPolicy(UiPolicy),
    PageBack,
    PageForward,
    AskTrash,
    AskDelete,
    CancelConfirm,
    TogglePermanentAcknowledgement(bool),
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
                    acknowledged: false,
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
        Message::TogglePermanentAcknowledgement(value) => {
            if let Screen::Confirm { acknowledged, .. } = &mut app.screen {
                *acknowledged = value;
            }
        }
        Message::ConfirmCleanup => {
            if let (
                Screen::Confirm {
                    permanent,
                    acknowledged,
                    ..
                },
                Some(result),
            ) = (app.screen.clone(), app.latest_result.clone())
            {
                if permanent && !acknowledged {
                    app.notice = Some("Acknowledge permanent deletion before continuing.".into());
                    return Task::none();
                }
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
        Message::DismissNotice => app.notice = None,
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
            acknowledged,
        } => confirmation(*permanent, *count, *bytes, *acknowledged),
        Screen::CleanupDone {
            permanent,
            count,
            bytes,
        } => cleanup_done(*permanent, *count, *bytes),
        Screen::History => history(app),
    };
    let mut content = column![header()].spacing(24).padding([24, 32]);
    if let Some(notice) = &app.notice {
        content = content.push(
            container(row![
                text(notice).width(Length::Fill),
                button("Dismiss")
                    .style(secondary_button)
                    .on_press(Message::DismissNotice)
            ])
            .padding(12)
            .width(Length::Fill)
            .style(alert_style),
        );
    }
    container(
        container(content.push(body))
            .max_width(1180)
            .center_x(Length::Fill),
    )
    .width(Length::Fill)
    .height(Length::Fill)
    .style(|_| container::Style {
        background: Some(Background::Color(BG)),
        text_color: Some(TEXT),
        ..container::Style::default()
    })
    .into()
}
fn header() -> Element<'static, Message> {
    container(
        row![
            column![
                text("dupekit").size(24),
                text("SAFE DUPLICATE CLEANUP").size(11).color(MUTED)
            ]
            .spacing(2),
            Space::with_width(Length::Fill),
            button("New scan")
                .style(secondary_button)
                .on_press(Message::Home),
            button("History")
                .style(secondary_button)
                .on_press(Message::OpenHistory)
        ]
        .spacing(8)
        .align_y(alignment::Vertical::Center),
    )
    .padding([0, 14])
    .into()
}
fn home(app: &App) -> Element<'_, Message> {
    let mut dirs = column![
        text("Scan locations").size(18),
        text("Add folders to compare. Files in preferred locations are kept by default.")
            .size(14)
            .color(MUTED)
    ]
    .spacing(8);
    if app.paths.is_empty() {
        dirs = dirs.push(
            container(
                column![
                    text("No locations added").size(16),
                    text("Choose one or more folders to start a safe duplicate scan.")
                        .size(13)
                        .color(MUTED)
                ]
                .spacing(5),
            )
            .padding(20)
            .width(Length::Fill)
            .style(raised_style),
        );
    }
    for (i, entry) in app.paths.iter().enumerate() {
        dirs = dirs.push(
            container(
                row![
                    checkbox("Keep files here by default", entry.preferred)
                        .on_toggle(move |_| Message::TogglePreferred(i))
                        .width(220),
                    text(entry.path.to_string_lossy())
                        .size(15)
                        .width(Length::Fill),
                    button("Remove")
                        .style(secondary_button)
                        .on_press(Message::RemovePath(i))
                ]
                .align_y(alignment::Vertical::Center)
                .spacing(12),
            )
            .padding(12)
            .width(Length::Fill)
            .style(card_style),
        );
    }
    dirs = dirs.push(
        button("Add folder")
            .style(secondary_button)
            .padding([10, 14])
            .on_press(Message::AddDirectory),
    );
    let options = column![
        text("Scan settings").size(18),
        text("Limit the files included in this scan.")
            .size(13)
            .color(MUTED),
        row![
            column![
                text("Minimum file size").size(13).color(MUTED),
                text_input("e.g. 1 MB", &app.min_size)
                    .on_input(Message::MinSize)
                    .padding(10)
            ]
            .width(Length::Fill),
            column![
                text("Maximum file size (optional)").size(13).color(MUTED),
                text_input("No limit", &app.max_size)
                    .on_input(Message::MaxSize)
                    .padding(10)
            ]
            .width(Length::Fill)
        ]
        .spacing(22),
        checkbox("Use fclones hash cache", app.cache).on_toggle(Message::ToggleCache),
        text(
            "fclones manages its own cache; Dupekit never duplicates file hashes in its database."
        )
        .size(13)
        .color(MUTED)
    ]
    .spacing(10)
    .padding(18);
    column![
        text("Find duplicate files").size(30),
        text("Review matching files, choose what to remove, and always keep at least one copy.")
            .size(15)
            .color(MUTED),
        container(dirs.padding(18)).style(card_style),
        container(options).style(card_style),
        row![
            Space::with_width(Length::Fill),
            button(text("Find duplicates").size(17))
                .padding([12, 22])
                .style(primary_button)
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
        text("Scanning locations").size(30),
        text("Preparing a safe comparison. Progress phases are activity indicators, not an exact percentage.").size(15).color(MUTED),
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
                    "Working…"
                } else {
                    "Waiting"
                })
                .color(if i < phase { SUCCESS } else { MUTED })
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
        .push(
            text("Scanning continues until fclones finishes comparing files.")
                .size(13)
                .color(MUTED),
        )
        .push(Space::with_height(Length::Fill))
        .push(row![
            Space::with_width(Length::Fill),
            button("Cancel scan")
                .style(secondary_button)
                .on_press(Message::CancelScan)
        ]);
    container(body.padding(28))
        .style(raised_style)
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
                text("Review duplicates").size(30),
                text(format!(
                    "{} · Select files to remove; one copy is always kept.",
                    results.scan_name
                ))
                .size(14)
                .color(MUTED)
            ],
            Space::with_width(Length::Fill),
            column![
                text("SELECTED").size(11).color(MUTED),
                text(format!("{} files · {}", count, bytes_label(bytes)))
                    .align_x(alignment::Horizontal::Right),
                text(format!(
                    "{} potentially reclaimable",
                    bytes_label(potential)
                ))
                .size(12)
                .color(MUTED)
                .align_x(alignment::Horizontal::Right)
            ]
        ],
        container(
            row![
                text(format!(
                    "{} groups · {} files",
                    results.groups.len(),
                    results.groups.iter().map(|g| g.files.len()).sum::<usize>()
                ))
                .size(14)
                .color(MUTED)
                .width(Length::Fill),
                text("Automatic selection").size(13).color(MUTED),
                pick_list(&UiPolicy::ALL[..], None::<UiPolicy>, Message::ApplyPolicy)
                    .placeholder("Select duplicates")
            ]
            .align_y(alignment::Vertical::Center)
            .spacing(12)
        )
        .padding(12)
        .style(card_style),
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
        container(
            row![
                column![
                    text("READY TO CLEAN UP").size(11).color(MUTED),
                    text(format!(
                        "{} selected · {} recoverable",
                        count,
                        bytes_label(bytes)
                    ))
                    .size(15)
                ],
                Space::with_width(Length::Fill),
                button("Permanently delete…")
                    .style(danger_button)
                    .on_press_maybe((count > 0).then_some(Message::AskDelete)),
                button("Move selected to Trash")
                    .style(primary_button)
                    .on_press_maybe((count > 0).then_some(Message::AskTrash))
            ]
            .align_y(alignment::Vertical::Center)
            .spacing(10)
        )
        .padding(14)
        .style(raised_style)
    ]
    .spacing(14)
    .height(Length::Fill)
    .into()
}
fn group_view(g: &DuplicateGroup) -> Element<'_, Message> {
    let mut body = column![row![
        text(format!("{} matching files", g.files.len())).size(16),
        Space::with_width(Length::Fill),
        text(format!("{} each", bytes_label(g.file_size)))
            .size(13)
            .color(MUTED)
    ]]
    .spacing(7);
    for f in &g.files {
        let id = f.id;
        let selected = g.is_selected(id);
        let kept = !selected && g.selected_ids().len() == g.files.len() - 1;
        body = body.push(
            button(
                row![
                    text(if selected { "☑" } else { "☐" })
                        .size(20)
                        .color(if selected { BLUE } else { MUTED }),
                    column![
                        text(f.path.to_string_lossy()).size(15),
                        text(format!(
                            "{} · modified {}",
                            bytes_label(g.file_size),
                            modified_label(f)
                        ))
                        .size(12)
                        .color(MUTED)
                    ]
                    .width(Length::Fill),
                    text(if kept { "KEPT" } else { "" }).size(11).color(SUCCESS)
                ]
                .align_y(alignment::Vertical::Center)
                .spacing(12),
            )
            .width(Length::Fill)
            .padding([10, 12])
            .style(row_button(selected))
            .on_press(Message::ToggleFile(id)),
        );
    }
    container(body.padding(14))
        .width(Length::Fill)
        .style(card_style)
        .into()
}
fn confirmation(
    permanent: bool,
    count: usize,
    bytes: u64,
    acknowledged: bool,
) -> Element<'static, Message> {
    let action = if permanent {
        "Permanently delete"
    } else {
        "Move to Trash"
    };
    let mut content = column![
        text(format!("{} {} files?", action, count)).size(28),
        text(format!(
            "{} will be affected, recovering approximately {}.",
            count,
            bytes_label(bytes)
        ))
        .color(MUTED),
        text(if permanent {
            "This action is irreversible. Files will not be sent to your system Trash."
        } else {
            "Files remain recoverable until the system Trash is emptied."
        })
        .size(14),
    ]
    .spacing(16);
    if permanent {
        content = content.push(
            checkbox("I understand these files cannot be recovered", acknowledged)
                .on_toggle(Message::TogglePermanentAcknowledgement),
        );
    }
    let action_button: Element<'static, Message> = if permanent {
        button(action)
            .style(danger_button)
            .on_press_maybe(acknowledged.then_some(Message::ConfirmCleanup))
            .into()
    } else {
        button(action)
            .style(primary_button)
            .on_press(Message::ConfirmCleanup)
            .into()
    };
    content = content.push(row![
        button("Cancel")
            .style(secondary_button)
            .on_press(Message::CancelConfirm),
        Space::with_width(Length::Fill),
        action_button
    ]);
    container(content.padding(28))
        .max_width(610)
        .center_x(Length::Fill)
        .center_y(Length::Fill)
        .style(if permanent { alert_style } else { raised_style })
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
            row![
                button("New scan")
                    .style(secondary_button)
                    .on_press(Message::Home),
                button("Review results")
                    .style(primary_button)
                    .on_press(Message::CancelConfirm)
            ]
            .spacing(10)
        ]
        .spacing(16)
        .padding(28),
    )
    .center_x(Length::Fill)
    .center_y(Length::Fill)
    .style(raised_style)
    .into()
}
fn history(app: &App) -> Element<'_, Message> {
    let mut list = column![
        text("Scan history").size(30),
        text("Reopen a saved scan to review its duplicate groups.")
            .size(14)
            .color(MUTED)
    ]
    .spacing(14);
    for scan in &app.history {
        list = list.push(
            container(
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
                    button("Open results")
                        .style(secondary_button)
                        .on_press(Message::Reopen(scan.id))
                ]
                .align_y(alignment::Vertical::Center),
            )
            .padding(14)
            .style(card_style),
        );
    }
    if app.history.is_empty() {
        list = list.push(
            container(
                column![
                    text("No saved scans").size(17),
                    text("Completed scans will appear here.")
                        .size(13)
                        .color(MUTED)
                ]
                .spacing(6),
            )
            .padding(22)
            .style(card_style),
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

// A small, deliberately restrained visual system. Keeping it local means the
// application can remain a single native binary without a web-style asset layer.
const BG: Color = Color {
    r: 0.063,
    g: 0.078,
    b: 0.094,
    a: 1.0,
};
const SURFACE: Color = Color {
    r: 0.094,
    g: 0.129,
    b: 0.176,
    a: 1.0,
};
const RAISED: Color = Color {
    r: 0.125,
    g: 0.169,
    b: 0.220,
    a: 1.0,
};
const BORDER: Color = Color {
    r: 0.173,
    g: 0.227,
    b: 0.290,
    a: 1.0,
};
const TEXT: Color = Color {
    r: 0.929,
    g: 0.949,
    b: 0.969,
    a: 1.0,
};
const MUTED: Color = Color {
    r: 0.576,
    g: 0.643,
    b: 0.722,
    a: 1.0,
};
const BLUE: Color = Color {
    r: 0.310,
    g: 0.549,
    b: 1.0,
    a: 1.0,
};
const BLUE_HOVER: Color = Color {
    r: 0.439,
    g: 0.639,
    b: 1.0,
    a: 1.0,
};
const DANGER: Color = Color {
    r: 0.875,
    g: 0.396,
    b: 0.439,
    a: 1.0,
};
const SUCCESS: Color = Color {
    r: 0.200,
    g: 0.710,
    b: 0.553,
    a: 1.0,
};

fn card_style(_: &Theme) -> container::Style {
    container::Style {
        background: Some(Background::Color(SURFACE)),
        text_color: Some(TEXT),
        border: Border {
            color: BORDER,
            width: 1.0,
            radius: 10.0.into(),
        },
        shadow: Shadow::default(),
    }
}
fn raised_style(_: &Theme) -> container::Style {
    container::Style {
        background: Some(Background::Color(RAISED)),
        text_color: Some(TEXT),
        border: Border {
            color: BORDER,
            width: 1.0,
            radius: 10.0.into(),
        },
        shadow: Shadow {
            color: Color {
                a: 0.25,
                ..Color::BLACK
            },
            offset: Vector::new(0.0, 5.0),
            blur_radius: 14.0,
        },
    }
}
fn alert_style(_: &Theme) -> container::Style {
    container::Style {
        background: Some(Background::Color(Color::from_rgb8(55, 35, 40))),
        text_color: Some(TEXT),
        border: Border {
            color: DANGER,
            width: 1.0,
            radius: 8.0.into(),
        },
        shadow: Shadow::default(),
    }
}
fn primary_button(_: &Theme, status: button::Status) -> button::Style {
    let color = match status {
        button::Status::Hovered => BLUE_HOVER,
        button::Status::Pressed => Color::from_rgb8(58, 112, 220),
        button::Status::Disabled => Color::from_rgb8(54, 75, 105),
        _ => BLUE,
    };
    button::Style {
        background: Some(Background::Color(color)),
        text_color: TEXT,
        border: Border {
            color,
            width: 1.0,
            radius: 8.0.into(),
        },
        shadow: Shadow::default(),
    }
}
fn secondary_button(_: &Theme, status: button::Status) -> button::Style {
    let background = matches!(status, button::Status::Hovered).then_some(Background::Color(RAISED));
    button::Style {
        background,
        text_color: if matches!(status, button::Status::Disabled) {
            MUTED
        } else {
            TEXT
        },
        border: Border {
            color: BORDER,
            width: 1.0,
            radius: 8.0.into(),
        },
        shadow: Shadow::default(),
    }
}
fn danger_button(_: &Theme, status: button::Status) -> button::Style {
    let background = matches!(status, button::Status::Hovered)
        .then_some(Background::Color(Color::from_rgb8(78, 42, 48)));
    button::Style {
        background,
        text_color: DANGER,
        border: Border {
            color: DANGER,
            width: 1.0,
            radius: 8.0.into(),
        },
        shadow: Shadow::default(),
    }
}
fn row_button(selected: bool) -> impl Fn(&Theme, button::Status) -> button::Style {
    move |_, status| {
        let background = if selected {
            Color::from_rgb8(31, 54, 83)
        } else if matches!(status, button::Status::Hovered | button::Status::Pressed) {
            RAISED
        } else {
            SURFACE
        };
        button::Style {
            background: Some(Background::Color(background)),
            text_color: TEXT,
            border: Border {
                color: if selected { BLUE } else { BORDER },
                width: 1.0,
                radius: 7.0.into(),
            },
            shadow: Shadow::default(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use dupekit_core::GroupId;
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
}
