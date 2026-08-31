use crate::styles::*;
use crate::utilities::totals;
use crate::views_history::{bytes_label, modified_label};
use crate::*;
use iced::widget::{
    Space, button, checkbox, column, container, pick_list, row, scrollable, text, text_input,
};
use iced::{Element, Length, alignment};

pub(super) fn results(results: &ScanResults) -> Element<'_, Message> {
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
    let header = row![
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
    ];
    let toolbar = container(
        column![
            row![
                text(format!(
                    "{} groups · {} files",
                    results.groups.len(),
                    results.groups.iter().map(|g| g.files.len()).sum::<usize>()
                ))
                .size(14)
                .color(MUTED),
                Space::with_width(Length::Fill),
                text("Automatic selection").size(13).color(MUTED),
                pick_list(&UiPolicy::ALL[..], None::<UiPolicy>, Message::ApplyPolicy)
                    .placeholder("Select duplicates")
            ]
            .align_y(alignment::Vertical::Center)
            .spacing(12),
            row![
                text("Refresh settings for this scan").size(13).color(MUTED),
                Space::with_width(Length::Fill),
                text(results.refresh_settings.summary())
                    .size(12)
                    .color(MUTED),
                button(if results.refresh_settings.expanded {
                    "Hide refresh settings"
                } else {
                    "Refresh…"
                })
                .style(secondary_button)
                .on_press(Message::ToggleRefreshSettings)
            ]
            .align_y(alignment::Vertical::Center)
            .spacing(12)
        ]
        .spacing(10),
    )
    .padding(12)
    .style(card_style);
    let mut body = column![header, toolbar].spacing(14);
    if results.refresh_settings.expanded {
        let mut settings = column![
            text("Refresh settings").size(18),
            text("These settings belong to this scan and will be saved after a successful refresh.")
                .size(13)
                .color(MUTED),
            text("Changes take effect only when you refresh.")
                .size(13)
                .color(MUTED),
            row![
                column![
                    text("Minimum file size").size(13).color(MUTED),
                    text_input("No minimum", &results.refresh_settings.min_size)
                        .on_input(Message::RefreshMinSize)
                        .padding(10)
                ]
                .width(Length::Fill),
                column![
                    text("Maximum file size (optional)").size(13).color(MUTED),
                    text_input("No limit", &results.refresh_settings.max_size)
                        .on_input(Message::RefreshMaxSize)
                        .padding(10)
                ]
                .width(Length::Fill)
            ]
            .spacing(22),
            checkbox("Use fclones hash cache", results.refresh_settings.cache)
                .on_toggle(Message::ToggleRefreshCache),
            text("The cache can make refreshes faster; it does not change which files count as duplicates.")
                .size(13)
                .color(MUTED)
        ]
        .spacing(10);
        if results.refresh_settings.legacy_unrecorded {
            settings = settings.push(
                text("Original settings were not recorded for this older scan. Review these defaults before refreshing.")
                    .size(13)
                    .color(DANGER),
            );
        }
        if let Some(error) = &results.refresh_settings.error {
            settings = settings.push(
                container(text(error).color(DANGER))
                    .padding(10)
                    .width(Length::Fill)
                    .style(alert_style),
            );
        }
        settings = settings.push(
            row![
                Space::with_width(Length::Fill),
                button("Close")
                    .style(secondary_button)
                    .on_press(Message::ToggleRefreshSettings),
                button("Refresh with these settings")
                    .style(primary_button)
                    .on_press(Message::RefreshResults)
            ]
            .spacing(10),
        );
        body = body.push(container(settings.padding(18)).style(card_style));
    }
    body = body
        .push(scrollable(groups).height(Length::Fill))
        .push(row![
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
        ])
        .push(
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
                .spacing(10),
            )
            .padding(14)
            .style(raised_style),
        );
    body.height(Length::Fill).into()
}
pub(super) fn group_view(g: &DuplicateGroup) -> Element<'_, Message> {
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
pub(super) fn confirmation(
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
pub(super) fn cleanup_done(permanent: bool, count: usize, bytes: u64) -> Element<'static, Message> {
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
