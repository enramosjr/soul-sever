use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;

use crate::app::{App, ScrollId, Target, TransportButton};
use crate::ui::chart::spectrum_lines;
use crate::ui::chrome::bordered;
use crate::ui::layout::{horizontal, vertical};
use crate::ui::pointer::Pointer;
use crate::ui::scroll;
use crate::ui::theme::theme;

pub fn draw(frame: &mut Frame, area: Rect, app: &App, pointer: &mut Pointer) {
    let meter = 11.min(area.height.saturating_sub(8)).max(4);
    let meter = meter.min(area.height.saturating_sub(1));
    let rows = vertical(area, &[meter, area.height.saturating_sub(meter)]);
    let info_width = (rows[0].width / 3).clamp(24, 42);
    let meter_width = rows[0].width.saturating_sub(info_width);
    let top = horizontal(rows[0], &[meter_width, info_width]);
    draw_meter(frame, top[0], app);
    draw_info(frame, top[1], app);
    let third = rows[1].width / 3;
    let columns = horizontal(rows[1], &[third, third, third]);
    let artists = app.artist_names();
    let albums = app.album_names();
    let songs = app.song_labels();
    draw_column(
        frame,
        columns[0],
        app,
        pointer,
        Column {
            title: "artists",
            rows: &artists,
            active: app.library_column() == 0,
            cursor: app.library_index(0),
            column: 0,
        },
    );
    draw_column(
        frame,
        columns[1],
        app,
        pointer,
        Column {
            title: "albums",
            rows: &albums,
            active: app.library_column() == 1,
            cursor: app.library_index(1),
            column: 1,
        },
    );
    draw_column(
        frame,
        columns[2],
        app,
        pointer,
        Column {
            title: "songs",
            rows: &songs,
            active: app.library_column() == 2,
            cursor: app.library_index(2),
            column: 2,
        },
    );
}

struct Column<'a> {
    title: &'a str,
    rows: &'a [String],
    active: bool,
    cursor: usize,
    column: u8,
}

fn draw_meter(frame: &mut Frame, area: Rect, app: &App) {
    let block = bordered("playback");
    let inner = block.inner(area);
    frame.render_widget(block, area);
    if inner.width == 0 || inner.height == 0 {
        return;
    }
    let lines = spectrum_lines(app.spectrum().as_slice(), inner.width, inner.height);
    frame.render_widget(Paragraph::new(lines), inner);
}

fn draw_info(frame: &mut Frame, area: Rect, app: &App) {
    let block = bordered("track");
    let inner = block.inner(area);
    frame.render_widget(block, area);
    if inner.width == 0 || inner.height == 0 {
        return;
    }
    let palette = theme();
    let lines: Vec<Line> = app
        .track_rows()
        .into_iter()
        .map(|row| Line::from(Span::styled(row, palette.text)))
        .collect();
    frame.render_widget(Paragraph::new(lines), inner);
}

/// Seek row plus one row of icon-sized play, pause, and stop marks.
pub const TRANSPORT_ROWS: u16 = 2;

const PLAY: &str = "▶";
const PAUSE: &str = "❚❚";
const STOP: &str = "■";

pub fn draw_transport(frame: &mut Frame, area: Rect, app: &App, pointer: &mut Pointer) {
    if area.height == 0 || area.width == 0 {
        return;
    }
    let palette = theme();
    let (position, length, fraction) = app.transport();
    let chrome = position.len() + length.len() + 3;
    let bar_width = usize::from(area.width).saturating_sub(chrome);
    let bar = seek_bar(bar_width, fraction);
    let line = format!(" {position} {bar} {length}");
    let seek_row = Rect {
        x: area.x,
        y: area.y,
        width: area.width,
        height: 1,
    };
    frame.render_widget(Paragraph::new(line).style(palette.text), seek_row);
    if bar_width > 0 {
        pointer.seek(Rect {
            x: area
                .x
                .saturating_add(u16::try_from(position.len() + 2).unwrap_or(0)),
            y: area.y,
            width: u16::try_from(bar_width).unwrap_or(0),
            height: 1,
        });
    }
    if area.height < 2 {
        return;
    }
    let lit = app.transport_button();
    let shapes = [
        (PLAY, Target::Play, TransportButton::Play),
        (PAUSE, Target::Pause, TransportButton::Pause),
        (STOP, Target::Stop, TransportButton::Stop),
    ];
    let mark_width: u16 = shapes
        .iter()
        .enumerate()
        .map(|(index, (glyph, _, _))| {
            let gap = if index == 0 { 0 } else { 2 };
            gap + u16::try_from(glyph.chars().count()).unwrap_or(0)
        })
        .sum();
    let origin = area
        .x
        .saturating_add(area.width.saturating_sub(mark_width) / 2);
    let mut x = origin;
    let mut spans = Vec::new();
    for (index, (glyph, target, button)) in shapes.iter().enumerate() {
        if index > 0 {
            spans.push(Span::styled("  ", palette.text));
            x = x.saturating_add(2);
        }
        let width = u16::try_from(glyph.chars().count()).unwrap_or(0);
        if width > 0 && area.height >= 2 {
            pointer.control(
                Rect {
                    x,
                    y: area.y.saturating_add(1),
                    width: width.min(area.x.saturating_add(area.width).saturating_sub(x)),
                    height: 1,
                },
                *target,
            );
        }
        let style = if *button == lit {
            palette.title
        } else {
            palette.text
        };
        spans.push(Span::styled((*glyph).to_owned(), style));
        x = x.saturating_add(width);
    }
    let controls = Rect {
        x: origin,
        y: area.y.saturating_add(1),
        width: mark_width.min(area.x.saturating_add(area.width).saturating_sub(origin)),
        height: 1,
    };
    frame.render_widget(Paragraph::new(Line::from(spans)), controls);
}

fn seek_bar(width: usize, fraction: f64) -> String {
    if width == 0 {
        return String::new();
    }
    let head = ((width - 1) as f64 * fraction.clamp(0.0, 1.0)).round() as usize;
    let mut bar = String::new();
    for index in 0..width {
        if index == head {
            bar.push('●');
        } else if index < head {
            bar.push('━');
        } else {
            bar.push('─');
        }
    }
    bar
}

fn draw_column(
    frame: &mut Frame,
    area: Rect,
    app: &App,
    pointer: &mut Pointer,
    column: Column<'_>,
) {
    let Column {
        title,
        rows,
        active,
        cursor,
        column,
    } = column;
    let block = bordered(title);
    let inner = block.inner(area);
    frame.render_widget(block, area);
    if inner.height == 0 {
        return;
    }
    let palette = theme();
    if rows.is_empty() {
        frame.render_widget(
            Paragraph::new(Line::from(Span::styled(" nothing here", palette.muted))),
            inner,
        );
        return;
    }
    let height = usize::from(inner.height);
    let start = app.window_origin(ScrollId::Library(column), rows.len(), height);
    let end = (start + height).min(rows.len());
    pointer.wheel(inner, ScrollId::Library(column));
    for (offset, row) in rows[start..end].iter().enumerate() {
        let index = start + offset;
        let selected = index == cursor;
        let mark = if selected { "▸ " } else { "  " };
        let style = if selected && active {
            palette.selected
        } else if selected {
            palette.title
        } else {
            palette.text
        };
        let line = Rect {
            x: inner.x,
            y: inner.y + u16::try_from(offset).unwrap_or(0),
            width: inner.width,
            height: 1,
        };
        pointer.column(line, column, index);
        frame.render_widget(Paragraph::new(format!("{mark}{row}")).style(style), line);
    }
    scroll::paint(
        frame,
        scroll::padding_track(area, inner.y, inner.height),
        rows.len(),
        height,
        start,
        pointer,
        ScrollId::Library(column),
    );
}

pub fn draw_delete(frame: &mut Frame, area: Rect, app: &App, pointer: &mut Pointer) {
    let Some((heading, detail)) = app.delete_prompt() else {
        return;
    };
    let palette = theme();
    let width = heading
        .chars()
        .count()
        .max(detail.chars().count())
        .max(28)
        .saturating_add(4);
    let width = u16::try_from(width).unwrap_or(u16::MAX).min(area.width);
    let height = 7.min(area.height);
    let popup = Rect {
        x: area.x + area.width.saturating_sub(width) / 2,
        y: area.y + area.height.saturating_sub(height) / 2,
        width,
        height,
    };
    pointer.picker_stay(popup);
    let block = bordered("delete");
    let inner = block.inner(popup);
    frame.render_widget(block, popup);
    if inner.width == 0 || inner.height < 4 {
        return;
    }
    let lines = vec![
        Line::from(Span::styled(heading, palette.title)),
        Line::from(Span::styled(detail, palette.text)),
        Line::raw(""),
        Line::from(Span::styled(
            "enter removes it    esc cancels",
            palette.muted,
        )),
    ];
    let text = Rect {
        x: inner.x,
        y: inner.y,
        width: inner.width,
        height: inner.height.saturating_sub(1),
    };
    frame.render_widget(Paragraph::new(lines), text);
    let buttons = Rect {
        x: inner.x,
        y: inner.y + inner.height.saturating_sub(1),
        width: inner.width,
        height: 1,
    };
    let remove = Rect {
        x: buttons.x,
        y: buttons.y,
        width: 8.min(buttons.width),
        height: 1,
    };
    let cancel_x = buttons.x + 10.min(buttons.width.saturating_sub(1));
    let cancel = Rect {
        x: cancel_x,
        y: buttons.y,
        width: 6.min(
            buttons
                .width
                .saturating_sub(cancel_x.saturating_sub(buttons.x)),
        ),
        height: 1,
    };
    pointer.control(remove, Target::Confirm(true));
    pointer.control(cancel, Target::Confirm(false));
    frame.render_widget(
        Paragraph::new(Line::from(vec![
            Span::styled(" remove ", palette.title),
            Span::styled("  cancel", palette.muted),
        ])),
        buttons,
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_transport_shapes_are_a_triangle_two_bars_and_a_square() {
        assert_eq!(PLAY, "▶");
        assert_eq!(PLAY.chars().count(), 1);
        assert_eq!(PAUSE.chars().filter(|ch| *ch == '❚').count(), 2);
        assert_eq!(PAUSE.chars().count(), 2);
        assert_eq!(STOP, "■");
        assert_eq!(STOP.chars().count(), 1);
    }
}
