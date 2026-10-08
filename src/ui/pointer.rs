//! Regions recorded while drawing, so a click or wheel event can name a widget.

use ratatui::layout::Rect;

use crate::app::{ScrollId, Target};
use crate::panels::{Edge, Split};

/// Length, viewport, and track recorded while a scrollbar was painted.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ScrollMetrics {
    pub len: usize,
    pub viewport: usize,
    pub origin: usize,
    pub track: Rect,
}

#[derive(Default)]
pub struct Pointer {
    regions: Vec<(Rect, Target)>,
    metrics: Vec<(ScrollId, ScrollMetrics)>,
}

impl Pointer {
    pub fn clear(&mut self) {
        self.regions.clear();
        self.metrics.clear();
    }

    pub fn at(&self, column: u16, row: u16) -> Option<Target> {
        self.regions
            .iter()
            .rev()
            .find(|(area, _)| contains(*area, column, row))
            .map(|(_, target)| *target)
    }

    pub fn region(&self, matches: impl Fn(Target) -> bool) -> Option<Rect> {
        self.regions
            .iter()
            .rev()
            .find(|(_, target)| matches(*target))
            .map(|(area, _)| *area)
    }

    pub fn scroll(&mut self, area: Rect) {
        self.push(area, Target::Scroll);
    }

    /// The list body. The wheel uses this. A later row target still wins a click.
    pub fn wheel(&mut self, area: Rect, id: ScrollId) {
        self.push(area, Target::Wheel(id));
    }

    /// The scrollbar track. Also stores the metrics from the frame that painted it.
    pub fn vscroll(&mut self, area: Rect, id: ScrollId, metrics: ScrollMetrics) {
        self.metrics.push((id, metrics));
        self.push(area, Target::VScroll(id));
    }

    pub fn metrics(&self, id: ScrollId) -> Option<ScrollMetrics> {
        self.metrics
            .iter()
            .rev()
            .find(|(found, _)| *found == id)
            .map(|(_, metrics)| *metrics)
    }

    pub fn open_view(&mut self, area: Rect, view: crate::app::View) {
        self.push(area, Target::OpenView(view));
    }

    pub fn select(&mut self, area: Rect, index: usize) {
        self.push(area, Target::Select(index));
    }

    pub fn column(&mut self, area: Rect, column: u8, index: usize) {
        self.push(area, Target::Column(column, index));
    }

    pub fn quality(&mut self, area: Rect, column: u8, index: usize) {
        self.push(area, Target::Quality(column, index));
    }

    pub fn seek(&mut self, area: Rect) {
        self.push(area, Target::Seek);
    }

    pub fn control(&mut self, area: Rect, target: Target) {
        self.push(area, target);
    }

    pub fn focus_query(&mut self, area: Rect) {
        self.push(area, Target::FocusQuery);
    }

    pub fn cycle_mode(&mut self, area: Rect) {
        self.push(area, Target::CycleMode);
    }

    pub fn cycle_filter(&mut self, area: Rect, index: u8) {
        self.push(area, Target::CycleFilter(index));
    }

    pub fn open_help(&mut self, area: Rect) {
        self.push(area, Target::OpenHelp);
    }

    pub fn toggle_log(&mut self, area: Rect) {
        self.push(area, Target::ToggleLog);
    }

    pub fn picker(&mut self, area: Rect, index: usize) {
        self.push(area, Target::Picker(index));
    }

    pub fn picker_stay(&mut self, area: Rect) {
        self.push(area, Target::PickerStay);
    }

    pub fn setting(&mut self, area: Rect, index: usize) {
        self.push(area, Target::Setting(index));
    }

    pub fn resize(&mut self, area: Rect, split: Split, edge: Edge) {
        self.push(edge_band(area, edge), Target::Resize(split, edge));
    }

    fn push(&mut self, area: Rect, target: Target) {
        if area.width == 0 || area.height == 0 {
            return;
        }
        self.regions.push((area, target));
    }
}

fn edge_band(area: Rect, edge: Edge) -> Rect {
    match edge {
        Edge::East => Rect {
            x: area.x.saturating_add(area.width.saturating_sub(1)),
            y: area.y,
            width: 1,
            height: area.height,
        },
        Edge::West => Rect {
            x: area.x,
            y: area.y,
            width: 1,
            height: area.height,
        },
        Edge::South => Rect {
            x: area.x,
            y: area.y.saturating_add(area.height.saturating_sub(1)),
            width: area.width,
            height: 1,
        },
        Edge::North => Rect {
            x: area.x,
            y: area.y,
            width: area.width,
            height: 1,
        },
    }
}

fn contains(area: Rect, column: u16, row: u16) -> bool {
    column >= area.x
        && column < area.x.saturating_add(area.width)
        && row >= area.y
        && row < area.y.saturating_add(area.height)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::View;

    #[test]
    fn later_regions_cover_earlier_ones() {
        let mut pointer = Pointer::default();
        let area = Rect {
            x: 0,
            y: 0,
            width: 10,
            height: 4,
        };
        pointer.scroll(area);
        pointer.select(
            Rect {
                x: 0,
                y: 2,
                width: 10,
                height: 1,
            },
            3,
        );
        assert_eq!(pointer.at(1, 1), Some(Target::Scroll));
        assert_eq!(pointer.at(1, 2), Some(Target::Select(3)));
        assert_eq!(
            pointer.region(|target| target == Target::OpenView(View::Search)),
            None
        );
    }

    #[test]
    fn a_scrollbar_covers_the_row_under_it() {
        use crate::app::ScrollId;

        let mut pointer = Pointer::default();
        let area = Rect {
            x: 0,
            y: 0,
            width: 10,
            height: 4,
        };
        pointer.select(area, 3);
        let track = Rect {
            x: 9,
            y: 0,
            width: 1,
            height: 4,
        };
        let metrics = ScrollMetrics {
            len: 40,
            viewport: 4,
            origin: 0,
            track,
        };
        pointer.vscroll(track, ScrollId::List, metrics);
        assert_eq!(pointer.at(1, 2), Some(Target::Select(3)));
        assert_eq!(pointer.at(9, 2), Some(Target::VScroll(ScrollId::List)));
        assert_eq!(pointer.metrics(ScrollId::List), Some(metrics));
    }
}
