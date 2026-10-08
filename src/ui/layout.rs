//! Exact panel rectangles. The last pane takes whatever width or height remains.

use ratatui::layout::Rect;

pub fn horizontal(area: Rect, widths: &[u16]) -> Vec<Rect> {
    let mut x = area.x;
    let mut rects = Vec::with_capacity(widths.len());
    for (index, width) in widths.iter().enumerate() {
        let remaining = area.x.saturating_add(area.width).saturating_sub(x);
        let width = if index + 1 == widths.len() {
            remaining
        } else {
            (*width).min(remaining)
        };
        rects.push(Rect {
            x,
            y: area.y,
            width,
            height: area.height,
        });
        x = x.saturating_add(width);
    }
    rects
}

pub fn vertical(area: Rect, heights: &[u16]) -> Vec<Rect> {
    let mut y = area.y;
    let mut rects = Vec::with_capacity(heights.len());
    for (index, height) in heights.iter().enumerate() {
        let remaining = area.y.saturating_add(area.height).saturating_sub(y);
        let height = if index + 1 == heights.len() {
            remaining
        } else {
            (*height).min(remaining)
        };
        rects.push(Rect {
            x: area.x,
            y,
            width: area.width,
            height,
        });
        y = y.saturating_add(height);
    }
    rects
}
