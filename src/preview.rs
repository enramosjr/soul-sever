//! Local rows for tests. The binary does not open this catalog.
//!
//! Every row is prefixed `preview-` or titled as a preview. Nothing here came from the network.

use crate::model::{
    BrowseRow, ChatLine, Direction, ListedUser, Room, SearchHit, Transfer, TransferState, UserList,
};
use crate::protocol::UserStatus;

pub fn transfers() -> Vec<Transfer> {
    vec![
        transfer(
            Direction::Download,
            "preview-alice",
            "preview/miles-davis/so-what.flac",
            28_311_552,
            17_825_792,
            1_572_864,
            None,
            TransferState::Transferring,
        ),
        transfer(
            Direction::Download,
            "preview-bob",
            "preview/talking-heads/once-in-a-lifetime.mp3",
            8_388_608,
            0,
            0,
            Some(4),
            TransferState::Queued,
        ),
        transfer(
            Direction::Upload,
            "preview-chen",
            "preview/local/kind-of-blue/flamenco-sketches.flac",
            41_943_040,
            9_437_184,
            786_432,
            None,
            TransferState::Transferring,
        ),
        transfer(
            Direction::Upload,
            "preview-dana",
            "preview/local/remain-in-light/born-under-punches.flac",
            33_554_432,
            0,
            0,
            Some(2),
            TransferState::Queued,
        ),
        transfer(
            Direction::Download,
            "preview-erin",
            "preview/missing/side-a.mp3",
            4_194_304,
            1_048_576,
            0,
            None,
            TransferState::UserLoggedOff,
        ),
    ]
}

pub fn search_hits() -> Vec<SearchHit> {
    vec![
        hit(
            "preview-alice",
            "preview/miles-davis/so-what.flac",
            28_311_552,
            Some(900),
            Some(562),
            0,
            true,
            "US",
        ),
        hit(
            "preview-bob",
            "preview/talking-heads/once-in-a-lifetime.mp3",
            8_388_608,
            Some(320),
            Some(221),
            4,
            false,
            "DE",
        ),
        hit(
            "preview-chen",
            "preview/aphex-twin/xtal.flac",
            21_000_000,
            Some(850),
            Some(295),
            1,
            true,
            "UK",
        ),
    ]
}

pub fn browse_rows() -> Vec<BrowseRow> {
    vec![
        row(0, "preview", None, None, None),
        row(1, "miles-davis", None, None, None),
        row(2, "so-what.flac", Some(28_311_552), Some(900), Some(562)),
        row(1, "talking-heads", None, None, None),
        row(
            2,
            "once-in-a-lifetime.mp3",
            Some(8_388_608),
            Some(320),
            Some(221),
        ),
    ]
}

pub fn rooms() -> Vec<Room> {
    vec![
        Room {
            name: "preview-music".to_owned(),
            users: 128,
            joined: true,
        },
        Room {
            name: "preview-electronic".to_owned(),
            users: 42,
            joined: false,
        },
        Room {
            name: "preview-jazz".to_owned(),
            users: 17,
            joined: false,
        },
    ]
}

pub fn chat_lines() -> Vec<ChatLine> {
    vec![
        line(
            "preview-music",
            "18:02",
            "preview-alice",
            "this transcript is local sample text",
        ),
        line(
            "preview-music",
            "18:03",
            "preview-bob",
            "the room is not joined on the server",
        ),
        line(
            "preview-electronic",
            "18:11",
            "preview-chen",
            "electronic room sample",
        ),
    ]
}

pub fn users() -> Vec<ListedUser> {
    vec![
        ListedUser {
            list: UserList::Buddy,
            name: "preview-alice".to_owned(),
            status: UserStatus::Online,
            country: "US".to_owned(),
            note: "sample buddy".to_owned(),
            notify: true,
            prioritized: true,
            trusted: false,
            last_seen: "—".to_owned(),
        },
        ListedUser {
            list: UserList::Buddy,
            name: "preview-bob".to_owned(),
            status: UserStatus::Away,
            country: "DE".to_owned(),
            note: "sample, not watched".to_owned(),
            notify: false,
            prioritized: false,
            trusted: true,
            last_seen: "—".to_owned(),
        },
        ListedUser {
            list: UserList::Ignored,
            name: "preview-spam".to_owned(),
            status: UserStatus::Offline,
            country: "—".to_owned(),
            note: "sample ignore".to_owned(),
            notify: false,
            prioritized: false,
            trusted: false,
            last_seen: "never".to_owned(),
        },
    ]
}

/// Deterministic preview throughput in bytes per second.
pub fn rate_at(tick: u64, phase: u64) -> u64 {
    let t = tick.wrapping_add(phase);
    let tri = t % 40;
    let tri = if tri < 20 { tri } else { 40 - tri };
    let burst = if t % 53 < 4 { 4_000_000 } else { 0 };
    tri * 350_000 + burst + 200_000
}

// Display fixtures list every column of a transfer or search row.
#[allow(clippy::too_many_arguments)]
fn transfer(
    direction: Direction,
    user: &str,
    path: &str,
    size: u64,
    done: u64,
    speed: u64,
    queue: Option<u32>,
    state: TransferState,
) -> Transfer {
    Transfer {
        direction,
        user: user.to_owned(),
        path: path.to_owned(),
        size,
        done,
        speed,
        queue,
        state,
        detail: String::new(),
    }
}

#[allow(clippy::too_many_arguments)]
fn hit(
    user: &str,
    path: &str,
    size: u64,
    bitrate: Option<u32>,
    duration: Option<u32>,
    queue: u32,
    free_slot: bool,
    country: &str,
) -> SearchHit {
    SearchHit {
        user: user.to_owned(),
        path: path.to_owned(),
        size,
        bitrate,
        duration,
        bit_depth: None,
        sample_rate: None,
        queue,
        free_slot,
        upload_speed: 0,
        country: country.to_owned(),
    }
}

fn row(
    depth: u8,
    name: &str,
    size: Option<u64>,
    bitrate: Option<u32>,
    duration: Option<u32>,
) -> BrowseRow {
    BrowseRow {
        depth,
        name: name.to_owned(),
        size,
        bitrate,
        duration,
    }
}

fn line(room: &str, time: &str, user: &str, text: &str) -> ChatLine {
    ChatLine {
        room: room.to_owned(),
        time: time.to_owned(),
        user: user.to_owned(),
        text: text.to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn preview_rate_is_deterministic_and_varies() {
        assert_eq!(rate_at(3, 1), rate_at(3, 1));
        assert_ne!(rate_at(0, 0), rate_at(10, 0));
    }
}
