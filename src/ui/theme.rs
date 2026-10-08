use ratatui::style::{Color, Modifier, Style};

use crate::model::TransferState;
use crate::protocol::UserStatus;

#[derive(Clone, Copy)]
pub struct Theme {
    pub base: Style,
    pub text: Style,
    pub muted: Style,
    pub title: Style,
    pub brand: Style,
    pub warn: Style,
    pub border: Style,
    pub header: Style,
    pub selected: Style,
    pub zebra: Style,
    pub down: Style,
    pub up: Style,
}

pub fn theme() -> Theme {
    let bg = Color::Rgb(0, 0, 0);
    let text = Color::Rgb(196, 204, 196);
    let green = Color::Rgb(94, 214, 120);
    let green_dim = Color::Rgb(70, 110, 78);
    let amber = Color::Rgb(232, 184, 74);
    let blue = Color::Rgb(88, 170, 255);
    Theme {
        base: Style::default().fg(text).bg(bg),
        text: Style::default().fg(text),
        muted: Style::default().fg(Color::Rgb(110, 120, 110)),
        title: Style::default().fg(green).add_modifier(Modifier::BOLD),
        brand: Style::default().fg(green).add_modifier(Modifier::BOLD),
        warn: Style::default().fg(amber).add_modifier(Modifier::BOLD),
        border: Style::default().fg(green_dim),
        header: Style::default()
            .fg(Color::Rgb(150, 170, 150))
            .add_modifier(Modifier::BOLD),
        selected: Style::default()
            .fg(Color::Rgb(232, 255, 232))
            .bg(Color::Rgb(18, 58, 32))
            .add_modifier(Modifier::BOLD),
        zebra: Style::default().bg(Color::Rgb(8, 12, 8)),
        down: Style::default().fg(green),
        up: Style::default().fg(blue),
    }
}

pub fn transfer_style(state: TransferState) -> Style {
    let color = match state {
        TransferState::Transferring | TransferState::Finished => Color::Rgb(94, 214, 120),
        TransferState::Queued | TransferState::Paused => Color::Rgb(232, 184, 74),
        TransferState::Cancelled => Color::Rgb(150, 150, 150),
        TransferState::Filtered
        | TransferState::Banned
        | TransferState::TooManyFiles
        | TransferState::TooManyMegabytes
        | TransferState::InternalError
        | TransferState::LastTryFailed
        | TransferState::PendingShutdown
        | TransferState::Refused
        | TransferState::UserLoggedOff
        | TransferState::ConnectionClosed
        | TransferState::ConnectionTimeout
        | TransferState::FileNotShared
        | TransferState::Failed => Color::Rgb(230, 90, 90),
    };
    Style::default().fg(color)
}

pub fn status_style(status: UserStatus) -> Style {
    let color = match status {
        UserStatus::Online => Color::Rgb(94, 214, 120),
        UserStatus::Away => Color::Rgb(232, 184, 74),
        UserStatus::Offline => Color::Rgb(230, 90, 90),
    };
    Style::default().fg(color)
}
