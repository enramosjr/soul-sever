//! Session-only panel sizes. A drag grows one panel outward and can return only
//! as far as that panel's original size. The neighbor gives up the space.

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Edge {
    North,
    South,
    West,
    East,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Split {
    ChatRooms,
    ChatMembers,
    SearchFilters,
    SearchQuery,
    UsersDetail,
    SettingsNav,
    DashMeters,
    DashDownload,
    DashNetwork,
    SharesLeft,
    SharesRight,
}

impl Split {
    fn index(self) -> usize {
        match self {
            Split::ChatRooms => 0,
            Split::ChatMembers => 1,
            Split::SearchFilters => 2,
            Split::SearchQuery => 3,
            Split::UsersDetail => 4,
            Split::SettingsNav => 5,
            Split::DashMeters => 6,
            Split::DashDownload => 7,
            Split::DashNetwork => 8,
            Split::SharesLeft => 9,
            Split::SharesRight => 10,
        }
    }
}

const SPLITS: usize = 11;
const CHAT_ROOMS: u16 = 24;
const CHAT_MEMBERS: u16 = 20;
const CHAT_TEXT_MIN: u16 = 12;
const SIDE_MIN: u16 = 8;
const SEARCH_FILTERS: u16 = 28;
const FILTER_MIN: u16 = 12;
const RESULTS_MIN: u16 = 16;
const SEARCH_QUERY: u16 = 5;
const QUERY_MIN: u16 = 3;
const BELOW_MIN: u16 = 4;
const USERS_DETAIL: u16 = 36;
const DETAIL_MIN: u16 = 16;
const LIST_MIN: u16 = 16;
const SETTINGS_NAV: u16 = 22;
const NAV_MIN: u16 = 12;
const FIELDS_MIN: u16 = 20;
const DASH_METERS: u16 = 11;
const METER_MIN: u16 = 4;
const DASH_NETWORK: u16 = 36;
const NETWORK_MIN: u16 = 16;
const METER_PAIR_MIN: u16 = 20;
const COLUMN_MIN: u16 = 10;

#[derive(Clone, Copy, Debug)]
struct Drag {
    split: Split,
    edge: Edge,
    origin_x: u16,
    origin_y: u16,
    origin_bias: i16,
}

#[derive(Clone, Debug, Default)]
pub struct Panels {
    bias: [i16; SPLITS],
    drag: Option<Drag>,
}

impl Panels {
    pub fn begin(&mut self, split: Split, edge: Edge, x: u16, y: u16) {
        if !edge.adjusts(split) {
            return;
        }
        self.drag = Some(Drag {
            split,
            edge,
            origin_x: x,
            origin_y: y,
            origin_bias: self.bias[split.index()],
        });
    }

    pub fn drag_to(&mut self, x: u16, y: u16) -> bool {
        let Some(drag) = self.drag else {
            return false;
        };
        let growth = drag.edge.growth(drag.origin_x, drag.origin_y, x, y);
        let outward = drag.edge.grows_primary(drag.split);
        self.bias[drag.split.index()] = apply_bias(drag.origin_bias, growth, outward);
        true
    }

    pub fn end(&mut self) {
        self.drag = None;
    }

    pub fn chat(&self, span: u16) -> (u16, u16) {
        let rooms = want(
            CHAT_ROOMS,
            self.bias[Split::ChatRooms.index()],
            SIDE_MIN,
            span,
        );
        let members = want(
            CHAT_MEMBERS,
            self.bias[Split::ChatMembers.index()],
            SIDE_MIN,
            span,
        );
        fit_pair(span, rooms, members, SIDE_MIN, SIDE_MIN, CHAT_TEXT_MIN)
    }

    pub fn search_query(&self, span: u16) -> u16 {
        let height = want(
            SEARCH_QUERY,
            self.bias[Split::SearchQuery.index()],
            QUERY_MIN,
            span,
        );
        height.min(span.saturating_sub(BELOW_MIN).max(QUERY_MIN))
    }

    pub fn search_filters(&self, span: u16) -> u16 {
        let width = want(
            SEARCH_FILTERS,
            self.bias[Split::SearchFilters.index()],
            FILTER_MIN,
            span,
        );
        width.min(span.saturating_sub(RESULTS_MIN).max(FILTER_MIN))
    }

    pub fn users_detail(&self, span: u16) -> u16 {
        let width = want(
            USERS_DETAIL,
            self.bias[Split::UsersDetail.index()],
            DETAIL_MIN,
            span,
        );
        width.min(span.saturating_sub(LIST_MIN).max(DETAIL_MIN))
    }

    pub fn settings_nav(&self, span: u16) -> u16 {
        let width = want(
            SETTINGS_NAV,
            self.bias[Split::SettingsNav.index()],
            NAV_MIN,
            span,
        );
        width.min(span.saturating_sub(FIELDS_MIN).max(NAV_MIN))
    }

    pub fn dash_meters(&self, span: u16) -> u16 {
        let height = want(
            DASH_METERS,
            self.bias[Split::DashMeters.index()],
            METER_MIN,
            span,
        );
        height.min(span.saturating_sub(BELOW_MIN).max(METER_MIN))
    }

    /// Download, upload, and network widths across `span`.
    pub fn dash_columns(&self, span: u16) -> (u16, u16, u16) {
        let network = want(
            DASH_NETWORK,
            self.bias[Split::DashNetwork.index()],
            NETWORK_MIN,
            span,
        );
        let network = network.min(span.saturating_sub(METER_PAIR_MIN).max(NETWORK_MIN));
        let rest = span.saturating_sub(network);
        let half = rest / 2;
        let mut download = want(
            half,
            self.bias[Split::DashDownload.index()],
            COLUMN_MIN,
            rest,
        );
        if download.saturating_add(COLUMN_MIN) > rest {
            download = rest.saturating_sub(COLUMN_MIN);
        }
        let upload = rest.saturating_sub(download);
        (download, upload, network)
    }

    /// Left, middle, and right share columns.
    pub fn shares(&self, span: u16) -> (u16, u16, u16) {
        let third = span / 3;
        let mut left = want(
            third,
            self.bias[Split::SharesLeft.index()],
            COLUMN_MIN,
            span,
        );
        let mut right = want(
            third,
            self.bias[Split::SharesRight.index()],
            COLUMN_MIN,
            span,
        );
        let mut overflow = left
            .saturating_add(right)
            .saturating_add(COLUMN_MIN)
            .saturating_sub(span);
        let take = overflow.min(left.saturating_sub(COLUMN_MIN));
        left = left.saturating_sub(take);
        overflow = overflow.saturating_sub(take);
        let take = overflow.min(right.saturating_sub(COLUMN_MIN));
        right = right.saturating_sub(take);
        let middle = span.saturating_sub(left).saturating_sub(right);
        (left, middle, right)
    }
}

impl Edge {
    fn growth(self, origin_x: u16, origin_y: u16, x: u16, y: u16) -> i32 {
        match self {
            Edge::East => i32::from(x) - i32::from(origin_x),
            Edge::West => i32::from(origin_x) - i32::from(x),
            Edge::South => i32::from(y) - i32::from(origin_y),
            Edge::North => i32::from(origin_y) - i32::from(y),
        }
    }

    fn grows_primary(self, split: Split) -> bool {
        matches!(
            (split, self),
            (Split::ChatRooms, Edge::East)
                | (Split::ChatMembers, Edge::West)
                | (Split::SearchFilters, Edge::East)
                | (Split::SearchQuery, Edge::South)
                | (Split::UsersDetail, Edge::West)
                | (Split::SettingsNav, Edge::East)
                | (Split::DashMeters, Edge::South)
                | (Split::DashDownload, Edge::East)
                | (Split::DashNetwork, Edge::West)
                | (Split::SharesLeft, Edge::East)
                | (Split::SharesRight, Edge::West)
        )
    }

    fn adjusts(self, split: Split) -> bool {
        self.grows_primary(split)
            || matches!(
                (split, self),
                (Split::ChatRooms, Edge::West)
                    | (Split::ChatMembers, Edge::East)
                    | (Split::SearchFilters, Edge::West)
                    | (Split::SearchQuery, Edge::North)
                    | (Split::UsersDetail, Edge::East)
                    | (Split::SettingsNav, Edge::West)
                    | (Split::DashMeters, Edge::North)
                    | (Split::DashDownload, Edge::West)
                    | (Split::DashNetwork, Edge::East)
                    | (Split::SharesLeft, Edge::West)
                    | (Split::SharesRight, Edge::East)
            )
    }
}

/// `growth` is how far the pointer moved in the edge's expand direction.
/// `outward` grows the panel that owns the default. The other edge grows the neighbor.
/// Moving back stops at the original size.
pub fn apply_bias(origin: i16, growth: i32, outward: bool) -> i16 {
    let delta = if outward { growth } else { -growth };
    let next = i32::from(origin).saturating_add(delta);
    let kept = if growth >= 0 {
        if outward {
            next.max(i32::from(origin))
        } else {
            next.min(i32::from(origin))
        }
    } else if outward {
        if origin <= 0 {
            i32::from(origin)
        } else {
            next.clamp(0, i32::from(origin))
        }
    } else if origin >= 0 {
        i32::from(origin)
    } else {
        next.clamp(i32::from(origin), 0)
    };
    kept.clamp(i32::from(i16::MIN), i32::from(i16::MAX)) as i16
}

fn want(default: u16, bias: i16, min: u16, span: u16) -> u16 {
    let raw = i32::from(default).saturating_add(i32::from(bias));
    let max = i32::from(span.max(min));
    raw.clamp(i32::from(min), max) as u16
}

fn fit_pair(
    span: u16,
    mut first: u16,
    mut second: u16,
    first_min: u16,
    second_min: u16,
    gap_min: u16,
) -> (u16, u16) {
    let need = first.saturating_add(second).saturating_add(gap_min);
    if need <= span {
        return (first, second);
    }
    let mut overflow = need - span;
    let take = overflow.min(first.saturating_sub(first_min));
    first = first.saturating_sub(take);
    overflow = overflow.saturating_sub(take);
    let take = overflow.min(second.saturating_sub(second_min));
    second = second.saturating_sub(take);
    (first, second)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn east_edge_expands_chat_rooms_and_stops_at_the_default() {
        let mut panels = Panels::default();
        panels.begin(Split::ChatRooms, Edge::East, 23, 2);
        assert!(panels.drag_to(33, 2));
        assert_eq!(panels.chat(120), (34, 20));
        assert!(panels.drag_to(0, 2));
        assert_eq!(panels.chat(120), (24, 20));
    }

    #[test]
    fn west_edge_widens_the_transcript_and_stops_at_the_default() {
        let mut panels = Panels::default();
        panels.begin(Split::ChatRooms, Edge::West, 24, 2);
        assert!(panels.drag_to(14, 2));
        let (rooms, members) = panels.chat(120);
        assert_eq!((rooms, members), (14, 20));
        assert_eq!(120 - rooms - members, 86);
        assert!(panels.drag_to(80, 2));
        let (rooms, members) = panels.chat(120);
        assert_eq!((rooms, members), (24, 20));
        assert_eq!(120 - rooms - members, 76);
    }

    #[test]
    fn an_east_drag_does_not_shrink_rooms_that_are_already_under_the_default() {
        let mut panels = Panels::default();
        panels.begin(Split::ChatRooms, Edge::West, 24, 2);
        panels.drag_to(14, 2);
        panels.end();
        assert_eq!(panels.chat(120).0, 14);

        panels.begin(Split::ChatRooms, Edge::East, 13, 2);
        panels.drag_to(0, 2);
        assert_eq!(panels.chat(120).0, 14);
        panels.end();

        panels.begin(Split::ChatRooms, Edge::East, 13, 2);
        panels.drag_to(19, 2);
        assert_eq!(panels.chat(120).0, 20);
        panels.drag_to(40, 2);
        assert_eq!(panels.chat(120).0, 41);
    }

    #[test]
    fn members_grow_west_and_the_transcript_grows_east_only_back_to_the_default() {
        let mut panels = Panels::default();
        panels.begin(Split::ChatMembers, Edge::West, 100, 2);
        panels.drag_to(90, 2);
        assert_eq!(panels.chat(120), (24, 30));
        panels.drag_to(140, 2);
        assert_eq!(panels.chat(120), (24, 20));
        panels.end();

        panels.begin(Split::ChatMembers, Edge::East, 99, 2);
        panels.drag_to(109, 2);
        assert_eq!(panels.chat(120).1, 10);
        panels.drag_to(0, 2);
        assert_eq!(panels.chat(120).1, 20);
    }

    #[test]
    fn query_grows_south_and_the_results_grow_north_only_back_to_the_default() {
        let mut panels = Panels::default();
        panels.begin(Split::SearchQuery, Edge::South, 0, 5);
        panels.drag_to(0, 8);
        assert_eq!(panels.search_query(37), 8);
        panels.drag_to(0, 0);
        assert_eq!(panels.search_query(37), 5);
        panels.end();

        panels.begin(Split::SearchQuery, Edge::North, 0, 6);
        panels.drag_to(0, 4);
        assert_eq!(panels.search_query(37), 3);
        panels.drag_to(0, 40);
        assert_eq!(panels.search_query(37), 5);
    }

    #[test]
    fn filters_users_settings_dashboard_and_shares_follow_the_same_floor() {
        let mut panels = Panels::default();
        panels.begin(Split::SearchFilters, Edge::East, 28, 1);
        panels.drag_to(36, 1);
        assert_eq!(panels.search_filters(120), 36);
        panels.drag_to(0, 1);
        assert_eq!(panels.search_filters(120), 28);
        panels.end();

        panels.begin(Split::UsersDetail, Edge::West, 80, 1);
        panels.drag_to(70, 1);
        assert_eq!(panels.users_detail(120), 46);
        panels.drag_to(200, 1);
        assert_eq!(panels.users_detail(120), 36);
        panels.end();

        panels.begin(Split::SettingsNav, Edge::East, 21, 1);
        panels.drag_to(27, 1);
        assert_eq!(panels.settings_nav(120), 28);
        panels.drag_to(0, 1);
        assert_eq!(panels.settings_nav(120), 22);
        panels.end();

        panels.begin(Split::DashMeters, Edge::South, 0, 11);
        panels.drag_to(0, 15);
        assert_eq!(panels.dash_meters(37), 15);
        panels.drag_to(0, 0);
        assert_eq!(panels.dash_meters(37), 11);
        panels.end();

        panels.begin(Split::DashNetwork, Edge::West, 84, 1);
        panels.drag_to(74, 1);
        assert_eq!(panels.dash_columns(120).2, 46);
        panels.drag_to(200, 1);
        assert_eq!(panels.dash_columns(120).2, 36);
        panels.end();

        panels.begin(Split::DashDownload, Edge::East, 40, 1);
        panels.drag_to(48, 1);
        let (download, upload, network) = panels.dash_columns(120);
        assert_eq!(network, 36);
        assert_eq!(download, 50);
        assert_eq!(upload, 34);
        panels.drag_to(0, 1);
        let (download, upload, _) = panels.dash_columns(120);
        assert_eq!(download, upload);
        panels.end();

        let (left, middle, right) = Panels::default().shares(90);
        assert_eq!((left, middle, right), (30, 30, 30));
        panels.begin(Split::SharesLeft, Edge::East, 29, 1);
        panels.drag_to(39, 1);
        assert_eq!(panels.shares(90).0, 40);
        panels.drag_to(0, 1);
        assert_eq!(panels.shares(90).0, 30);
        panels.end();

        panels.begin(Split::SharesRight, Edge::West, 60, 1);
        panels.drag_to(50, 1);
        assert_eq!(panels.shares(90).2, 40);
        panels.drag_to(90, 1);
        assert_eq!(panels.shares(90), (30, 30, 30));
    }
}
