use iced::widget::{button, container};
use iced::{Background, Border, Color, Shadow, Theme, Vector};

pub(super) const BG: Color = Color {
    r: 0.063,
    g: 0.078,
    b: 0.094,
    a: 1.0,
};
pub(super) const SURFACE: Color = Color {
    r: 0.094,
    g: 0.129,
    b: 0.176,
    a: 1.0,
};
pub(super) const RAISED: Color = Color {
    r: 0.125,
    g: 0.169,
    b: 0.220,
    a: 1.0,
};
pub(super) const BORDER: Color = Color {
    r: 0.173,
    g: 0.227,
    b: 0.290,
    a: 1.0,
};
pub(super) const TEXT: Color = Color {
    r: 0.929,
    g: 0.949,
    b: 0.969,
    a: 1.0,
};
pub(super) const MUTED: Color = Color {
    r: 0.576,
    g: 0.643,
    b: 0.722,
    a: 1.0,
};
pub(super) const BLUE: Color = Color {
    r: 0.310,
    g: 0.549,
    b: 1.0,
    a: 1.0,
};
pub(super) const BLUE_HOVER: Color = Color {
    r: 0.439,
    g: 0.639,
    b: 1.0,
    a: 1.0,
};
pub(super) const DANGER: Color = Color {
    r: 0.875,
    g: 0.396,
    b: 0.439,
    a: 1.0,
};
pub(super) const SUCCESS: Color = Color {
    r: 0.200,
    g: 0.710,
    b: 0.553,
    a: 1.0,
};

pub(super) fn card_style(_: &Theme) -> container::Style {
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
pub(super) fn raised_style(_: &Theme) -> container::Style {
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
pub(super) fn alert_style(_: &Theme) -> container::Style {
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
pub(super) fn primary_button(_: &Theme, status: button::Status) -> button::Style {
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
pub(super) fn secondary_button(_: &Theme, status: button::Status) -> button::Style {
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
pub(super) fn danger_button(_: &Theme, status: button::Status) -> button::Style {
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
pub(super) fn row_button(selected: bool) -> impl Fn(&Theme, button::Status) -> button::Style {
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
