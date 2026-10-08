//! Ratatui screens. The shell draws an offline preview until a session exists.

mod chart;
mod chrome;
mod dashboard;
mod format;
mod layout;
mod library;
pub(crate) mod pointer;
mod quality;
pub(crate) mod scroll;
mod theme;
mod views;

use ratatui::Frame;
use ratatui::layout::{Constraint, Layout};

use crate::app::App;
use crate::ui::chrome::{draw_header, draw_help, draw_hints, draw_log, draw_tabs, draw_too_small};
use crate::ui::theme::theme;

pub use chrome::tab_at;
pub use pointer::Pointer;

pub fn draw(frame: &mut Frame<'_>, app: &App, pointer: &mut Pointer) {
    pointer.clear();
    let area = frame.area();
    frame.buffer_mut().set_style(area, theme().base);
    if area.width < 70 || area.height < 18 {
        draw_too_small(frame, area);
        return;
    }
    let library = !app.signing_in() && app.view() == crate::app::View::Library;
    let log_height = if library {
        library::TRANSPORT_ROWS.min(area.height.saturating_sub(3))
    } else {
        log_rows(app.log_open(), area.height)
    };
    let chunks = Layout::vertical([
        Constraint::Length(1),
        Constraint::Min(0),
        Constraint::Length(log_height),
        Constraint::Length(1),
        Constraint::Length(1),
    ])
    .split(area);
    draw_header(frame, chunks[0], app);
    if app.signing_in() {
        views::draw_sign_in(frame, chunks[1], app, pointer);
        frame.render_widget(
            ratatui::widgets::Paragraph::new(" your Soulseek name  ·  this is not a preview"),
            chunks[3],
        );
    } else {
        views::draw(frame, chunks[1], app, pointer);
        draw_tabs(frame, chunks[3], app, pointer);
    }
    if library {
        library::draw_transport(frame, chunks[2], app, pointer);
    } else {
        draw_log(frame, chunks[2], app, pointer);
    }
    draw_hints(frame, chunks[4], app, pointer);
    if app.picker_open() {
        views::draw_folder_picker(frame, area, app, pointer);
    }
    if app.bind_picker_open() {
        views::draw_bind_picker(frame, area, app, pointer);
    }
    if app.delete_prompt().is_some() {
        library::draw_delete(frame, area, app, pointer);
    }
    if app.person_open() {
        views::draw_person(frame, area, app, pointer);
    }
    if app.help() {
        draw_help(frame, area, app.signing_in());
    }
}

fn log_rows(open: bool, height: u16) -> u16 {
    let room = height.saturating_sub(3);
    if room == 0 {
        return 0;
    }
    if !open {
        return 1;
    }
    let max = room.saturating_sub(8).max(1);
    7.min(max)
}

pub fn screen_text(
    frame_width: u16,
    frame_height: u16,
    buffer: &ratatui::buffer::Buffer,
) -> String {
    let mut text = String::new();
    for y in 0..frame_height {
        for x in 0..frame_width {
            text.push_str(buffer[(x, y)].symbol());
        }
        text.push('\n');
    }
    text
}
