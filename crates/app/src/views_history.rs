use crate::styles::*;
use crate::*;
use dupekit_core::DuplicateFile;
use iced::widget::{button, column, container, row, text};
use iced::{Element, Length, alignment};

pub(super) fn history(app: &App) -> Element<'_, Message> {
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
pub(super) fn scan_status_label(status: ScanStatus) -> &'static str {
    match status {
        ScanStatus::Running => "Running",
        ScanStatus::Completed => "Completed",
        ScanStatus::Failed => "Failed",
        ScanStatus::Cancelled => "Cancelled",
    }
}
pub(super) fn bytes_label(bytes: u64) -> String {
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
pub(super) fn modified_label(file: &DuplicateFile) -> String {
    file.modified
        .and_then(|t| t.duration_since(std::time::SystemTime::UNIX_EPOCH).ok())
        .map(|d| format!("Unix + {} days", d.as_secs() / 86_400))
        .unwrap_or_else(|| "unknown date".into())
}

// A small, deliberately restrained visual system. Keeping it local means the
// application can remain a single native binary without a web-style asset layer.
