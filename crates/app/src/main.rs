//! Native Iced MVP. Results are paged, so the widget tree remains bounded.
use dupekit_core::{
    CancellationToken, CleanupAction, CleanupService, DuplicateFile, DuplicateFileId,
    DuplicateGroup, DuplicateScanner, FclonesScanner, ScanConfig, ScanEvent, ScanPath, ScanResult,
    SelectionPolicy as CoreSelectionPolicy,
};
use dupekit_storage::{Database, NewScan, ScanId, ScanStatus};
use iced::widget::{
    Space, button, checkbox, column, container, pick_list, progress_bar, row, scrollable, text,
    text_input,
};
use iced::{
    Background, Border, Color, Element, Length, Shadow, Subscription, Task, Theme, Vector,
    alignment, time,
};
use std::{collections::BTreeMap, path::PathBuf, sync::mpsc::Receiver, time::Duration};

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
/// Records why the in-flight scan was started. Refreshes deliberately replace
/// an existing completed result set; a failed or cancelled refresh must leave
/// that result set intact.
#[derive(Debug, Clone)]
enum ScanMode {
    Initial,
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
    latest_result: Option<ScanResult>,
    db: Database,
    active_scan_id: Option<ScanId>,
    notice: Option<String>,
}
impl App {
    fn new() -> Self {
        let db = Database::open("dupekit.sqlite3")
            .unwrap_or_else(|_| Database::open_in_memory().expect("SQLite must be available"));
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
            latest_result: None,
            db,
            active_scan_id: None,
            notice: None,
        }
    }

    fn scan_worker_active(&self) -> bool {
        self.active_scan_run.is_some()
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
        Screen::Scanning(_) => time::every(Duration::from_millis(80)).map(|_| Message::Tick),
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
            if app.active_scan_run.is_some() {
                app.notice = Some(
                    "A scan is already running. Cancel or wait for it to finish first.".into(),
                );
                return Task::none();
            }
            if app.paths.is_empty() {
                app.notice = Some("Add at least one folder before starting a scan.".into());
            } else {
                let min_size = match parse_size_input(&app.min_size, "Minimum file size") {
                    Ok(value) => value,
                    Err(error) => {
                        app.notice = Some(error);
                        return Task::none();
                    }
                };
                let max_size = match parse_size_input(&app.max_size, "Maximum file size") {
                    Ok(value) => value,
                    Err(error) => {
                        app.notice = Some(error);
                        return Task::none();
                    }
                };
                if min_size.zip(max_size).is_some_and(|(min, max)| min > max) {
                    app.notice = Some("Minimum file size cannot exceed maximum file size.".into());
                    return Task::none();
                }
                return begin_scan(
                    app,
                    ScanConfig {
                        paths: app.paths.clone(),
                        min_size,
                        max_size,
                        cache: app.cache,
                    },
                    ScanMode::Initial,
                );
            }
        }
        Message::RefreshResults => {
            if app.active_scan_run.is_some() || app.active_cleanup_run.is_some() {
                return Task::none();
            }
            let Screen::Results(previous) = &app.screen else {
                return Task::none();
            };
            let min_size = match parse_size_input(&app.min_size, "Minimum file size") {
                Ok(value) => value,
                Err(error) => {
                    app.notice = Some(error);
                    return Task::none();
                }
            };
            let max_size = match parse_size_input(&app.max_size, "Maximum file size") {
                Ok(value) => value,
                Err(error) => {
                    app.notice = Some(error);
                    return Task::none();
                }
            };
            if min_size.zip(max_size).is_some_and(|(min, max)| min > max) {
                app.notice = Some("Minimum file size cannot exceed maximum file size.".into());
                return Task::none();
            }
            let selections = selection_by_path(&previous.groups);
            return begin_scan(
                app,
                ScanConfig {
                    paths: app.paths.clone(),
                    min_size,
                    max_size,
                    cache: app.cache,
                },
                ScanMode::Refresh {
                    previous: previous.clone(),
                    selections,
                },
            );
        }
        Message::ScanCompleted { run, result } => {
            if app.active_scan_run != Some(run) {
                return Task::none();
            }
            let mode = app.scan_mode.take().unwrap_or(ScanMode::Initial);
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
                    ScanMode::Initial => {
                        app.active_scan_id = None;
                        app.screen = Screen::Home;
                    }
                    ScanMode::Refresh { previous, .. } => {
                        app.latest_result = Some(ScanResult::from_groups(previous.groups.clone()));
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
                        let persisted = match app.db.replace_results_and_load(
                            id,
                            &displayed_result.groups,
                            &displayed_result.summary,
                            std::time::SystemTime::now(),
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
                                ScanMode::Initial => app.active_scan_id = None,
                                ScanMode::Refresh { previous, .. } => {
                                    app.latest_result =
                                        Some(ScanResult::from_groups(previous.groups.clone()));
                                    app.screen = Screen::Results(previous.clone());
                                    return Task::none();
                                }
                            }
                        }
                    }
                    if matches!(app.screen, Screen::Scanning(_)) {
                        app.latest_result = Some(displayed_result.clone());
                        app.screen = Screen::Results(ScanResults {
                            groups: displayed_result.groups,
                            page: 0,
                            scan_name: "Current scan".into(),
                        });
                    }
                }
                Err(error) => {
                    if matches!(mode, ScanMode::Initial)
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
                            ScanMode::Initial => {
                                append_notice(app, format!("Scan failed: {error}"));
                                app.screen = Screen::Home;
                            }
                            ScanMode::Refresh { previous, .. } => {
                                app.running_scan_id = None;
                                app.latest_result =
                                    Some(ScanResult::from_groups(previous.groups.clone()));
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
            if matches!(app.scan_mode, Some(ScanMode::Initial))
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
            if matches!(app.scan_mode, Some(ScanMode::Initial))
                && app.active_scan_id == cancelled_scan_id
            {
                app.active_scan_id = None;
            }
            app.screen = Screen::Cancelling
        }
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
                app.latest_result = Some(ScanResult::from_groups(r.groups.clone()));
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
                        app.latest_result = Some(ScanResult::from_groups(r.groups.clone()));
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
                        app.latest_result = Some(ScanResult::from_groups(r.groups.clone()));
                        return Task::none();
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
                app.active_cleanup_run = Some(run);
                return Task::perform(
                    async move {
                        let result = CleanupService::plan(&result, action)
                            .map(CleanupService::preflight)
                            .and_then(CleanupService::execute)
                            .map_err(|error| error.to_string());
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
                            Ok(groups) => app.latest_result = Some(ScanResult::from_groups(groups)),
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
            if matches!(app.screen, Screen::Confirm { .. }) {
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
        Message::Home => {
            if !app.scan_worker_active() {
                app.screen = Screen::Home;
            }
        }
        Message::OpenHistory => {
            if !app.scan_worker_active() {
                app.screen = Screen::History;
            }
        }
        Message::DismissNotice => app.notice = None,
        Message::Reopen(id) => {
            if app.scan_worker_active() {
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
                        app.latest_result = Some(ScanResult::from_groups(groups.clone()));
                        app.screen = Screen::Results(ScanResults {
                            groups,
                            page: 0,
                            scan_name: scan.name.unwrap_or_else(|| "Saved scan".into()),
                        });
                    }
                },
            }
        }
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

    if matches!(mode, ScanMode::Initial) {
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

fn selection_by_path(groups: &[DuplicateGroup]) -> BTreeMap<PathBuf, bool> {
    groups
        .iter()
        .flat_map(|group| {
            group
                .files
                .iter()
                .map(|file| (file.path.clone(), group.is_selected(file.id)))
        })
        .collect()
}

/// Restores choices from the preceding result set by path, rather than by the
/// database IDs which are replaced on every scan. New files retain fclones'
/// preferred-directory default. If files have regrouped and the old choices
/// would remove every copy, retain the scanner's default kept copy.
fn restore_selection_by_path(groups: &mut [DuplicateGroup], selections: &BTreeMap<PathBuf, bool>) {
    for group in groups {
        let default_kept = group
            .files
            .iter()
            .find(|file| !group.is_selected(file.id))
            .map(|file| file.id);
        let desired = group
            .files
            .iter()
            .filter_map(|file| match selections.get(&file.path) {
                Some(selected) => selected.then_some(file.id),
                // A new file has no former choice, so preserve the scanner's
                // preferred-directory default for it.
                None => group.is_selected(file.id).then_some(file.id),
            })
            .collect::<Vec<_>>();
        let has_known_path = group
            .files
            .iter()
            .any(|file| selections.contains_key(&file.path));
        // A completely new group keeps its scanner default. Unlike that case,
        // an all-false known selection intentionally clears this group.
        if !has_known_path {
            continue;
        }
        let file_ids = group.files.iter().map(|file| file.id).collect::<Vec<_>>();
        for id in file_ids {
            let _ = group.set_selected(id, false);
        }
        let keep = (desired.len() == group.files.len())
            .then_some(default_kept.unwrap_or(group.files[0].id));
        for id in desired {
            if Some(id) != keep {
                let _ = group.set_selected(id, true);
            }
        }
    }
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
fn refresh_history(app: &mut App) {
    match history_items(&app.db) {
        Ok(history) => app.history = history,
        Err(error) => app.notice = Some(format!("Could not refresh scan history: {error}")),
    }
}
fn append_notice(app: &mut App, message: String) {
    if let Some(existing) = &mut app.notice {
        existing.push('\n');
        existing.push_str(&message);
    } else {
        app.notice = Some(message);
    }
}
fn history_items(db: &Database) -> Result<Vec<HistoryItem>, dupekit_storage::StorageError> {
    Ok(db
        .scans()?
        .into_iter()
        .map(|scan| {
            let summary = scan.summary.unwrap_or_default();
            HistoryItem {
                id: scan.id,
                name: scan.name.unwrap_or_else(|| "Untitled scan".into()),
                date: format_scan_time(scan.finished_at.unwrap_or(scan.started_at)),
                status: scan.status,
                groups: summary.duplicate_groups as usize,
                bytes: summary.recoverable_bytes,
            }
        })
        .collect())
}
fn format_scan_time(time: std::time::SystemTime) -> String {
    match time.duration_since(std::time::UNIX_EPOCH) {
        Ok(duration) => format!("{} UTC", duration.as_secs()),
        Err(_) => "Unknown date".into(),
    }
}
fn parse_size_input(value: &str, field: &str) -> Result<Option<u64>, String> {
    let value = value.trim();
    if value.is_empty() {
        return Ok(None);
    }
    let mut parts = value.split_whitespace();
    let number_text = parts.next().expect("non-empty input has a first token");
    let number = number_text.parse::<f64>().map_err(|_| {
        format!("{field} must be a number followed by an optional unit (for example, 1 MB).")
    })?;
    if !number.is_finite() || number < 0.0 {
        return Err(format!("{field} must be a finite, non-negative size."));
    }
    let unit = parts.next().unwrap_or("B").to_ascii_uppercase();
    if parts.next().is_some() {
        return Err(format!("{field} has unexpected trailing text."));
    }
    let multiplier = match unit.as_str() {
        "B" | "BYTE" | "BYTES" => 1.0,
        "K" | "KB" | "KIB" => 1024.0,
        "M" | "MB" | "MIB" => 1024.0 * 1024.0,
        "G" | "GB" | "GIB" => 1024.0 * 1024.0 * 1024.0,
        "T" | "TB" | "TIB" => 1024.0 * 1024.0 * 1024.0 * 1024.0,
        _ => {
            return Err(format!(
                "{field} uses an unknown unit. Use B, KB, MB, GB, or TB."
            ));
        }
    };
    let bytes = number * multiplier;
    if !bytes.is_finite() || bytes > u64::MAX as f64 {
        return Err(format!("{field} is too large."));
    }
    Ok(Some(bytes as u64))
}

fn view(app: &App) -> Element<'_, Message> {
    let body = match &app.screen {
        Screen::Home => home(app),
        Screen::Scanning(progress) => scanning(progress),
        Screen::Cancelling => cancelling(),
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
    let mut content = column![header(app)].spacing(24).padding([24, 32]);
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
fn header(app: &App) -> Element<'static, Message> {
    let available = !app.scan_worker_active();
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
                .on_press_maybe(available.then_some(Message::Home)),
            button("History")
                .style(secondary_button)
                .on_press_maybe(available.then_some(Message::OpenHistory))
        ]
        .spacing(8)
        .align_y(alignment::Vertical::Center),
    )
    .padding([0, 14])
    .into()
}
fn cancelling() -> Element<'static, Message> {
    container(
        column![
            text("Cancelling scan").size(30),
            text("Waiting for fclones to safely finish and release its cache lock.")
                .size(15)
                .color(MUTED),
            Space::with_height(20),
            text("The scan's results will be discarded. You can start another scan as soon as this screen closes.")
                .size(14)
                .color(MUTED),
        ]
        .spacing(12)
        .padding(28),
    )
    .style(raised_style)
    .max_width(680)
    .center_x(Length::Fill)
    .height(Length::Fill)
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
fn scanning(progress: &ScanProgress) -> Element<'static, Message> {
    let (value, detail) = match progress.fraction() {
        Some(fraction) => (
            fraction,
            format!(
                "{} of {} processed",
                progress.processed,
                progress.total.unwrap_or_default()
            ),
        ),
        None => (
            // Iced does not offer an indeterminate ProgressBar. This moving
            // segment is explicitly labelled activity, not a percentage.
            0.15 + progress.pulse * 0.70,
            "Working — this phase does not report a total".into(),
        ),
    };
    let mut body = column![
        text("Scanning locations").size(30),
        text("Progress comes directly from fclones when it provides a total.")
            .size(15)
            .color(MUTED),
        Space::with_height(20),
        text(progress.phase.clone()).size(18),
        text(detail).size(13).color(MUTED),
        progress_bar(0.0..=1.0, value),
    ]
    .spacing(12);
    body = body
        .push(Space::with_height(24))
        .push(
            text("A bar is only a percentage when the scanner reports its total.")
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
                button("Refresh results")
                    .style(secondary_button)
                    .on_press(Message::RefreshResults),
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
                            "{} · {} · {} groups · {} recoverable",
                            scan.date,
                            scan_status_label(scan.status),
                            scan.groups,
                            bytes_label(scan.bytes)
                        ))
                        .size(13)
                    ]
                    .width(Length::Fill),
                    button("Open results")
                        .style(secondary_button)
                        .on_press_maybe(
                            (scan.status == ScanStatus::Completed)
                                .then_some(Message::Reopen(scan.id))
                        )
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
fn scan_status_label(status: ScanStatus) -> &'static str {
    match status {
        ScanStatus::Running => "Running",
        ScanStatus::Completed => "Completed",
        ScanStatus::Failed => "Failed",
        ScanStatus::Cancelled => "Cancelled",
    }
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
            latest_result: None,
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
            scan_mode: Some(ScanMode::Initial),
            next_cleanup_run: 0,
            active_cleanup_run: None,
            latest_result: None,
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
            scan_mode: Some(ScanMode::Initial),
            next_cleanup_run: 0,
            active_cleanup_run: None,
            latest_result: None,
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
        assert!(app.latest_result.is_none());
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
            latest_result: None,
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
            latest_result: None,
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
        assert!(
            matches!(&app.screen, Screen::Results(results) if results.groups == previous.groups)
        );
    }

    #[test]
    fn cancelled_refresh_restores_results_and_keeps_completed_history() {
        let previous = ScanResults {
            groups: vec![duplicate_group(1, &["/a", "/b"], &[1])],
            page: 0,
            scan_name: "Saved scan".into(),
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
            latest_result: None,
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
        assert!(
            matches!(&app.screen, Screen::Results(results) if results.groups == previous.groups)
        );
    }
}
