//! Filled braille charts, the same density btop uses for its meters.

use ratatui::style::{Color, Style};
use ratatui::text::{Line, Span};

#[derive(Clone, Copy)]
pub enum Heat {
    Down,
    Up,
}

/// Spreads each sample across about a second so one fast file is a hill, not a needle.
/// A steady rate is unchanged.
pub fn smooth_rates(samples: &[u64]) -> Vec<u64> {
    const WEIGHTS: [u64; 5] = [1, 4, 6, 4, 1];
    const DIV: u64 = 16;
    samples
        .iter()
        .enumerate()
        .map(|(index, _)| {
            let mut total = 0u64;
            for (offset, weight) in WEIGHTS.iter().copied().enumerate() {
                let Some(at) = index.checked_add(offset).and_then(|at| at.checked_sub(2)) else {
                    continue;
                };
                if let Some(value) = samples.get(at) {
                    total = total.saturating_add(value.saturating_mul(weight));
                }
            }
            total / DIV
        })
        .collect()
}

pub fn chart_lines(samples: &[u64], width: u16, height: u16, heat: Heat) -> Vec<Line<'static>> {
    let width = usize::from(width);
    let height = usize::from(height);
    if width == 0 || height == 0 {
        return Vec::new();
    }
    let columns = resample_max(samples, width * 2);
    paint(&columns, width, height, None, |_, tone| {
        heat_color(heat, tone)
    })
}

/// Frequency bars across the band. Low stays green, mids go amber, highs go red.
pub fn spectrum_lines(bands: &[f32], width: u16, height: u16) -> Vec<Line<'static>> {
    let width = usize::from(width);
    let height = usize::from(height);
    if width == 0 || height == 0 {
        return Vec::new();
    }
    let scaled = bands
        .iter()
        .map(|band| {
            let value = band.clamp(0.0, 1.0);
            (value * 10_000.0).round() as u64
        })
        .collect::<Vec<_>>();
    let columns = resample_stretch(&scaled, width * 2);
    // A fixed ceiling lets a quiet frame fall. Download charts still stretch to their own max.
    paint(&columns, width, height, Some(10_000), |column, tone| {
        spectrum_color(column, width, tone)
    })
}

fn paint(
    columns: &[u64],
    width: usize,
    height: usize,
    ceiling: Option<u64>,
    color_at: impl Fn(usize, u8) -> Color,
) -> Vec<Line<'static>> {
    let pixel_h = height * 4;
    let max = ceiling
        .unwrap_or_else(|| columns.iter().copied().max().unwrap_or(1))
        .max(1);
    let mut lines = Vec::with_capacity(height);
    for row in 0..height {
        let mut spans = Vec::with_capacity(width);
        for col in 0..width {
            let mut bits = 0u8;
            let mut peak = 0u64;
            for (sub, dots) in DOTS.iter().enumerate() {
                let value = columns.get(col * 2 + sub).copied().unwrap_or(0);
                peak = peak.max(value);
                let filled = filled_rows(value, max, pixel_h);
                for (subrow, dot) in dots.iter().copied().enumerate() {
                    let y_from_top = row * 4 + subrow;
                    let y_from_bottom = pixel_h - 1 - y_from_top;
                    if y_from_bottom < filled {
                        bits |= dot;
                    }
                }
            }
            let symbol = char::from_u32(0x2800 + u32::from(bits)).unwrap_or(' ');
            let tone = u8::try_from((u128::from(peak) * 255) / u128::from(max)).unwrap_or(255);
            spans.push(Span::styled(
                symbol.to_string(),
                Style::default().fg(color_at(col, tone)),
            ));
        }
        lines.push(Line::from(spans));
    }
    lines
}

fn filled_rows(value: u64, max: u64, pixel_h: usize) -> usize {
    if value == 0 {
        return 0;
    }
    let pixels = u128::try_from(pixel_h).unwrap_or(u128::MAX);
    let max = u128::from(max.max(1));
    let filled = (u128::from(value) * pixels).div_ceil(max).clamp(1, pixels);
    usize::try_from(filled).unwrap_or(pixel_h)
}

fn resample_stretch(samples: &[u64], out_len: usize) -> Vec<u64> {
    if out_len == 0 {
        return Vec::new();
    }
    if samples.is_empty() {
        return vec![0; out_len];
    }
    let mut out = vec![0; out_len];
    for (index, sample) in samples.iter().copied().enumerate() {
        let start = index * out_len / samples.len();
        let end = ((index + 1) * out_len / samples.len())
            .max(start.saturating_add(1))
            .min(out_len);
        for slot in &mut out[start..end] {
            *slot = (*slot).max(sample);
        }
    }
    out
}

fn spectrum_color(column: usize, width: usize, tone: u8) -> Color {
    let position = (column as f32 + 0.5) / width.max(1) as f32;
    let (dim, bright) = if position < 0.5 {
        let mix = (position / 0.5 * 255.0) as u8;
        (
            mix_rgb((16, 48, 24), (48, 36, 8), mix),
            mix_rgb((70, 230, 110), (255, 196, 64), mix),
        )
    } else {
        let mix = ((position - 0.5) / 0.5 * 255.0) as u8;
        (
            mix_rgb((48, 36, 8), (56, 12, 16), mix),
            mix_rgb((255, 196, 64), (255, 72, 64), mix),
        )
    };
    Color::Rgb(
        lerp(dim.0, bright.0, tone),
        lerp(dim.1, bright.1, tone),
        lerp(dim.2, bright.2, tone),
    )
}

fn mix_rgb(from: (u8, u8, u8), to: (u8, u8, u8), tone: u8) -> (u8, u8, u8) {
    (
        lerp(from.0, to.0, tone),
        lerp(from.1, to.1, tone),
        lerp(from.2, to.2, tone),
    )
}

fn resample_max(samples: &[u64], out_len: usize) -> Vec<u64> {
    if out_len == 0 {
        return Vec::new();
    }
    if samples.is_empty() {
        return vec![0; out_len];
    }
    if samples.len() <= out_len {
        let mut out = vec![0; out_len - samples.len()];
        out.extend_from_slice(samples);
        return out;
    }
    let mut out = vec![0; out_len];
    for (index, sample) in samples.iter().copied().enumerate() {
        let bucket = index * out_len / samples.len();
        out[bucket] = out[bucket].max(sample);
    }
    out
}

fn heat_color(heat: Heat, tone: u8) -> Color {
    let (low, high) = match heat {
        Heat::Down => ((18, 70, 28), (170, 255, 150)),
        Heat::Up => ((16, 40, 80), (150, 210, 255)),
    };
    Color::Rgb(
        lerp(low.0, high.0, tone),
        lerp(low.1, high.1, tone),
        lerp(low.2, high.2, tone),
    )
}

fn lerp(from: u8, to: u8, tone: u8) -> u8 {
    let from = i32::from(from);
    let to = i32::from(to);
    let tone = i32::from(tone);
    let value = from + (to - from) * tone / 255;
    u8::try_from(value.clamp(0, 255)).unwrap_or(0)
}

const DOTS: [[u8; 4]; 2] = [[0x01, 0x02, 0x04, 0x08], [0x10, 0x20, 0x40, 0x80]];

/// Horizontal braille meter for one transfer. Ten cells, the same family as the rate charts.
pub fn progress_line(percent: u8, heat: Heat) -> Line<'static> {
    const CELLS: usize = 10;
    let percent = percent.min(100);
    let pixels = CELLS * 2;
    let filled = if percent == 0 {
        0
    } else {
        (usize::from(percent) * pixels).div_ceil(100).min(pixels)
    };
    let mut spans = Vec::new();
    let mut chunk = String::new();
    let mut chunk_style = Style::default();
    for cell in 0..CELLS {
        let left = filled > cell * 2;
        let right = filled > cell * 2 + 1;
        let tip = left && filled <= cell * 2 + 2;
        let (glyph, tone) = if !left {
            ('⣀', 42)
        } else if tip {
            (if right { '⣿' } else { '⣇' }, 230)
        } else {
            ('⣿', 160)
        };
        let style = Style::default().fg(heat_color(heat, tone));
        if !chunk.is_empty() && style != chunk_style {
            spans.push(Span::styled(std::mem::take(&mut chunk), chunk_style));
        }
        chunk_style = style;
        chunk.push(glyph);
    }
    if !chunk.is_empty() {
        spans.push(Span::styled(chunk, chunk_style));
    }
    spans.push(Span::styled(
        format!(" {percent:>3}%"),
        Style::default().fg(heat_color(heat, 110)),
    ));
    Line::from(spans)
}

#[cfg(test)]
mod tests {
    use ratatui::style::Color;

    use super::*;

    fn line_text(line: &Line<'_>) -> String {
        line.spans
            .iter()
            .map(|span| span.content.as_ref())
            .collect()
    }

    #[test]
    fn a_download_meter_uses_the_braille_rail() {
        assert_eq!(line_text(&progress_line(0, Heat::Down)), "⣀⣀⣀⣀⣀⣀⣀⣀⣀⣀   0%");
        assert_eq!(
            line_text(&progress_line(100, Heat::Down)),
            "⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿ 100%"
        );
        assert_eq!(line_text(&progress_line(50, Heat::Up)), "⣿⣿⣿⣿⣿⣀⣀⣀⣀⣀  50%");
        assert_eq!(line_text(&progress_line(5, Heat::Down)), "⣇⣀⣀⣀⣀⣀⣀⣀⣀⣀   5%");
        let partial = progress_line(50, Heat::Down);
        assert!(partial.spans.len() >= 3);
        assert_ne!(
            partial.spans[0].style.fg,
            partial.spans.last().unwrap().style.fg
        );
    }

    #[test]
    fn full_cell_is_all_dots() {
        let lines = chart_lines(&[1, 1], 1, 1, Heat::Down);
        assert_eq!(lines.len(), 1);
        assert_eq!(lines[0].spans.len(), 1);
        assert_eq!(lines[0].spans[0].content.as_ref(), "⣿");
    }

    #[test]
    fn zero_samples_leave_the_cell_blank() {
        let lines = chart_lines(&[0, 0], 1, 1, Heat::Up);
        assert_eq!(lines[0].spans[0].content.as_ref(), "⠀");
    }

    #[test]
    fn short_series_is_right_aligned() {
        let columns = resample_max(&[9], 4);
        assert_eq!(columns, vec![0, 0, 0, 9]);
    }

    #[test]
    fn the_spectrum_is_green_on_the_left_and_red_on_the_right() {
        let low = spectrum_lines(&[1.0, 0.0], 2, 1);
        let high = spectrum_lines(&[0.0, 1.0], 2, 1);
        assert_eq!(low[0].spans[0].content.as_ref(), "⣿");
        assert_eq!(high[0].spans[1].content.as_ref(), "⣿");
        let (low_r, low_g, _) = rgb(low[0].spans[0].style.fg);
        let (high_r, high_g, _) = rgb(high[0].spans[1].style.fg);
        assert!(low_g > low_r, "{low_r} {low_g}");
        assert!(high_r > high_g, "{high_r} {high_g}");
    }

    #[test]
    fn a_quiet_band_stays_below_the_top() {
        let lines = spectrum_lines(&[0.25], 1, 4);
        assert_eq!(lines[0].spans[0].content.as_ref(), "⠀");
        assert_eq!(lines[3].spans[0].content.as_ref(), "⣿");
        let full = spectrum_lines(&[1.0], 1, 4);
        assert_eq!(full[0].spans[0].content.as_ref(), "⣿");
    }

    fn rgb(color: Option<Color>) -> (u8, u8, u8) {
        match color {
            Some(Color::Rgb(red, green, blue)) => (red, green, blue),
            other => panic!("expected rgb, got {other:?}"),
        }
    }

    #[test]
    fn a_one_tick_burst_becomes_a_hill_and_a_steady_rate_stays() {
        let hill = smooth_rates(&[0, 0, 90, 0, 0]);
        let peak = hill.iter().copied().max().unwrap();
        assert!(peak < 90);
        assert!(hill.iter().filter(|value| **value > 0).count() >= 3);
        assert_eq!(smooth_rates(&[10, 10, 10, 10, 10])[2], 10);
    }
}
