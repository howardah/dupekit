use crate::styles::*;
use crate::views_results::results;
use crate::*;
use iced::widget::{
    Space, button, checkbox, column, container, progress_bar, row, stack, text, text_input,
};
use iced::{Background, Color, Element, Length, alignment};

pub(super) fn header(app: &App) -> Element<'static, Message> {
    let available = !app.scan_worker_active() && app.active_cleanup_run.is_none();
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
pub(super) fn cancelling() -> Element<'static, Message> {
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
pub(super) fn home(app: &App) -> Element<'_, Message> {
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
pub(super) fn scanning(progress: &ScanProgress) -> Element<'static, Message> {
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
pub(super) fn refreshing<'a>(
    previous: &'a ScanResults,
    progress: &ScanProgress,
) -> Element<'a, Message> {
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
            0.15 + progress.pulse * 0.70,
            "Working — this phase does not report a total".into(),
        ),
    };
    let popover = container(
        column![
            text("Refreshing results").size(24),
            text(progress.phase.clone()).size(16),
            text(detail).size(13).color(MUTED),
            progress_bar(0.0..=1.0, value),
            row![
                Space::with_width(Length::Fill),
                button("Cancel refresh")
                    .style(secondary_button)
                    .on_press(Message::CancelScan)
            ]
        ]
        .spacing(12)
        .padding(24),
    )
    .max_width(520)
    .style(raised_style);
    let overlay = container(popover)
        .width(Length::Fill)
        .height(Length::Fill)
        .center_x(Length::Fill)
        .center_y(Length::Fill)
        .style(|_| container::Style {
            background: Some(Background::Color(Color::from_rgba8(8, 12, 20, 0.72))),
            ..container::Style::default()
        });
    stack![results(previous), overlay].into()
}

pub(super) fn cleaning(progress: &CleanupProgress) -> Element<'static, Message> {
    let action = if progress.action == CleanupAction::Trash {
        "Moving files to Trash"
    } else {
        "Permanently deleting files"
    };
    let fraction = if progress.total == 0 {
        0.0
    } else {
        progress.processed as f32 / progress.total as f32
    };
    let current = progress
        .current
        .as_ref()
        .map(|path| path.display().to_string())
        .unwrap_or_else(|| "Checking that selected files have not changed…".into());
    container(
        column![
            text(action).size(30),
            text(if progress.phase == CleanupProgressPhase::Checking {
                format!(
                    "Preparing {} of {} files",
                    progress.processed, progress.total
                )
            } else {
                format!(
                    "{} of {} files processed",
                    progress.processed, progress.total
                )
            }),
            progress_bar(0.0..=1.0, fraction),
            text(current).size(13).color(MUTED),
            text(if progress.phase == CleanupProgressPhase::Checking {
                "Checking selected files before moving them together."
            } else {
                "The selected files are being moved to the system Trash."
            })
            .size(13)
            .color(MUTED)
        ]
        .spacing(14)
        .padding(28),
    )
    .style(raised_style)
    .max_width(680)
    .center_x(Length::Fill)
    .center_y(Length::Fill)
    .into()
}
