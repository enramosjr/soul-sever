use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;

use crate::app::{App, ScrollId};
use crate::ui::chrome::bordered;
use crate::ui::layout::{horizontal, vertical};
use crate::ui::pointer::Pointer;
use crate::ui::scroll;
use crate::ui::theme::theme;

pub fn draw(frame: &mut Frame, area: Rect, app: &App, pointer: &mut Pointer) {
    let head = 8.min(area.height.saturating_sub(6)).max(4);
    let rows = vertical(area, &[head, area.height.saturating_sub(head)]);
    draw_detail(frame, rows[0], app);
    let half = rows[1].width / 2;
    let columns = horizontal(rows[1], &[half, rows[1].width.saturating_sub(half)]);
    for (column, title) in ["measuring", "replacing"].into_iter().enumerate() {
        let column = u8::try_from(column).unwrap_or(0);
        draw_column(
            frame,
            columns[usize::from(column)],
            app,
            pointer,
            Column {
                title,
                active: app.quality_column() == column,
                cursor: app.quality_index(column),
                column,
            },
        );
    }
}

fn draw_detail(frame: &mut Frame, area: Rect, app: &App) {
    let block = bordered("quality");
    let inner = block.inner(area);
    frame.render_widget(block, area);
    if inner.width == 0 || inner.height == 0 {
        return;
    }
    let palette = theme();
    let lines: Vec<Line> = app
        .quality_detail()
        .into_iter()
        .map(|row| Line::from(Span::styled(row, palette.text)))
        .collect();
    frame.render_widget(Paragraph::new(lines), inner);
}

struct Column<'a> {
    title: &'a str,
    active: bool,
    cursor: usize,
    column: u8,
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
        active,
        cursor,
        column,
    } = column;
    let len = app.quality_len(column);
    let block = bordered(title);
    let inner = block.inner(area);
    frame.render_widget(block, area);
    if inner.height == 0 {
        return;
    }
    let palette = theme();
    if len == 0 {
        frame.render_widget(
            Paragraph::new(Line::from(Span::styled(" idle", palette.muted))),
            inner,
        );
        return;
    }
    let height = usize::from(inner.height);
    let start = app.window_origin(ScrollId::Quality(column), len, height);
    let end = (start + height).min(len);
    pointer.wheel(inner, ScrollId::Quality(column));
    for offset in 0..(end - start) {
        let index = start + offset;
        let row = app.quality_label(column, index);
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
        pointer.quality(line, column, index);
        frame.render_widget(Paragraph::new(format!("{mark}{row}")).style(style), line);
    }
    scroll::paint(
        frame,
        scroll::padding_track(area, inner.y, inner.height),
        len,
        height,
        start,
        pointer,
        ScrollId::Quality(column),
    );
}
