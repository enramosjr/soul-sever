use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::Style;
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;

use crate::app::{App, Feed, PickerKind, ScrollId, Target, View};
use crate::model::{Direction, SearchLine, Transfer};
use crate::panels::{Edge, Split};
use crate::ui::chart::{Heat, progress_line};
use crate::ui::chrome::{bordered, draw_table, line_row, paint_rows, row_style};
use crate::ui::format::{format_bytes, format_duration, format_rate};
use crate::ui::layout::{horizontal, vertical};
use crate::ui::pointer::Pointer;
use crate::ui::scroll;
use crate::ui::theme::{status_style, theme, transfer_style};

pub fn draw_sign_in(frame: &mut Frame, area: Rect, app: &App, pointer: &mut Pointer) {
    let palette = theme();
    let user_mark = if app.sign_field() == 0 { "█" } else { "" };
    let pass_mark = if app.sign_field() == 1 { "█" } else { "" };
    let password = "*".repeat(app.sign_password_len());
    let lines = vec![
        Line::from(Span::styled(
            "sign in to the Soulseek network",
            palette.title,
        )),
        Line::from(Span::styled(
            "A name that is not already registered",
            palette.title,
        )),
        Line::from(Span::styled(
            "creates a new Soulseek account.",
            palette.title,
        )),
        Line::from(Span::styled(
            "A name that already exists signs in with that account's password.",
            palette.text,
        )),
        Line::raw(""),
        Line::from(Span::styled(
            format!("server    {}", app.server_label()),
            palette.muted,
        )),
        Line::raw(""),
        Line::from(Span::styled("username", palette.muted)),
        Line::from(Span::styled(
            format!("{}{user_mark}", app.sign_username()),
            palette.brand,
        )),
        Line::raw(""),
        Line::from(Span::styled("password", palette.muted)),
        Line::from(Span::styled(
            format!("{password}{pass_mark}"),
            palette.brand,
        )),
        Line::raw(""),
        Line::from(Span::styled(
            "the password is saved in the system keyring",
            palette.muted,
        )),
    ];
    frame.render_widget(Paragraph::new(lines).block(bordered("sign in")), area);
    let username_row = Rect {
        x: area.x,
        y: area.y.saturating_add(9),
        width: area.width,
        height: 1,
    };
    let password_row = Rect {
        x: area.x,
        y: area.y.saturating_add(12),
        width: area.width,
        height: 1,
    };
    pointer.select(username_row, 0);
    pointer.select(password_row, 1);
}

pub fn draw(frame: &mut Frame, area: Rect, app: &App, pointer: &mut Pointer) {
    pointer.scroll(area);
    match app.view() {
        View::Dashboard => crate::ui::dashboard::draw(frame, area, app, pointer),
        View::Search => draw_search(frame, area, app, pointer),
        View::Downloads => draw_transfers(frame, area, app, Direction::Download, pointer),
        View::Uploads => draw_transfers(frame, area, app, Direction::Upload, pointer),
        View::Browse => draw_browse(frame, area, app, pointer),
        View::Chat => draw_chat(frame, area, app, pointer),
        View::Users => draw_users(frame, area, app, pointer),
        View::Shares => draw_shares(frame, area, app, pointer),
        View::Settings => draw_settings(frame, area, app, pointer),
        View::Library => crate::ui::library::draw(frame, area, app, pointer),
        View::Quality => crate::ui::quality::draw(frame, area, app, pointer),
    }
}

pub fn draw_folder_picker(frame: &mut Frame, area: Rect, app: &App, pointer: &mut Pointer) {
    let width = 64.min(area.width);
    let height = 18.min(area.height);
    let popup = Rect {
        x: area.x + area.width.saturating_sub(width) / 2,
        y: area.y + area.height.saturating_sub(height) / 2,
        width,
        height,
    };
    pointer.picker_stay(popup);
    let block = bordered("folder");
    let inner = block.inner(popup);
    frame.render_widget(block, popup);
    let parts = Layout::vertical([
        Constraint::Length(1),
        Constraint::Min(0),
        Constraint::Length(1),
    ])
    .split(inner);
    let palette = theme();
    frame.render_widget(
        Paragraph::new(app.picker_path()).style(palette.title),
        parts[0],
    );
    let rows = app.picker_rows();
    let cursor = app.picker_cursor();
    let list_height = usize::from(parts[1].height);
    let (start, end) = picker_window(
        app.window_origin(ScrollId::Picker, rows.len(), list_height),
        cursor,
        rows.len(),
        list_height,
    );
    for (offset, row) in rows[start..end].iter().enumerate() {
        let absolute = start + offset;
        let selected = absolute == cursor;
        let mark = if selected { "▸ " } else { "  " };
        let style = if selected {
            palette.selected
        } else if row.kind == PickerKind::File {
            palette.muted
        } else {
            palette.text
        };
        let line = Rect {
            x: parts[1].x,
            y: parts[1].y + u16::try_from(offset).unwrap_or(0),
            width: parts[1].width,
            height: 1,
        };
        pointer.picker(line, absolute);
        frame.render_widget(
            Paragraph::new(format!("{mark}{}", row.label)).style(style),
            line,
        );
    }
    frame.render_widget(
        Paragraph::new(if app.folder_field_open() {
            "enter opens   s uses this folder   esc closes"
        } else {
            "enter opens   s shares this folder   esc closes"
        })
        .style(palette.muted),
        parts[2],
    );
    scroll::paint(
        frame,
        scroll::padding_track(popup, parts[1].y, parts[1].height),
        rows.len(),
        list_height,
        start,
        pointer,
        ScrollId::Picker,
    );
}

pub fn draw_bind_picker(frame: &mut Frame, area: Rect, app: &App, pointer: &mut Pointer) {
    let width = 72.min(area.width);
    let height = 18.min(area.height);
    let popup = Rect {
        x: area.x + area.width.saturating_sub(width) / 2,
        y: area.y + area.height.saturating_sub(height) / 2,
        width,
        height,
    };
    pointer.picker_stay(popup);
    let block = bordered("bind address");
    let inner = block.inner(popup);
    frame.render_widget(block, popup);
    let parts = Layout::vertical([Constraint::Min(0), Constraint::Length(1)]).split(inner);
    let palette = theme();
    let rows = app.bind_rows();
    let cursor = app.bind_cursor();
    let list_height = usize::from(parts[0].height);
    let (start, end) = picker_window(
        app.window_origin(ScrollId::Bind, rows.len(), list_height),
        cursor,
        rows.len(),
        list_height,
    );
    for (offset, label) in rows[start..end].iter().enumerate() {
        let absolute = start + offset;
        let selected = absolute == cursor;
        let mark = if selected { "▸ " } else { "  " };
        let style = if selected {
            palette.selected
        } else {
            palette.text
        };
        let line = Rect {
            x: parts[0].x,
            y: parts[0].y + u16::try_from(offset).unwrap_or(0),
            width: parts[0].width,
            height: 1,
        };
        pointer.picker(line, absolute);
        frame.render_widget(Paragraph::new(format!("{mark}{label}")).style(style), line);
    }
    frame.render_widget(
        Paragraph::new("enter selects   esc closes").style(palette.muted),
        parts[1],
    );
    scroll::paint(
        frame,
        scroll::padding_track(popup, parts[0].y, parts[0].height),
        rows.len(),
        list_height,
        start,
        pointer,
        ScrollId::Bind,
    );
}

fn picker_window(origin: usize, cursor: usize, len: usize, height: usize) -> (usize, usize) {
    let start = scroll::reveal(origin, cursor, len, height);
    if height == 0 || len == 0 {
        return (0, 0);
    }
    (start, (start + height).min(len))
}

fn draw_search(frame: &mut Frame, area: Rect, app: &App, pointer: &mut Pointer) {
    let query_height = app.search_query_height(area.height);
    let rows = vertical(
        area,
        &[query_height, area.height.saturating_sub(query_height)],
    );
    let query = if app.editing() {
        format!("{}█", app.query())
    } else if app.query().is_empty() {
        "type a query, then enter".to_owned()
    } else {
        app.query().to_owned()
    };
    let palette = theme();
    let text = vec![
        Line::from(Span::styled(query, palette.brand)),
        Line::from(vec![
            Span::styled("mode ", palette.muted),
            Span::styled(app.mode().label(), palette.title),
            Span::styled(
                if app.feed() == Feed::Live {
                    "   server search"
                } else {
                    "   sign in to search"
                },
                palette.muted,
            ),
        ]),
    ];
    frame.render_widget(Paragraph::new(text).block(bordered("query")), rows[0]);
    pointer.focus_query(line_rect(rows[0], 1));
    pointer.cycle_mode(line_rect(rows[0], 2));

    let filter_width = app.search_filters_width(rows[1].width);
    let columns = horizontal(
        rows[1],
        &[filter_width, rows[1].width.saturating_sub(filter_width)],
    );
    let mut filters = app.filter_lines();
    if let Some(index) = app.filter_field()
        && let Some(line) = filters.get_mut(usize::from(index))
    {
        line.push('█');
    }
    let filters = filters
        .into_iter()
        .map(|line| Line::from(Span::styled(line, palette.text)))
        .collect::<Vec<_>>();
    frame.render_widget(
        Paragraph::new(filters).block(bordered("filters")),
        columns[0],
    );
    for index in 0..9 {
        pointer.cycle_filter(
            line_rect(columns[0], 1 + u16::try_from(index).unwrap_or(0)),
            u8::try_from(index).unwrap_or(0),
        );
    }

    let results_title = format!("results · {}", app.download_cue());
    let body = columns[1].height.saturating_sub(3) as usize;
    let height = body.max(1);
    let (first, table_rows, hits_len) = {
        let hits = app.search_lines();
        let start = app.window_origin(ScrollId::List, hits.len(), height);
        let shown = height.min(hits.len().saturating_sub(start));
        let rows = hits
            .iter()
            .skip(start)
            .take(shown)
            .map(|hit| line_row(search_row(hit)))
            .collect();
        (start, rows, hits.len())
    };
    paint_rows(
        frame,
        columns[1],
        &results_title,
        &["user", "file", "size", "bit", "time", "q", "slot", "cc"],
        &[
            Constraint::Length(14),
            Constraint::Fill(1),
            Constraint::Length(8),
            Constraint::Length(6),
            Constraint::Length(6),
            Constraint::Length(4),
            Constraint::Length(4),
            Constraint::Length(4),
        ],
        table_rows,
        first,
        app.cursor(),
        pointer,
        hits_len,
        height,
    );
    pointer.resize(rows[0], Split::SearchQuery, Edge::South);
    pointer.resize(rows[1], Split::SearchQuery, Edge::North);
    pointer.resize(columns[0], Split::SearchFilters, Edge::East);
    pointer.resize(columns[1], Split::SearchFilters, Edge::West);
}

fn draw_transfers(
    frame: &mut Frame,
    area: Rect,
    app: &App,
    direction: Direction,
    pointer: &mut Pointer,
) {
    let (title, blurb) = match direction {
        Direction::Download => (
            "download queue",
            "queued, transferring, paused, finished   x removes the row   X clears finished"
                .to_owned(),
        ),
        Direction::Upload => (
            "upload queue",
            format!(
                "slots {}   fifo   privileged users go first   x removes the row   X clears finished",
                app.upload_slot_line()
            ),
        ),
    };
    let parts = Layout::vertical([Constraint::Length(3), Constraint::Min(0)]).split(area);
    frame.render_widget(
        Paragraph::new(Line::from(blurb)).block(bordered(if direction == Direction::Download {
            "incoming"
        } else {
            "outgoing"
        })),
        parts[0],
    );
    let mut rows: Vec<_> = app
        .transfers()
        .iter()
        .filter(|transfer| transfer.direction == direction)
        .map(transfer_row)
        .collect();
    rows.extend(
        app.live_transfers()
            .iter()
            .filter(|transfer| transfer.direction == direction)
            .map(transfer_row),
    );
    draw_table(
        frame,
        parts[1],
        title,
        &["user", "file", "progress", "speed", "q", "state"],
        &[
            Constraint::Length(14),
            Constraint::Fill(1),
            Constraint::Length(15),
            Constraint::Length(8),
            Constraint::Length(4),
            Constraint::Length(22),
        ],
        rows,
        app.cursor(),
        app,
        pointer,
    );
}

fn draw_browse(frame: &mut Frame, area: Rect, app: &App, pointer: &mut Pointer) {
    let rows = app
        .browse()
        .iter()
        .map(|row| {
            let indent = "  ".repeat(usize::from(row.depth));
            line_row(vec![
                Span::raw(format!("{indent}{}", row.name)),
                Span::raw(row.size.map(format_bytes).unwrap_or_else(|| "—".to_owned())),
                Span::raw(option_kbps(row.bitrate)),
                Span::raw(
                    row.duration
                        .map(format_duration)
                        .unwrap_or_else(|| "—".to_owned()),
                ),
            ])
        })
        .collect();
    draw_table(
        frame,
        area,
        "browse",
        &["name", "size", "bit", "time"],
        &[
            Constraint::Fill(1),
            Constraint::Length(8),
            Constraint::Length(6),
            Constraint::Length(6),
        ],
        rows,
        app.cursor(),
        app,
        pointer,
    );
}

fn draw_chat(frame: &mut Frame, area: Rect, app: &App, pointer: &mut Pointer) {
    let (rooms_width, text_width, members_width) = app.chat_widths(area.width);
    let columns = horizontal(area, &[rooms_width, text_width, members_width]);
    let rooms = app
        .rooms()
        .iter()
        .map(|room| {
            let mark = if room.joined { "*" } else { " " };
            line_row(vec![Span::raw(format!(
                "{mark} {}  {}",
                room.name, room.users
            ))])
        })
        .collect();
    draw_table(
        frame,
        columns[0],
        "rooms",
        &[if app.feed() == Feed::Live {
            "room"
        } else {
            "preview"
        }],
        &[Constraint::Fill(1)],
        rooms,
        app.cursor(),
        app,
        pointer,
    );

    let selected = app
        .rooms()
        .get(app.cursor())
        .map(|room| room.name.as_str())
        .unwrap_or("no room");
    let palette = theme();
    let block = bordered(&format!("room {selected}"));
    let inner = block.inner(columns[1]);
    frame.render_widget(block, columns[1]);
    let (draft, draft_style) = say_line(app);
    let draft_rows = wrap_spans(vec![Span::raw(draft)], inner.width);
    let wanted = u16::try_from(draft_rows.len().max(1)).unwrap_or(u16::MAX);
    let say_height = match inner.height {
        0 => 0,
        1 => 1,
        height => wanted.min(height.saturating_sub(1)).max(1),
    };
    let transcript = Rect {
        x: inner.x,
        y: inner.y,
        width: inner.width,
        height: inner.height.saturating_sub(say_height),
    };
    let say_row = Rect {
        x: inner.x,
        y: inner.y.saturating_add(transcript.height),
        width: inner.width,
        height: say_height,
    };
    if transcript.height > 0 {
        let mut rows = wrap_spans(
            vec![Span::styled(app.ticker_line(), palette.warn)],
            transcript.width,
        );
        for line in app.chat_for_selected_room() {
            rows.extend(wrap_message(
                vec![
                    Span::styled(format!("{} ", line.time), palette.muted),
                    Span::styled(format!("{}  ", line.user), palette.brand),
                ],
                &line.text,
                transcript.width,
            ));
        }
        let viewport = usize::from(transcript.height);
        let origin = app.window_origin(ScrollId::Transcript, rows.len(), viewport);
        let end = (origin + viewport).min(rows.len());
        pointer.wheel(transcript, ScrollId::Transcript);
        frame.render_widget(Paragraph::new(rows[origin..end].to_vec()), transcript);
        scroll::paint(
            frame,
            scroll::padding_track(columns[1], transcript.y, transcript.height),
            rows.len(),
            viewport,
            origin,
            pointer,
            ScrollId::Transcript,
        );
    }
    if say_row.height > 0 {
        let rows = fit_rows(draft_rows, usize::from(say_row.height));
        frame.render_widget(Paragraph::new(rows).style(draft_style), say_row);
        pointer.focus_query(say_row);
    }

    draw_people(frame, columns[2], app, pointer);
    pointer.resize(columns[0], Split::ChatRooms, Edge::East);
    pointer.resize(columns[1], Split::ChatRooms, Edge::West);
    pointer.resize(columns[1], Split::ChatMembers, Edge::East);
    pointer.resize(columns[2], Split::ChatMembers, Edge::West);
}

fn say_line(app: &App) -> (String, Style) {
    let palette = theme();
    if app.saying() {
        return (format!("{}█", app.say_draft()), palette.brand);
    }
    if !app.say_draft().is_empty() {
        return (app.say_draft().to_owned(), palette.text);
    }
    ("type a message".to_owned(), palette.muted)
}

#[derive(Clone, Copy)]
struct Glyph {
    ch: char,
    style: Style,
}

/// Wrap styled spans onto rows of `width` display columns.
///
/// A break falls on the last whitespace that fits, and that space is left off
/// the next row. A token wider than `width` breaks at the last column that fits.
/// `\n` starts a new row. A width of 0 produces no rows.
fn wrap_spans(spans: Vec<Span<'static>>, width: u16) -> Vec<Line<'static>> {
    let columns = usize::from(width);
    if columns == 0 {
        return Vec::new();
    }
    let mut rows = Vec::new();
    for segment in split_spans(&spans) {
        if segment.is_empty() {
            rows.push(Line::from(""));
        } else {
            rows.extend(wrap_styled(&segment, columns));
        }
    }
    rows
}

/// First row is `prefix` plus as much of `body` as fits. Later rows are body
/// only. A prefix wider than `width` wraps on its own, and the body starts
/// on the following row.
fn wrap_message(prefix: Vec<Span<'static>>, body: &str, width: u16) -> Vec<Line<'static>> {
    let columns = usize::from(width);
    if columns == 0 {
        return Vec::new();
    }
    let prefix_columns = Line::from(prefix.clone()).width();
    if prefix_columns > columns {
        let mut rows = wrap_spans(prefix, width);
        if body.is_empty() {
            return rows;
        }
        for segment in body.split('\n') {
            rows.extend(wrap_spans(vec![Span::raw(segment.to_owned())], width));
        }
        return rows;
    }
    let mut pieces = body.split('\n');
    let first = pieces.next().unwrap_or("");
    let glyphs = glyphs_of_text(first);
    let (head, rest) = split_first_row(&glyphs, columns - prefix_columns);
    let mut line = prefix;
    line.extend(glyphs_to_spans(&head));
    let mut rows = vec![Line::from(line)];
    if !rest.is_empty() {
        rows.extend(wrap_styled(&rest, columns));
    }
    for segment in pieces {
        rows.extend(wrap_spans(vec![Span::raw(segment.to_owned())], width));
    }
    rows
}

fn fit_rows(mut rows: Vec<Line<'static>>, height: usize) -> Vec<Line<'static>> {
    if rows.len() > height {
        let skip = rows.len() - height;
        rows.drain(0..skip);
    }
    rows
}

fn split_spans(spans: &[Span<'static>]) -> Vec<Vec<Glyph>> {
    let mut segments = vec![Vec::new()];
    for span in spans {
        for ch in span.content.chars() {
            if ch == '\n' {
                segments.push(Vec::new());
            } else {
                segments.last_mut().unwrap().push(Glyph {
                    ch,
                    style: span.style,
                });
            }
        }
    }
    segments
}

fn glyphs_of_text(text: &str) -> Vec<Glyph> {
    text.chars()
        .map(|ch| Glyph {
            ch,
            style: Style::default(),
        })
        .collect()
}

fn wrap_styled(glyphs: &[Glyph], width: usize) -> Vec<Line<'static>> {
    let mut rows = Vec::new();
    let mut start = 0;
    while start < glyphs.len() {
        let mut cols = 0;
        let mut end = start;
        let mut break_at = None;
        while end < glyphs.len() {
            let cols_ch = char_columns(glyphs[end].ch);
            if cols + cols_ch > width {
                break;
            }
            if glyphs[end].ch.is_whitespace() {
                break_at = Some(end);
            }
            cols += cols_ch;
            end += 1;
        }
        if end == start {
            end = start + 1;
            rows.push(glyphs_line(&glyphs[start..end]));
            start = end;
            continue;
        }
        if end < glyphs.len()
            && let Some(index) = break_at
            && index > start
        {
            rows.push(glyphs_line(&glyphs[start..index]));
            start = index + 1;
            continue;
        }
        rows.push(glyphs_line(&glyphs[start..end]));
        start = end;
    }
    rows
}

fn split_first_row(glyphs: &[Glyph], room: usize) -> (Vec<Glyph>, Vec<Glyph>) {
    if glyphs.is_empty() || room == 0 {
        return (Vec::new(), glyphs.to_vec());
    }
    if char_columns(glyphs[0].ch) > room {
        return (Vec::new(), glyphs.to_vec());
    }
    let mut cols = 0;
    let mut end = 0;
    let mut break_at = None;
    while end < glyphs.len() {
        let cols_ch = char_columns(glyphs[end].ch);
        if cols + cols_ch > room {
            break;
        }
        if glyphs[end].ch.is_whitespace() {
            break_at = Some(end);
        }
        cols += cols_ch;
        end += 1;
    }
    if end == glyphs.len() {
        return (glyphs.to_vec(), Vec::new());
    }
    if let Some(index) = break_at
        && index > 0
    {
        return (glyphs[..index].to_vec(), glyphs[index + 1..].to_vec());
    }
    (glyphs[..end].to_vec(), glyphs[end..].to_vec())
}

fn glyphs_line(glyphs: &[Glyph]) -> Line<'static> {
    Line::from(glyphs_to_spans(glyphs))
}

fn glyphs_to_spans(glyphs: &[Glyph]) -> Vec<Span<'static>> {
    if glyphs.is_empty() {
        return Vec::new();
    }
    let mut spans = Vec::new();
    let mut buf = String::new();
    let mut style = glyphs[0].style;
    for glyph in glyphs {
        if glyph.style != style {
            spans.push(Span::styled(std::mem::take(&mut buf), style));
            style = glyph.style;
        }
        buf.push(glyph.ch);
    }
    if !buf.is_empty() {
        spans.push(Span::styled(buf, style));
    }
    spans
}

fn char_columns(ch: char) -> usize {
    Span::raw(ch.to_string()).width()
}

fn draw_people(frame: &mut Frame, area: Rect, app: &App, pointer: &mut Pointer) {
    let people = app.room_members();
    let cursor = app.member_cursor();
    let block = bordered("in room");
    let inner = block.inner(area);
    frame.render_widget(block, area);
    if inner.width == 0 || inner.height == 0 {
        return;
    }
    let height = usize::from(inner.height);
    let start = app.window_origin(ScrollId::Members, people.len(), height.max(1));
    pointer.wheel(inner, ScrollId::Members);
    for (offset, person) in people.iter().skip(start).take(height).enumerate() {
        let index = start + offset;
        let style = row_style(index == cursor, index.is_multiple_of(2));
        let row = Rect {
            x: inner.x,
            y: inner
                .y
                .saturating_add(u16::try_from(offset).unwrap_or(u16::MAX)),
            width: inner.width,
            height: 1,
        };
        pointer.control(row, Target::Person(index));
        let label = match person.status {
            Some(status) => format!(
                "{}  {}",
                person.name,
                crate::protocol::UserStatus::from_wire(status).label()
            ),
            None => person.name.clone(),
        };
        frame.render_widget(Paragraph::new(label).style(style), row);
    }
    scroll::paint(
        frame,
        scroll::padding_track(area, inner.y, inner.height),
        people.len(),
        height,
        start,
        pointer,
        ScrollId::Members,
    );
}

pub fn draw_person(frame: &mut Frame, area: Rect, app: &App, pointer: &mut Pointer) {
    if !app.person_open() {
        return;
    }
    let palette = theme();
    let summary = app.person_summary();
    let actions = app.person_actions();
    let width = summary
        .iter()
        .chain(actions.iter())
        .map(|line| line.chars().count())
        .max()
        .unwrap_or(24)
        .max(24)
        .saturating_add(4);
    let width = u16::try_from(width).unwrap_or(u16::MAX).min(area.width);
    let height = u16::try_from(summary.len() + actions.len() + 3)
        .unwrap_or(u16::MAX)
        .min(area.height);
    let popup = Rect {
        x: area.x + area.width.saturating_sub(width) / 2,
        y: area.y + area.height.saturating_sub(height) / 2,
        width,
        height,
    };
    pointer.picker_stay(popup);
    let block = bordered("user");
    let inner = block.inner(popup);
    frame.render_widget(block, popup);
    if inner.width == 0 || inner.height == 0 {
        return;
    }
    let mut lines = Vec::new();
    for line in &summary {
        lines.push(Line::from(Span::styled(line.clone(), palette.text)));
    }
    lines.push(Line::raw(""));
    let text_height = u16::try_from(lines.len()).unwrap_or(u16::MAX);
    let text = Rect {
        x: inner.x,
        y: inner.y,
        width: inner.width,
        height: text_height.min(inner.height),
    };
    frame.render_widget(Paragraph::new(lines), text);
    let mut y = inner.y.saturating_add(text_height);
    for (index, action) in actions.iter().enumerate() {
        if y >= inner.y.saturating_add(inner.height) {
            break;
        }
        let row = Rect {
            x: inner.x,
            y,
            width: inner.width,
            height: 1,
        };
        let selected = u8::try_from(index).unwrap_or(u8::MAX) == app.person_action();
        pointer.control(row, Target::PersonAction(u8::try_from(index).unwrap_or(4)));
        let style = if selected {
            palette.selected
        } else {
            palette.text
        };
        frame.render_widget(Paragraph::new(action.as_str()).style(style), row);
        y = y.saturating_add(1);
    }
}

fn draw_users(frame: &mut Frame, area: Rect, app: &App, pointer: &mut Pointer) {
    let detail = app.users_detail_width(area.width);
    let columns = horizontal(area, &[area.width.saturating_sub(detail), detail]);
    let rows = app
        .users()
        .iter()
        .map(|user| {
            let flags = format!(
                "{}{}{}",
                flag(user.notify, "n"),
                flag(user.prioritized, "p"),
                flag(user.trusted, "t")
            );
            line_row(vec![
                Span::raw(user.list.label()),
                Span::raw(user.name.clone()),
                Span::styled(user.status.label(), status_style(user.status)),
                Span::raw(user.country.clone()),
                Span::raw(user.note.clone()),
                Span::raw(flags),
            ])
        })
        .collect();
    draw_table(
        frame,
        columns[0],
        "buddies · ignored · banned",
        &["list", "user", "status", "cc", "note", "flags"],
        &[
            Constraint::Length(8),
            Constraint::Length(14),
            Constraint::Length(8),
            Constraint::Length(4),
            Constraint::Fill(1),
            Constraint::Length(5),
        ],
        rows,
        app.cursor(),
        app,
        pointer,
    );
    let mut body = app.user_detail();
    body.push_str("\n\nflags: n notify  p priority  t trusted");
    frame.render_widget(
        Paragraph::new(body).block(bordered("user info")),
        columns[1],
    );
    pointer.resize(columns[0], Split::UsersDetail, Edge::East);
    pointer.resize(columns[1], Split::UsersDetail, Edge::West);
}

fn draw_shares(frame: &mut Frame, area: Rect, app: &App, pointer: &mut Pointer) {
    let groups = app.shares();
    let (left, middle, right) = app.share_widths(area.width);
    let columns = horizontal(area, &[left, middle, right]);
    for (index, (column, group)) in columns.iter().zip(groups).enumerate() {
        pointer.select(*column, index);
        let title = if index == app.cursor() {
            format!("▸ {} shares", group.name)
        } else {
            format!("{} shares", group.name)
        };
        let palette = theme();
        let selected = index == app.cursor();
        let paths = app.share_paths()[index];
        let block = bordered(&title);
        let inner = block.inner(*column);
        frame.render_widget(block, *column);
        if inner.height == 0 {
            continue;
        }
        let mut lines = vec![
            Line::from(Span::styled(group.access, palette.muted)),
            Line::raw(""),
            Line::from(Span::styled(
                format!("{} folders", group.folders),
                palette.brand,
            )),
            Line::from(Span::styled(format!("{} files", group.files), palette.text)),
            Line::raw(""),
        ];
        if paths.is_empty() {
            lines.push(Line::from(Span::styled("no folders yet", palette.warn)));
        }
        let footer = if selected { 2 } else { 0 };
        let viewport = usize::from(inner.height).saturating_sub(lines.len() + footer);
        let column_id = u8::try_from(index).unwrap_or(0);
        let origin = app.window_origin(ScrollId::Share(column_id), paths.len(), viewport);
        let end = (origin + viewport).min(paths.len());
        for (path_index, path) in paths.iter().enumerate().take(end).skip(origin) {
            let mark = if selected && path_index == app.path_cursor().min(paths.len() - 1) {
                "▸ "
            } else {
                "  "
            };
            lines.push(Line::from(Span::styled(
                format!("{mark}{path}"),
                palette.text,
            )));
        }
        if selected {
            lines.push(Line::raw(""));
            lines.push(Line::from(Span::styled(
                "enter picks a folder",
                palette.brand,
            )));
        }
        frame.render_widget(Paragraph::new(lines), inner);
        scroll::paint(
            frame,
            scroll::padding_track(*column, inner.y, inner.height),
            paths.len(),
            viewport,
            origin,
            pointer,
            ScrollId::Share(column_id),
        );
    }
    pointer.resize(columns[0], Split::SharesLeft, Edge::East);
    pointer.resize(columns[1], Split::SharesLeft, Edge::West);
    pointer.resize(columns[1], Split::SharesRight, Edge::East);
    pointer.resize(columns[2], Split::SharesRight, Edge::West);
}

fn draw_settings(frame: &mut Frame, area: Rect, app: &App, pointer: &mut Pointer) {
    let nav = app.settings_nav_width(area.width);
    let columns = horizontal(area, &[nav, area.width.saturating_sub(nav)]);
    let rows = crate::settings::SECTIONS
        .iter()
        .map(|name| line_row(vec![Span::raw(*name)]))
        .collect();
    draw_table(
        frame,
        columns[0],
        "settings",
        &["section"],
        &[Constraint::Fill(1)],
        rows,
        app.cursor(),
        app,
        pointer,
    );
    let palette = theme();
    let block = bordered(app.setting_section());
    let inner = block.inner(columns[1]);
    frame.render_widget(block, columns[1]);
    let fields = app.setting_lines();
    let height = usize::from(inner.height);
    let origin = app.window_origin(ScrollId::Settings, fields.len(), height);
    let end = (origin + height).min(fields.len());
    let lines = fields[origin..end]
        .iter()
        .enumerate()
        .map(|(offset, (label, value, selected))| {
            let index = origin + offset;
            pointer.setting(
                Rect {
                    x: inner.x,
                    y: inner.y.saturating_add(u16::try_from(offset).unwrap_or(0)),
                    width: inner.width,
                    height: 1,
                },
                index,
            );
            let mark = if *selected { "▸ " } else { "  " };
            Line::from(vec![
                Span::styled(format!("{mark}{label:<18}"), palette.muted),
                Span::styled(
                    value.clone(),
                    if *selected {
                        palette.brand
                    } else {
                        palette.text
                    },
                ),
            ])
        })
        .collect::<Vec<_>>();
    if inner.height > 0 {
        frame.render_widget(Paragraph::new(lines), inner);
    }
    scroll::paint(
        frame,
        scroll::padding_track(columns[1], inner.y, inner.height),
        fields.len(),
        height,
        origin,
        pointer,
        ScrollId::Settings,
    );
    pointer.resize(columns[0], Split::SettingsNav, Edge::East);
    pointer.resize(columns[1], Split::SettingsNav, Edge::West);
}

fn line_rect(area: Rect, offset: u16) -> Rect {
    Rect {
        x: area.x,
        y: area.y.saturating_add(offset),
        width: area.width,
        height: 1,
    }
}

fn search_row(hit: &SearchLine) -> Vec<Span<'static>> {
    let label = if hit.folder {
        let mark = if hit.expanded { "▾" } else { "▸" };
        format!("{mark} {}   {} files", hit.path, hit.count)
    } else if hit.nested {
        format!("  {}", file_name(&hit.path))
    } else {
        hit.path.clone()
    };
    vec![
        Span::raw(hit.user.clone()),
        Span::raw(label),
        Span::raw(format_bytes(hit.size)),
        Span::raw(option_kbps(hit.bitrate)),
        Span::raw(
            hit.duration
                .map(format_duration)
                .unwrap_or_else(|| "—".to_owned()),
        ),
        Span::raw(hit.queue.to_string()),
        Span::raw(if hit.free_slot { "yes" } else { "no" }),
        Span::raw(hit.country.clone()),
    ]
}

fn file_name(path: &str) -> &str {
    path.rsplit(['\\', '/']).next().unwrap_or(path)
}

fn transfer_row(transfer: &Transfer) -> Vec<Line<'static>> {
    let queue = transfer
        .queue
        .map(|place| place.to_string())
        .unwrap_or_else(|| "—".to_owned());
    vec![
        Line::from(transfer.user.clone()),
        Line::from(transfer.path.clone()),
        progress_line(
            transfer.percent(),
            match transfer.direction {
                Direction::Download => Heat::Down,
                Direction::Upload => Heat::Up,
            },
        ),
        Line::from(if transfer.speed == 0 {
            "—".to_owned()
        } else {
            format_rate(transfer.speed)
        }),
        Line::from(queue),
        Line::from(Span::styled(
            transfer.status(),
            transfer_style(transfer.state),
        )),
    ]
}

fn option_kbps(bitrate: Option<u32>) -> String {
    bitrate
        .map(|value| format!("{value}k"))
        .unwrap_or_else(|| "—".to_owned())
}

fn flag(on: bool, mark: &str) -> String {
    if on { mark.to_owned() } else { "·".to_owned() }
}

#[cfg(test)]
mod tests {
    use ratatui::style::Color;

    use super::*;

    fn row_text(line: &Line<'_>) -> String {
        line.spans
            .iter()
            .map(|span| span.content.as_ref())
            .collect()
    }

    #[test]
    fn a_short_line_stays_one_row() {
        let rows = wrap_spans(vec![Span::raw("hello")], 10);
        assert_eq!(rows.len(), 1);
        assert_eq!(row_text(&rows[0]), "hello");
    }

    #[test]
    fn a_spaced_sentence_wraps_narrow_and_stays_one_row_when_wide() {
        let text = "hello world";
        let narrow = wrap_spans(vec![Span::raw(text)], 8);
        assert_eq!(narrow.len(), 2);
        assert_eq!(row_text(&narrow[0]), "hello");
        assert_eq!(row_text(&narrow[1]), "world");
        let wide = wrap_spans(vec![Span::raw(text)], 11);
        assert_eq!(wide.len(), 1);
        assert_eq!(row_text(&wide[0]), "hello world");
    }

    #[test]
    fn a_spaceless_token_continues_on_the_next_row() {
        let rows = wrap_spans(vec![Span::raw("abcdefghij")], 4);
        assert_eq!(rows.len(), 3);
        assert_eq!(row_text(&rows[0]), "abcd");
        assert_eq!(row_text(&rows[1]), "efgh");
        assert_eq!(row_text(&rows[2]), "ij");
    }

    #[test]
    fn a_newline_forces_a_new_row() {
        let rows = wrap_spans(vec![Span::raw("hello\nworld")], 20);
        assert_eq!(rows.len(), 2);
        assert_eq!(row_text(&rows[0]), "hello");
        assert_eq!(row_text(&rows[1]), "world");
    }

    #[test]
    fn a_double_width_character_counts_as_two_columns() {
        let rows = wrap_spans(vec![Span::raw("ab你cd")], 4);
        assert_eq!(row_text(&rows[0]), "ab你");
        assert_eq!(row_text(&rows[1]), "cd");
        let tight = wrap_spans(vec![Span::raw("a你")], 2);
        assert_eq!(tight.len(), 2);
        assert_eq!(row_text(&tight[0]), "a");
        assert_eq!(row_text(&tight[1]), "你");
    }

    #[test]
    fn width_zero_yields_no_rows() {
        assert!(wrap_spans(vec![Span::raw("hi")], 0).is_empty());
        assert!(wrap_message(vec![Span::raw("hi")], "there", 0).is_empty());
    }

    #[test]
    fn the_message_body_continues_on_the_next_row() {
        let prefix = vec![Span::raw("18:02 ada  ")];
        let narrow = wrap_message(prefix.clone(), "violet lanterns", 20);
        assert_eq!(row_text(&narrow[0]), "18:02 ada  violet");
        assert_eq!(row_text(&narrow[1]), "lanterns");
        let wide = wrap_message(prefix, "violet lanterns", 40);
        assert_eq!(wide.len(), 1);
        assert_eq!(row_text(&wide[0]), "18:02 ada  violet lanterns");
    }

    #[test]
    fn a_newline_in_the_body_starts_a_new_row() {
        let rows = wrap_message(vec![Span::raw("t ")], "hello\nworld", 40);
        assert_eq!(rows.len(), 2);
        assert_eq!(row_text(&rows[0]), "t hello");
        assert_eq!(row_text(&rows[1]), "world");
    }

    #[test]
    fn a_prefix_wider_than_the_row_wraps_before_the_body() {
        let prefix = vec![
            Span::styled("12345678", Style::default().fg(Color::Red)),
            Span::styled("abcd", Style::default().fg(Color::Blue)),
        ];
        let rows = wrap_message(prefix, "hello", 6);
        assert_eq!(row_text(&rows[0]), "123456");
        assert_eq!(row_text(&rows[1]), "78abcd");
        assert_eq!(row_text(&rows[2]), "hello");
        assert_eq!(rows[0].spans[0].style.fg, Some(Color::Red));
        assert_eq!(rows[1].spans[0].style.fg, Some(Color::Red));
        assert_eq!(rows[1].spans[1].style.fg, Some(Color::Blue));
    }

    #[test]
    fn the_newest_rows_stay_when_the_pane_is_shorter_than_the_message() {
        let rows = wrap_spans(vec![Span::raw("abcdefghij")], 4);
        let fitted = fit_rows(rows, 2);
        assert_eq!(fitted.len(), 2);
        assert_eq!(row_text(&fitted[0]), "efgh");
        assert_eq!(row_text(&fitted[1]), "ij");
    }
}
