use ratatui::Frame;
use ratatui::layout::{Constraint, Rect};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;

use crate::app::App;
use crate::model::{Direction, Transfer};
use crate::panels::{Edge, Split};
use crate::ui::chart::{Heat, chart_lines, progress_line, smooth_rates};
use crate::ui::chrome::{bordered, draw_table};
use crate::ui::format::{format_rate, latest, peak};
use crate::ui::layout::{horizontal, vertical};
use crate::ui::pointer::Pointer;
use crate::ui::theme::{theme, transfer_style};

pub fn draw(frame: &mut Frame, area: Rect, app: &App, pointer: &mut Pointer) {
    let meters = app.dash_meters_height(area.height);
    let rows = vertical(area, &[meters, area.height.saturating_sub(meters)]);
    draw_meters(frame, rows[0], app, pointer);
    draw_queue(frame, rows[1], app, pointer);
    pointer.resize(rows[0], Split::DashMeters, Edge::South);
    pointer.resize(rows[1], Split::DashMeters, Edge::North);
}

fn draw_meters(frame: &mut Frame, area: Rect, app: &App, pointer: &mut Pointer) {
    let (download, upload, network) = app.dash_columns(area.width);
    let columns = horizontal(area, &[download, upload, network]);
    draw_meter(
        frame,
        columns[0],
        "download",
        app.down_history(),
        Heat::Down,
    );
    draw_meter(frame, columns[1], "upload", app.up_history(), Heat::Up);
    draw_network(frame, columns[2], app);
    pointer.resize(columns[1], Split::DashNetwork, Edge::East);
    pointer.resize(columns[2], Split::DashNetwork, Edge::West);
    pointer.resize(columns[0], Split::DashDownload, Edge::East);
    pointer.resize(columns[1], Split::DashDownload, Edge::West);
}

fn draw_meter(frame: &mut Frame, area: Rect, title: &str, samples: &[u64], heat: Heat) {
    let block = bordered(title);
    let inner = block.inner(area);
    frame.render_widget(block, area);
    if inner.width == 0 || inner.height == 0 {
        return;
    }
    let chart_height = inner.height.saturating_sub(1);
    let shown = smooth_rates(samples);
    let mut lines = chart_lines(&shown, inner.width, chart_height, heat);
    lines.push(Line::from(Span::styled(
        format!(
            "{}   peak {}",
            format_rate(latest(&shown)),
            format_rate(peak(samples))
        ),
        theme().muted,
    )));
    frame.render_widget(Paragraph::new(lines), inner);
}

fn draw_network(frame: &mut Frame, area: Rect, app: &App) {
    let palette = theme();
    let parent = app.parent().unwrap_or("none").to_owned();
    let children = app.child_count().to_string();
    let body = [
        ("listen", "—".to_owned()),
        ("parent", parent),
        ("children", children),
        ("privileges", "no".to_owned()),
        ("slots", app.upload_slot_line()),
        ("peers", "0".to_owned()),
    ]
    .into_iter()
    .map(|(label, value)| {
        Line::from(vec![
            Span::styled(format!("{label:<12}"), palette.muted),
            Span::styled(value, palette.text),
        ])
    })
    .collect::<Vec<_>>();
    frame.render_widget(Paragraph::new(body).block(bordered("network")), area);
}

fn draw_queue(frame: &mut Frame, area: Rect, app: &App, pointer: &mut Pointer) {
    let rows = app.transfers().iter().map(transfer_row).collect::<Vec<_>>();
    draw_table(
        frame,
        area,
        "queue",
        &["", "user", "file", "progress", "speed", "q", "state"],
        &[
            Constraint::Length(1),
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

fn transfer_row(transfer: &Transfer) -> Vec<Line<'static>> {
    let direction_style = match transfer.direction {
        Direction::Download => theme().down,
        Direction::Upload => theme().up,
    };
    let queue = transfer
        .queue
        .map(|place| place.to_string())
        .unwrap_or_else(|| "—".to_owned());
    vec![
        Line::from(Span::styled(transfer.direction.glyph(), direction_style)),
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
