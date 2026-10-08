//! View models for the terminal. These are display records, not protocol messages.

use crate::protocol::UserStatus;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Direction {
    Download,
    Upload,
}

impl Direction {
    pub fn glyph(self) -> &'static str {
        match self {
            Self::Download => "↓",
            Self::Upload => "↑",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TransferState {
    Queued,
    Transferring,
    Paused,
    Finished,
    /// The peer's own filter rejected the file. The wire reason is `Filtered`.
    Filtered,
    Banned,
    TooManyFiles,
    TooManyMegabytes,
    /// The peer reported an internal error. Another user is tried.
    InternalError,
    /// The peer will not queue the file because a transfer with them just failed.
    /// Another user is tried.
    LastTryFailed,
    PendingShutdown,
    /// The peer refused with a reason that is shown as they wrote it.
    Refused,
    UserLoggedOff,
    ConnectionClosed,
    ConnectionTimeout,
    FileNotShared,
    Cancelled,
    /// The album could not be fetched from that peer. A new search takes over.
    Failed,
}

impl TransferState {
    pub fn label(self) -> &'static str {
        match self {
            Self::Queued => "Queued",
            Self::Transferring => "Transferring",
            Self::Paused => "Paused",
            Self::Finished => "Finished",
            Self::Filtered => "Blocked by filter",
            Self::Banned => "Banned",
            Self::TooManyFiles => "Too many files",
            Self::TooManyMegabytes => "Too many megabytes",
            Self::InternalError => "Internal error",
            Self::LastTryFailed => "Last try failed",
            Self::PendingShutdown => "Pending shutdown",
            Self::Refused => "Refused",
            Self::UserLoggedOff => "User logged off",
            Self::ConnectionClosed => "Connection closed",
            Self::ConnectionTimeout => "Connection timeout",
            Self::FileNotShared => "File not shared",
            Self::Cancelled => "Cancelled",
            Self::Failed => "Failed",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Transfer {
    pub direction: Direction,
    pub user: String,
    pub path: String,
    pub size: u64,
    pub done: u64,
    pub speed: u64,
    pub queue: Option<u32>,
    pub state: TransferState,
    /// The peer's words when `state` is [`TransferState::Refused`].
    pub detail: String,
}

impl Transfer {
    pub fn percent(&self) -> u8 {
        if self.size == 0 {
            return 0;
        }
        let pct = self.done.saturating_mul(100) / self.size;
        u8::try_from(pct.min(100)).unwrap_or(100)
    }

    /// The state column. A refusal uses the peer's own words when they sent some.
    /// A failed album with no free slot says `Failed - Unavailable`.
    pub fn status(&self) -> String {
        if self.state == TransferState::Refused && !self.detail.is_empty() {
            self.detail.clone()
        } else if self.state == TransferState::Failed && !self.detail.is_empty() {
            format!("Failed - {}", self.detail)
        } else {
            self.state.label().to_owned()
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SearchMode {
    Global,
    Room,
    Buddy,
    User,
    Wishlist,
}

impl SearchMode {
    pub const ALL: [Self; 5] = [
        Self::Global,
        Self::Room,
        Self::Buddy,
        Self::User,
        Self::Wishlist,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Self::Global => "global",
            Self::Room => "room",
            Self::Buddy => "buddy",
            Self::User => "user",
            Self::Wishlist => "wishlist",
        }
    }

    pub fn cycle(self) -> Self {
        let index = Self::ALL.iter().position(|mode| *mode == self).unwrap_or(0);
        Self::ALL[(index + 1) % Self::ALL.len()]
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SearchHit {
    pub user: String,
    pub path: String,
    pub size: u64,
    pub bitrate: Option<u32>,
    pub duration: Option<u32>,
    pub bit_depth: Option<u32>,
    pub sample_rate: Option<u32>,
    pub queue: u32,
    pub free_slot: bool,
    /// The peer's reported upload speed, in bytes per second.
    pub upload_speed: u32,
    pub country: String,
}

const LOSSLESS: &[&str] = &[
    "flac", "wav", "wave", "aif", "aiff", "aifc", "alac", "ape", "wv", "tak", "tta", "shn", "dsf",
    "dff", "dsdiff", "pcm", "caf",
];

const LOSSY: &[&str] = &[
    "mp3", "mp2", "aac", "m4a", "m4b", "mp4", "ogg", "oga", "opus", "wma", "mpc", "ac3", "dts",
    "ra", "amr", "webm",
];

/// Lossless container, lossy container, or an extension this client does not classify.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ContainerClass {
    Lossless,
    Lossy,
    Other,
}

/// Extension class used by search ranking and library quality.
///
/// `.m4a` is lossy unless a bit depth is present, which is how ALAC is marked.
pub fn container_class(extension: Option<&str>, bit_depth: Option<u32>) -> ContainerClass {
    let Some(ext) = extension else {
        return if bit_depth.is_some() {
            ContainerClass::Lossless
        } else {
            ContainerClass::Other
        };
    };
    if known(ext, LOSSLESS) {
        return ContainerClass::Lossless;
    }
    if ext.eq_ignore_ascii_case("m4a") && bit_depth.is_some() {
        return ContainerClass::Lossless;
    }
    if known(ext, LOSSY) {
        return ContainerClass::Lossy;
    }
    if bit_depth.is_some() {
        ContainerClass::Lossless
    } else {
        ContainerClass::Other
    }
}

impl SearchHit {
    /// `0` is lossless, `1` is anything else, `2` is a lossy extension.
    ///
    /// `.m4a` is lossy unless a bit depth is present, which is how ALAC is marked.
    pub fn format_rank(&self) -> u8 {
        match container_class(self.extension(), self.bit_depth) {
            ContainerClass::Lossless => 0,
            ContainerClass::Other => 1,
            ContainerClass::Lossy => 2,
        }
    }

    pub fn extension(&self) -> Option<&str> {
        file_extension(&self.path)
    }
}

fn file_extension(path: &str) -> Option<&str> {
    let name = path.rsplit(['\\', '/']).next().unwrap_or(path);
    let (stem, ext) = name.rsplit_once('.')?;
    if stem.is_empty() || ext.is_empty() {
        None
    } else {
        Some(ext)
    }
}

fn known(ext: &str, list: &[&str]) -> bool {
    list.iter().any(|item| ext.eq_ignore_ascii_case(item))
}

/// One search table row. A folder stands for every file that shares that directory.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SearchLine {
    pub user: String,
    pub path: String,
    pub folder: bool,
    pub expanded: bool,
    pub nested: bool,
    pub count: u32,
    pub size: u64,
    pub bitrate: Option<u32>,
    pub duration: Option<u32>,
    pub queue: u32,
    pub free_slot: bool,
    pub country: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BrowseRow {
    pub depth: u8,
    pub name: String,
    pub size: Option<u64>,
    pub bitrate: Option<u32>,
    pub duration: Option<u32>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Room {
    pub name: String,
    pub users: u32,
    pub joined: bool,
}

/// Someone listed in a room. Status and counts come from the join reply when the server sent them.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RoomPerson {
    pub name: String,
    pub status: Option<u32>,
    pub country: String,
    pub files: Option<u32>,
    pub dirs: Option<u32>,
}

impl RoomPerson {
    pub fn named(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            status: None,
            country: String::new(),
            files: None,
            dirs: None,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ChatLine {
    pub room: String,
    pub time: String,
    pub user: String,
    pub text: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum UserList {
    Buddy,
    Ignored,
    Banned,
}

impl UserList {
    pub fn label(self) -> &'static str {
        match self {
            Self::Buddy => "buddy",
            Self::Ignored => "ignored",
            Self::Banned => "banned",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ListedUser {
    pub list: UserList,
    pub name: String,
    pub status: UserStatus,
    pub country: String,
    pub note: String,
    pub notify: bool,
    pub prioritized: bool,
    pub trusted: bool,
    pub last_seen: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ShareGroup {
    pub name: &'static str,
    pub access: &'static str,
    pub folders: u32,
    pub files: u32,
}

pub struct SettingGroup {
    pub name: &'static str,
    pub items: &'static [(&'static str, &'static str)],
}

pub fn share_groups() -> [ShareGroup; 3] {
    [
        ShareGroup {
            name: "public",
            access: "anyone on the network",
            folders: 0,
            files: 0,
        },
        ShareGroup {
            name: "buddy",
            access: "buddy list only",
            folders: 0,
            files: 0,
        },
        ShareGroup {
            name: "trusted",
            access: "trusted buddies only",
            folders: 0,
            files: 0,
        },
    ]
}

pub fn setting_groups() -> &'static [SettingGroup] {
    &[
        SettingGroup {
            name: "network",
            items: &[
                ("server", "server.slsknet.org:2242"),
                ("listen port", "2234"),
                ("bind", "all interfaces"),
                ("upnp / nat-pmp", "off"),
                ("connection", "not started"),
            ],
        },
        SettingGroup {
            name: "transfers",
            items: &[
                ("upload slots", "2"),
                ("queue", "fifo, privileged first"),
                ("upload limit", "unlimited"),
                ("download limit", "unlimited"),
                ("incomplete folder", "unset"),
            ],
        },
        SettingGroup {
            name: "shares",
            items: &[
                ("public folders", "none"),
                ("buddy folders", "none"),
                ("trusted folders", "none"),
                ("rescan", "not connected"),
            ],
        },
        SettingGroup {
            name: "search",
            items: &[
                ("minimum characters", "3"),
                ("maximum results", "300"),
                ("history", "on"),
                ("wishlist interval", "server defined"),
            ],
        },
        SettingGroup {
            name: "chat",
            items: &[
                ("auto-away", "15 min"),
                ("auto-reply", "empty"),
                ("auto-join", "none"),
                ("logging", "off"),
            ],
        },
        SettingGroup {
            name: "interface",
            items: &[("theme", "btop"), ("mouse", "on"), ("graph", "braille")],
        },
    ]
}
