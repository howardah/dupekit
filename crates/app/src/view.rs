use crate::styles::*;
use crate::views_history::history;
use crate::views_home::{cancelling, cleaning, header, home, refreshing, scanning};
use crate::views_results::{cleanup_done, confirmation, results};
use crate::*;
use iced::widget::{button, column, container, row, text};
use iced::{Background, Element, Length};

pub(super) fn view(app: &App) -> Element<'_, Message> {
    let body = match &app.screen {
        Screen::Home => home(app),
        Screen::Scanning(progress) => match &app.scan_mode {
            Some(ScanMode::Refresh { previous, .. }) => refreshing(previous, progress),
            _ => scanning(progress),
        },
        Screen::Cancelling => cancelling(),
        Screen::Results(r) => results(r),
        Screen::Confirm {
            permanent,
            count,
            bytes,
            acknowledged,
        } => confirmation(*permanent, *count, *bytes, *acknowledged),
        Screen::Cleaning(progress) => cleaning(progress),
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
