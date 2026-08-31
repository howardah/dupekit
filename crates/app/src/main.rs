//! Native Iced MVP. Results are paged, so the widget tree remains bounded.
use dupekit_core::{
    CancellationToken, CleanupAction, CleanupProgressPhase, DuplicateFileId, DuplicateGroup,
    ScanEvent, ScanPath, ScanResult, SelectionPolicy as CoreSelectionPolicy,
};
use dupekit_storage::{Database, NewScan, ScanId, ScanSettings, ScanStatus};
use iced::{Subscription, Task, Theme, time};
use std::{collections::BTreeMap, fs, path::PathBuf, sync::mpsc::Receiver, time::Duration};

mod cleanup;
mod navigation;
mod results_update;
mod scan;
mod simple_update;
mod styles;
mod utilities;
mod view;
mod views_history;
mod views_home;
mod views_results;

use utilities::{history_items, parse_size_input, size_input_value};
use view::view;

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
    refresh_settings: RefreshSettings,
}
#[derive(Debug, Clone)]
struct RefreshSettings {
    min_size: String,
    max_size: String,
    cache: bool,
    expanded: bool,
    error: Option<String>,
    legacy_unrecorded: bool,
}
impl RefreshSettings {
    fn from_scan(settings: ScanSettings, recorded: bool) -> Self {
        Self {
            min_size: settings.min_size.map(size_input_value).unwrap_or_default(),
            max_size: settings.max_size.map(size_input_value).unwrap_or_default(),
            cache: settings.cache,
            expanded: false,
            error: None,
            legacy_unrecorded: !recorded,
        }
    }

    fn stored(&self) -> Result<ScanSettings, String> {
        let min_size = parse_size_input(&self.min_size, "Minimum file size")?;
        let max_size = parse_size_input(&self.max_size, "Maximum file size")?;
        if min_size.zip(max_size).is_some_and(|(min, max)| min > max) {
            return Err("Minimum file size cannot exceed maximum file size.".into());
        }
        Ok(ScanSettings {
            min_size,
            max_size,
            cache: self.cache,
        })
    }

    fn summary(&self) -> String {
        format!(
            "Min {} · Max {} · Cache {}",
            if self.min_size.trim().is_empty() {
                "none"
            } else {
                self.min_size.trim()
            },
            if self.max_size.trim().is_empty() {
                "none"
            } else {
                self.max_size.trim()
            },
            if self.cache { "on" } else { "off" }
        )
    }
}
/// Records why the in-flight scan was started. Refreshes deliberately replace
/// an existing completed result set; a failed or cancelled refresh must leave
/// that result set intact.
#[derive(Debug, Clone)]
enum ScanMode {
    Initial {
        settings: RefreshSettings,
    },
    Refresh {
        previous: ScanResults,
        selections: BTreeMap<PathBuf, bool>,
    },
}
#[derive(Debug, Clone)]
struct HistoryItem {
    id: ScanId,
    name: String,
    date: String,
    status: ScanStatus,
    groups: usize,
    bytes: u64,
}
#[derive(Debug, Clone)]
enum Screen {
    Home,
    Scanning(ScanProgress),
    /// fclones cannot be interrupted while it owns its cache database. Keep
    /// the scan task alive until it unwinds and releases that lock.
    Cancelling,
    Results(ScanResults),
    Confirm {
        permanent: bool,
        count: usize,
        bytes: u64,
        acknowledged: bool,
    },
    Cleaning(CleanupProgress),
    CleanupDone {
        permanent: bool,
        count: usize,
        bytes: u64,
    },
    History,
}

/// This is deliberately a view of the scanner's last event, rather than an
/// estimate. `pulse` only animates an unknown-length activity indicator.
#[derive(Debug, Clone, PartialEq)]
struct ScanProgress {
    phase: String,
    processed: u64,
    total: Option<u64>,
    pulse: f32,
}

#[derive(Debug, Clone)]
struct CleanupProgress {
    action: CleanupAction,
    phase: CleanupProgressPhase,
    processed: usize,
    total: usize,
    current: Option<PathBuf>,
}

#[derive(Debug)]
struct CleanupProgressEvent {
    phase: CleanupProgressPhase,
    processed: usize,
    total: usize,
    path: PathBuf,
}

impl Default for ScanProgress {
    fn default() -> Self {
        Self {
            phase: "Starting scan".into(),
            processed: 0,
            total: None,
            pulse: 0.0,
        }
    }
}

impl ScanProgress {
    fn apply(&mut self, event: &ScanEvent) {
        match event {
            ScanEvent::Started => {
                self.phase = "Starting scan".into();
                self.processed = 0;
                self.total = None;
            }
            ScanEvent::PhaseStarted { name, total } => {
                self.phase = name.clone();
                self.processed = 0;
                self.total = *total;
            }
            ScanEvent::Progress { processed, total } => {
                // Progress trackers can be updated by concurrent workers.
                // Ignore an older delivery rather than making a real bar move
                // backwards.
                self.processed = self.processed.max(*processed);
                // A tracker supplies its total with every update. Preserve a
                // preceding total only if a backend ever omits it.
                if total.is_some() {
                    self.total = *total;
                }
            }
            ScanEvent::FilesDiscovered(count) => {
                self.phase = format!("Found {count} files in duplicate groups");
                self.processed = *count;
                self.total = None;
            }
            ScanEvent::GroupFound(_)
            | ScanEvent::Finished(_)
            | ScanEvent::Failed(_)
            | ScanEvent::Cancelled => {}
        }
    }

    fn fraction(&self) -> Option<f32> {
        self.total.and_then(|total| {
            (total > 0).then_some((self.processed.min(total) as f32 / total as f32).min(1.0))
        })
    }
}
struct App {
    screen: Screen,
    paths: Vec<ScanPath>,
    min_size: String,
    max_size: String,
    cache: bool,
    history: Vec<HistoryItem>,
    scan_cancel: Option<CancellationToken>,
    scan_events: Option<Receiver<ScanEvent>>,
    next_scan_run: u64,
    active_scan_run: Option<u64>,
    running_scan_id: Option<ScanId>,
    scan_mode: Option<ScanMode>,
    next_cleanup_run: u64,
    active_cleanup_run: Option<u64>,
    cleanup_events: Option<Receiver<CleanupProgressEvent>>,
    latest_review: Option<ScanResults>,
    db: Database,
    active_scan_id: Option<ScanId>,
    notice: Option<String>,
}
impl App {
    fn new() -> Self {
        let db = local_database_path()
            .and_then(|path| Database::open(path).ok())
            .unwrap_or_else(|| Database::open_in_memory().expect("SQLite must be available"));
        let history = history_items(&db).unwrap_or_default();
        Self {
            screen: Screen::Home,
            paths: vec![],
            min_size: "1 MB".into(),
            max_size: String::new(),
            cache: true,
            history,
            scan_cancel: None,
            scan_events: None,
            next_scan_run: 0,
            active_scan_run: None,
            running_scan_id: None,
            scan_mode: None,
            next_cleanup_run: 0,
            active_cleanup_run: None,
            cleanup_events: None,
            latest_review: None,
            db,
            active_scan_id: None,
            notice: None,
        }
    }

    fn scan_worker_active(&self) -> bool {
        self.active_scan_run.is_some()
    }
}

fn local_database_path() -> Option<PathBuf> {
    let directory = dirs::data_local_dir()?.join("dupekit");
    fs::create_dir_all(&directory).ok()?;
    Some(directory.join("dupekit.sqlite3"))
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
    ToggleRefreshSettings,
    RefreshMinSize(String),
    RefreshMaxSize(String),
    ToggleRefreshCache(bool),
    StartScan,
    RefreshResults,
    ScanCompleted {
        run: u64,
        result: Result<dupekit_core::ScanResult, String>,
    },
    CleanupCompleted {
        run: u64,
        scan_id: Option<ScanId>,
        result: Result<dupekit_core::CleanupOutcome, String>,
    },
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
        Screen::Scanning(_) | Screen::Cleaning(_) => {
            time::every(Duration::from_millis(80)).map(|_| Message::Tick)
        }
        _ => Subscription::none(),
    }
}

fn update(app: &mut App, message: Message) -> Task<Message> {
    match message {
        message @ (Message::AddDirectory
        | Message::DirectoryPicked(_)
        | Message::RemovePath(_)
        | Message::TogglePreferred(_)
        | Message::MinSize(_)
        | Message::MaxSize(_)
        | Message::ToggleCache(_)
        | Message::ToggleRefreshSettings
        | Message::RefreshMinSize(_)
        | Message::RefreshMaxSize(_)
        | Message::ToggleRefreshCache(_)) => simple_update::handle(app, message),
        message @ (Message::StartScan
        | Message::RefreshResults
        | Message::ScanCompleted { .. }
        | Message::Tick
        | Message::CancelScan) => scan::handle(app, message),
        message @ (Message::ToggleFile(_)
        | Message::ApplyPolicy(_)
        | Message::PageBack
        | Message::PageForward
        | Message::AskTrash
        | Message::AskDelete
        | Message::CancelConfirm
        | Message::TogglePermanentAcknowledgement(_)) => results_update::handle(app, message),
        message @ (Message::ConfirmCleanup | Message::CleanupCompleted { .. }) => {
            cleanup::handle(app, message)
        }
        message @ (Message::Home
        | Message::OpenHistory
        | Message::DismissNotice
        | Message::Reopen(_)) => navigation::handle(app, message),
    }
}

#[cfg(test)]
mod tests;
