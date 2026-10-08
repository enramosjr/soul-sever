use ratatui::Frame;
use ratatui::layout::{Constraint, Rect};
use ratatui::style::Style;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Borders, Cell, Padding, Paragraph, Row, Table};

use crate::app::{App, ScrollId, View};
use crate::ui::pointer::Pointer;
use crate::ui::scroll;
use crate::ui::theme::theme;

pub struct TabHit {
    pub view: View,
    pub start: u16,
    pub end: u16,
}

pub fn bordered(title: &str) -> Block<'static> {
    let palette = theme();
    Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(palette.border)
        .style(palette.base)
        .padding(Padding::horizontal(1))
        .title(Line::from(format!(" {title} ")).style(palette.title))
}

pub fn draw_header(frame: &mut Frame, area: Rect, app: &App) {
    frame.render_widget(Paragraph::new(header_line(area.width, app)), area);
}

pub fn draw_hints(frame: &mut Frame, area: Rect, app: &App, pointer: &mut Pointer) {
    let palette = theme();
    let help_width = 6u16.min(area.width);
    let help_area = Rect {
        x: area.x + area.width - help_width,
        y: area.y,
        width: help_width,
        height: area.height,
    };
    pointer.open_help(help_area);
    let hints_area = Rect {
        x: area.x,
        y: area.y,
        width: area.width.saturating_sub(help_width),
        height: area.height,
    };
    let (text, style) = if app.notice().is_empty() {
        (format!(" {}", app.hints()), palette.muted)
    } else if app.view() == View::Search && app.logged_in() {
        (
            format!(" {}   ·   {}", app.notice(), app.download_cue()),
            palette.warn,
        )
    } else {
        (format!(" {}", app.notice()), palette.warn)
    };
    frame.render_widget(Paragraph::new(text).style(style), hints_area);
    frame.render_widget(Paragraph::new(" ? ").style(palette.brand), help_area);
}

pub fn draw_tabs(frame: &mut Frame, area: Rect, app: &App, pointer: &mut Pointer) {
    let (line, hits) = tabs(area.width, app.view());
    frame.render_widget(Paragraph::new(line), area);
    for hit in hits {
        pointer.open_view(
            Rect {
                x: area.x + hit.start,
                y: area.y,
                width: hit.end.saturating_sub(hit.start),
                height: 1,
            },
            hit.view,
        );
    }
}

pub fn tab_at(area: Rect, column: u16, row: u16, active_width: u16) -> Option<View> {
    if area.height < 2 {
        return None;
    }
    let tab_row = area.y + area.height - 2;
    if row != tab_row {
        return None;
    }
    let local_x = column.saturating_sub(area.x);
    tabs(active_width, View::Dashboard)
        .1
        .into_iter()
        .find(|hit| local_x >= hit.start && local_x < hit.end)
        .map(|hit| hit.view)
}

const WORDMARK: [&str; 5] = [
    "█████ █████ █   █ █      █████ █████ █   █ █████ █████",
    "█     █   █ █   █ █      █     █     █   █ █     █   █",
    "█████ █   █ █   █ █      █████ ████  █   █ ████  ████ ",
    "    █ █   █ █   █ █          █ █      █ █  █     █  █ ",
    "█████ █████ █████ █████  █████ █████   █   █████ █   █",
];

pub fn draw_help(frame: &mut Frame, area: Rect, signing_in: bool) {
    let palette = theme();
    let mut lines = WORDMARK
        .into_iter()
        .map(|row| Line::from(Span::styled(row, palette.brand)))
        .collect::<Vec<_>>();
    lines.push(Line::from(Span::styled(credit_line(), palette.muted)));
    lines.push(Line::raw(""));
    for row in help_commands(signing_in) {
        lines.push(Line::from(Span::styled(*row, palette.text)));
    }
    let inner = lines.iter().map(Line::width).max().unwrap_or(0);
    let width = u16::try_from(inner)
        .unwrap_or(u16::MAX)
        .saturating_add(4)
        .min(area.width);
    let height = u16::try_from(lines.len())
        .unwrap_or(u16::MAX)
        .saturating_add(2)
        .min(area.height);
    let popup = Rect {
        x: area.x + area.width.saturating_sub(width) / 2,
        y: area.y + area.height.saturating_sub(height) / 2,
        width,
        height,
    };
    frame.render_widget(Paragraph::new(lines).block(bordered("help")), popup);
}

const CREDIT: &str = "-by E.N. Ramos <enramos@live.com>-";

fn credit_line() -> String {
    let width = WORDMARK[0].chars().count();
    let credit = CREDIT.chars().count();
    let pad = width.saturating_sub(credit);
    format!("{}{CREDIT}", " ".repeat(pad))
}

fn help_commands(signing_in: bool) -> &'static [&'static str] {
    if signing_in {
        &[
            " type a username",
            " enter    move to the password",
            " type the password",
            " enter    sign in and save it",
            " tab      switch field",
            " esc      quit",
            " ctrl-c   quit",
            "",
            " a name that is not registered creates a new account",
            " nothing is sent until you sign in",
        ]
    } else {
        &[
            " 1-9 0 a  switch view",
            " a        quality: measuring and replacement",
            " r        check quality again",
            " [ ]      artist, album, or song",
            " enter    play the highlighted song",
            " space    pause or resume",
            " s        stop",
            " tab      next view",
            " h / l    previous / next view",
            " j / k    move selection",
            " /        edit the search query",
            " m        cycle search mode",
            " d        download the highlighted album or song",
            " x        remove the highlighted queue row",
            " x        delete the highlighted library item",
            " X        clear finished rows",
            " enter    open an album",
            " click    a tab, a row, the query, or mode",
            " wheel    scroll the list",
            " `        show or hide the log",
            " ?        close this help",
            " q        quit, or close help",
            " ctrl-c   quit",
        ]
    }
}

pub fn draw_log(frame: &mut Frame, area: Rect, app: &App, pointer: &mut Pointer) {
    if area.height == 0 {
        return;
    }
    let palette = theme();
    let title_row = Rect {
        x: area.x,
        y: area.y,
        width: area.width,
        height: 1,
    };
    pointer.toggle_log(title_row);
    if !app.log_open() || area.height == 1 {
        let latest = app
            .log_lines()
            .last()
            .map(String::as_str)
            .unwrap_or("no messages");
        frame.render_widget(
            Paragraph::new(format!(" ▸ log   {latest}")).style(palette.muted),
            area,
        );
        return;
    }
    let viewport = usize::from(area.height.saturating_sub(2));
    let lines = app.log_lines();
    let origin = app.window_origin(ScrollId::Log, lines.len(), viewport);
    let end = (origin + viewport).min(lines.len());
    let body: Vec<Line> = if lines.is_empty() {
        vec![Line::from(" no messages").style(palette.muted)]
    } else {
        lines[origin..end]
            .iter()
            .map(|line| Line::from(format!(" {line}")).style(palette.base))
            .collect()
    };
    let body_row = Rect {
        x: area.x,
        y: area.y.saturating_add(1),
        width: area.width,
        height: area.height.saturating_sub(2),
    };
    pointer.wheel(body_row, ScrollId::Log);
    frame.render_widget(
        Paragraph::new(body).block(bordered("log  ·  ` hides")),
        area,
    );
    scroll::paint(
        frame,
        scroll::padding_track(area, body_row.y, body_row.height),
        lines.len(),
        viewport,
        origin,
        pointer,
        ScrollId::Log,
    );
}

pub fn draw_too_small(frame: &mut Frame, area: Rect) {
    frame.render_widget(
        Paragraph::new(" terminal too small — need 70×18")
            .style(theme().warn)
            .block(bordered("soul sever")),
        area,
    );
}

pub(crate) fn line_row(cells: Vec<Span<'static>>) -> Vec<Line<'static>> {
    cells.into_iter().map(Line::from).collect()
}

#[allow(clippy::too_many_arguments)]
pub fn draw_table(
    frame: &mut Frame,
    area: Rect,
    title: &str,
    headers: &[&str],
    widths: &[Constraint],
    rows: Vec<Vec<Line<'static>>>,
    cursor: usize,
    app: &App,
    pointer: &mut Pointer,
) {
    let body = area.height.saturating_sub(3) as usize;
    let height = body.max(1);
    let content = rows.len();
    let start = app.window_origin(ScrollId::List, content, height);
    let shown = height.min(content.saturating_sub(start));
    let visible: Vec<_> = rows.into_iter().skip(start).take(shown).collect();
    paint_rows(
        frame, area, title, headers, widths, visible, start, cursor, pointer, content, height,
    );
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn paint_rows(
    frame: &mut Frame,
    area: Rect,
    title: &str,
    headers: &[&str],
    widths: &[Constraint],
    rows: Vec<Vec<Line<'static>>>,
    first: usize,
    cursor: usize,
    pointer: &mut Pointer,
    content: usize,
    viewport: usize,
) {
    let palette = theme();
    pointer.scroll(area);
    for (offset, _) in rows.iter().enumerate() {
        let y = area
            .y
            .saturating_add(2)
            .saturating_add(u16::try_from(offset).unwrap_or(u16::MAX));
        pointer.select(
            Rect {
                x: area.x,
                y,
                width: area.width,
                height: 1,
            },
            first + offset,
        );
    }
    let visible: Vec<Row> = rows
        .into_iter()
        .enumerate()
        .map(|(offset, cells)| {
            let index = first + offset;
            let style = row_style(index == cursor, index.is_multiple_of(2));
            let cells = cells.into_iter().map(|line| {
                if index == cursor {
                    Cell::from(line).style(style)
                } else {
                    Cell::from(line)
                }
            });
            Row::new(cells).style(style)
        })
        .collect();
    let header = Row::new(headers.iter().copied().map(Cell::from)).style(palette.header);
    let table = Table::new(visible, widths.to_vec())
        .header(header)
        .block(bordered(title))
        .column_spacing(1);
    frame.render_widget(table, area);
    scroll::paint(
        frame,
        scroll::padding_track(
            area,
            area.y.saturating_add(2),
            area.height.saturating_sub(3),
        ),
        content,
        viewport,
        first,
        pointer,
        ScrollId::List,
    );
}

pub fn row_style(selected: bool, zebra: bool) -> Style {
    let palette = theme();
    if selected {
        palette.selected
    } else if zebra {
        palette.zebra
    } else {
        palette.text
    }
}

fn header_line(width: u16, app: &App) -> Line<'static> {
    let palette = theme();
    let clock = chrono::Local::now().format("%H:%M:%S").to_string();
    let mut spans = vec![
        Span::styled(" soul sever ", palette.brand),
        Span::styled(env!("CARGO_PKG_VERSION"), palette.muted),
        Span::raw("  "),
        Span::styled(
            format!(" {} ", app.connection_label()),
            if app.logged_in() {
                palette.brand
            } else {
                palette.warn
            },
        ),
        Span::styled(format!("  up {}  ", app.uptime()), palette.muted),
    ];
    if let Some(ip) = app.listen_ip() {
        spans.push(Span::styled(format!("{ip}  "), palette.text));
    }
    spans.push(Span::styled(app.server_label().to_owned(), palette.text));
    let used: usize = spans.iter().map(span_width).sum();
    let clock_width = clock.chars().count() + 1;
    if usize::from(width) > used + clock_width {
        spans.push(Span::raw(
            " ".repeat(usize::from(width) - used - clock_width),
        ));
        spans.push(Span::styled(clock, palette.brand));
    }
    Line::from(spans)
}

fn tabs(width: u16, active: View) -> (Line<'static>, Vec<TabHit>) {
    let palette = theme();
    let mut spans = vec![Span::raw(" ")];
    let mut hits = Vec::new();
    let mut x = 1u16;
    for view in View::ALL {
        let label = view.label();
        let piece = 1 + 1 + label.len() + 2;
        let next = x.saturating_add(u16::try_from(piece).unwrap_or(0));
        if next > width {
            break;
        }
        let on = view == active;
        let key_style = if on { palette.brand } else { palette.muted };
        let label_style = if on { palette.selected } else { palette.text };
        spans.push(Span::styled(view.key().to_string(), key_style));
        spans.push(Span::raw(" "));
        spans.push(Span::styled(label.to_owned(), label_style));
        spans.push(Span::raw("  "));
        hits.push(TabHit {
            view,
            start: x,
            end: next,
        });
        x = next;
    }
    (Line::from(spans), hits)
}

fn span_width(span: &Span<'_>) -> usize {
    span.content.chars().count()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reveal_keeps_the_cursor_on_screen() {
        assert_eq!(scroll::reveal(0, 0, 10, 30), 0);
        assert_eq!(scroll::reveal(0, 0, 100, 10), 0);
        assert_eq!(scroll::reveal(0, 50, 100, 10), 41);
        assert_eq!(scroll::reveal(0, 99, 100, 10), 90);
    }

    #[test]
    fn wordmark_rows_are_one_width() {
        let width = WORDMARK[0].chars().count();
        assert!(width > 40, "{width}");
        assert!(WORDMARK.iter().all(|row| row.chars().count() == width));
        let credit = credit_line();
        assert_eq!(credit.chars().count(), width);
        assert!(credit.ends_with(CREDIT));
        assert!(credit.starts_with(' '));
    }

    #[test]
    fn second_tab_is_search() {
        let (_, hits) = tabs(120, View::Dashboard);
        let search = hits.iter().find(|hit| hit.view == View::Search).unwrap();
        assert_eq!(
            tab_at(
                Rect {
                    x: 0,
                    y: 0,
                    width: 120,
                    height: 20
                },
                search.start,
                18,
                120
            ),
            Some(View::Search)
        );
    }
}
