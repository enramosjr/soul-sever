//! Vertical scrollbar geometry and the one-column track drawn in a pane's padding.

use ratatui::Frame;
use ratatui::layout::Rect;

use crate::app::ScrollId;
use crate::ui::pointer::{Pointer, ScrollMetrics};
use crate::ui::theme::theme;

/// Thumb placement inside a track. `thumb_start` is a row offset from the top of the track.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Bar {
    pub thumb_start: u16,
    pub thumb_len: u16,
}

/// The right padding column of a bordered pane. A narrow pane returns an empty rect.
pub fn padding_track(area: Rect, y: u16, height: u16) -> Rect {
    if area.width < 4 || height == 0 {
        return Rect::default();
    }
    Rect {
        x: area.x.saturating_add(area.width.saturating_sub(2)),
        y,
        width: 1,
        height,
    }
}

/// `None` when the content fits in the viewport or the track has no rows.
pub fn bar(content: usize, viewport: usize, origin: usize, track: u16) -> Option<Bar> {
    if track == 0 || viewport == 0 || content <= viewport {
        return None;
    }
    let max_origin = content - viewport;
    let origin = origin.min(max_origin);
    let thumb_len = u16::try_from(usize::from(track) * viewport / content)
        .unwrap_or(u16::MAX)
        .max(1)
        .min(track);
    let span = track - thumb_len;
    let thumb_start = if max_origin == 0 || span == 0 {
        0
    } else {
        u16::try_from(origin * usize::from(span) / max_origin).unwrap_or(span)
    };
    Some(Bar {
        thumb_start,
        thumb_len,
    })
}

/// First visible row. The window stays put until `cursor` leaves it.
pub fn reveal(origin: usize, cursor: usize, len: usize, viewport: usize) -> usize {
    if viewport == 0 || len <= viewport {
        return 0;
    }
    let max_origin = len - viewport;
    let origin = origin.min(max_origin);
    if len == 0 {
        return 0;
    }
    let cursor = cursor.min(len - 1);
    if cursor < origin {
        cursor
    } else if cursor >= origin + viewport {
        cursor + 1 - viewport
    } else {
        origin
    }
}

/// First visible row for a tail-pinned list. A pinned window sticks to the newest rows.
pub fn pinned_origin(stored: usize, pinned: bool, len: usize, viewport: usize) -> usize {
    if viewport == 0 || len <= viewport {
        return 0;
    }
    if pinned {
        len - viewport
    } else {
        stored.min(len - viewport)
    }
}

/// Relative thumb drag. One track row past the thumb maps onto the content that sits outside the viewport.
pub fn origin_after_drag(
    origin: usize,
    content: usize,
    viewport: usize,
    track: u16,
    delta_rows: i32,
) -> usize {
    if viewport == 0 || content <= viewport || track == 0 {
        return 0;
    }
    let max_origin = content - viewport;
    let thumb = bar(content, viewport, origin, track)
        .map(|bar| bar.thumb_len)
        .unwrap_or(1);
    let span = track.saturating_sub(thumb).max(1);
    let delta =
        i64::from(delta_rows) * i64::try_from(max_origin).unwrap_or(i64::MAX) / i64::from(span);
    let next = i64::try_from(origin.min(max_origin)).unwrap_or(0) + delta;
    if next <= 0 {
        0
    } else {
        usize::try_from(next).unwrap_or(max_origin).min(max_origin)
    }
}

/// A press outside the thumb moves one viewport toward that row. A press on the thumb stays put.
pub fn page_toward(
    origin: usize,
    content: usize,
    viewport: usize,
    track: u16,
    row_in_track: u16,
) -> usize {
    let Some(bar) = bar(content, viewport, origin, track) else {
        return 0;
    };
    let max_origin = content.saturating_sub(viewport);
    let row = row_in_track.min(track.saturating_sub(1));
    if row < bar.thumb_start {
        origin.min(max_origin).saturating_sub(viewport)
    } else if row >= bar.thumb_start.saturating_add(bar.thumb_len) {
        origin.saturating_add(viewport).min(max_origin)
    } else {
        origin.min(max_origin)
    }
}

/// Draws the track and thumb, and records the hit target. A list that fits records nothing.
pub fn paint(
    frame: &mut Frame<'_>,
    track: Rect,
    content: usize,
    viewport: usize,
    origin: usize,
    pointer: &mut Pointer,
    id: ScrollId,
) {
    let Some(bar) = bar(content, viewport, origin, track.height) else {
        return;
    };
    if track.width == 0 {
        return;
    }
    let palette = theme();
    let buffer = frame.buffer_mut();
    for row in 0..track.height {
        let on_thumb =
            row >= bar.thumb_start && row < bar.thumb_start.saturating_add(bar.thumb_len);
        let (symbol, style) = if on_thumb {
            ("█", palette.title)
        } else {
            ("│", palette.muted)
        };
        let cell = &mut buffer[(track.x, track.y.saturating_add(row))];
        cell.set_symbol(symbol);
        cell.set_style(style);
    }
    pointer.vscroll(
        track,
        id,
        ScrollMetrics {
            len: content,
            viewport,
            origin,
            track,
        },
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hidden_when_the_content_fits() {
        assert!(bar(10, 10, 0, 8).is_none());
        assert!(bar(4, 10, 0, 8).is_none());
        assert!(bar(20, 10, 0, 0).is_none());
    }

    #[test]
    fn thumb_rests_at_the_ends() {
        let top = bar(100, 10, 0, 10).unwrap();
        assert_eq!(top.thumb_start, 0);
        assert!(top.thumb_len >= 1);
        let bottom = bar(100, 10, 90, 10).unwrap();
        assert_eq!(bottom.thumb_start + bottom.thumb_len, 10);
    }

    #[test]
    fn thumb_is_at_least_one_row() {
        let thumb = bar(10_000, 1, 0, 5).unwrap();
        assert_eq!(thumb.thumb_len, 1);
    }

    #[test]
    fn reveal_follows_the_edges() {
        assert_eq!(reveal(0, 0, 10, 30), 0);
        assert_eq!(reveal(0, 0, 100, 10), 0);
        assert_eq!(reveal(0, 9, 100, 10), 0);
        assert_eq!(reveal(0, 10, 100, 10), 1);
        assert_eq!(reveal(0, 50, 100, 10), 41);
        assert_eq!(reveal(11, 20, 100, 10), 11);
        assert_eq!(reveal(11, 10, 100, 10), 10);
        assert_eq!(reveal(0, 99, 100, 10), 90);
    }

    #[test]
    fn drag_moves_by_the_span() {
        assert_eq!(origin_after_drag(0, 100, 10, 10, 4), 40);
        assert_eq!(origin_after_drag(40, 100, 10, 10, -100), 0);
        assert_eq!(origin_after_drag(0, 100, 10, 10, 100), 90);
    }

    #[test]
    fn page_toward_moves_one_viewport() {
        assert_eq!(page_toward(0, 100, 10, 10, 0), 0);
        assert_eq!(page_toward(0, 100, 10, 10, 5), 10);
        assert_eq!(page_toward(20, 100, 10, 10, 0), 10);
    }

    #[test]
    fn a_pinned_window_sticks_to_the_tail() {
        assert_eq!(pinned_origin(0, true, 50, 10), 40);
        assert_eq!(pinned_origin(0, true, 80, 10), 70);
        assert_eq!(pinned_origin(39, false, 80, 10), 39);
    }
}
