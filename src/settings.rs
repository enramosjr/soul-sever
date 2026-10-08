//! Every saved option, in the order the settings screen shows it.
//!
//! The shares screen edits share folders. The users screen edits buddy flags.
//! This module is the editor for the remaining fields, and it also shows those
//! two lists so their current values are visible here.

use crate::config::{Buddy, Config, QueueMode, WordPair};

pub const SECTIONS: [&str; 7] = [
    "account",
    "network",
    "transfers",
    "shares",
    "search",
    "chat",
    "people",
];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Field {
    Username,
    Password,
    ServerHost,
    ServerPort,
    ListenPort,
    BindAddress,
    Upnp,
    UpnpGateway,
    UploadSlots,
    QueueMode,
    UploadLimit,
    DownloadLimit,
    IncompleteDir,
    DownloadDir,
    QueueFiles,
    QueueMegabytes,
    PublicShares,
    BuddyShares,
    TrustedShares,
    Exclude,
    MinSearchChars,
    MaxResults,
    Wishlist,
    AutoAway,
    AutoReply,
    AutoJoin,
    LogRooms,
    LogPrivate,
    RoomLogDir,
    PrivateLogDir,
    Censor,
    ReplaceWords,
    Buddies,
    Ignored,
    Banned,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Text,
    Secret,
    Port,
    Count,
    Slots,
    Minutes,
    Kib,
    Bool,
    Queue,
    Folder,
    /// Enter opens a list of this machine's interfaces.
    Interface,
    Names,
    Pairs,
    /// Enter leaves settings for the screen that already edits this.
    Jump,
}

pub struct Spec {
    pub label: &'static str,
    pub field: Field,
    pub kind: Kind,
    /// Saved immediately. The listen socket and the login are unchanged until restart.
    pub restart: bool,
}

pub fn rows(section: usize) -> &'static [Spec] {
    match section {
        0 => ACCOUNT,
        1 => NETWORK,
        2 => TRANSFERS,
        3 => SHARES,
        4 => SEARCH,
        5 => CHAT,
        _ => PEOPLE,
    }
}

const ACCOUNT: &[Spec] = &[
    spec("username", Field::Username, Kind::Text, true),
    spec("password", Field::Password, Kind::Secret, true),
];

const NETWORK: &[Spec] = &[
    spec("server", Field::ServerHost, Kind::Text, true),
    spec("server port", Field::ServerPort, Kind::Port, true),
    spec("listen port", Field::ListenPort, Kind::Port, true),
    spec("bind address", Field::BindAddress, Kind::Interface, true),
    spec("upnp / nat-pmp", Field::Upnp, Kind::Bool, true),
    spec("upnp gateway", Field::UpnpGateway, Kind::Text, true),
];

const TRANSFERS: &[Spec] = &[
    spec("upload slots", Field::UploadSlots, Kind::Slots, false),
    spec("queue", Field::QueueMode, Kind::Queue, false),
    spec("upload limit", Field::UploadLimit, Kind::Kib, false),
    spec("download limit", Field::DownloadLimit, Kind::Kib, false),
    spec(
        "incomplete folder",
        Field::IncompleteDir,
        Kind::Folder,
        false,
    ),
    spec("download folder", Field::DownloadDir, Kind::Folder, false),
    spec("queue file limit", Field::QueueFiles, Kind::Count, false),
    spec("queue megabytes", Field::QueueMegabytes, Kind::Count, false),
];

const SHARES: &[Spec] = &[
    spec("public folders", Field::PublicShares, Kind::Jump, false),
    spec("buddy folders", Field::BuddyShares, Kind::Jump, false),
    spec("trusted folders", Field::TrustedShares, Kind::Jump, false),
    spec("exclude", Field::Exclude, Kind::Names, false),
];

const SEARCH: &[Spec] = &[
    spec(
        "minimum characters",
        Field::MinSearchChars,
        Kind::Count,
        false,
    ),
    spec("maximum results", Field::MaxResults, Kind::Count, false),
    spec("wishlist", Field::Wishlist, Kind::Names, false),
];

const CHAT: &[Spec] = &[
    spec("auto-away minutes", Field::AutoAway, Kind::Minutes, false),
    spec("auto-reply", Field::AutoReply, Kind::Text, false),
    spec("auto-join", Field::AutoJoin, Kind::Names, false),
    spec("log rooms", Field::LogRooms, Kind::Bool, false),
    spec("log private", Field::LogPrivate, Kind::Bool, false),
    spec("room log folder", Field::RoomLogDir, Kind::Folder, false),
    spec(
        "private log folder",
        Field::PrivateLogDir,
        Kind::Folder,
        false,
    ),
    spec("censor", Field::Censor, Kind::Pairs, false),
    spec("replace words", Field::ReplaceWords, Kind::Pairs, false),
];

const PEOPLE: &[Spec] = &[
    spec("buddies", Field::Buddies, Kind::Names, false),
    spec("ignored", Field::Ignored, Kind::Names, false),
    spec("banned", Field::Banned, Kind::Names, false),
];

const fn spec(label: &'static str, field: Field, kind: Kind, restart: bool) -> Spec {
    Spec {
        label,
        field,
        kind,
        restart,
    }
}

/// One row in the bind-address popup.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BindChoice {
    pub label: String,
    pub address: String,
}

/// Interfaces on this machine, plus `all interfaces` for an empty bind address.
pub fn bind_choices() -> Vec<BindChoice> {
    let found = if_addrs::get_if_addrs().map(|list| {
        list.into_iter()
            .map(|iface| {
                let ip = iface.ip();
                (iface.name, ip)
            })
            .collect::<Vec<_>>()
    });
    bind_rows(found.unwrap_or_default())
}

fn bind_rows(found: Vec<(String, std::net::IpAddr)>) -> Vec<BindChoice> {
    let mut listed: Vec<(String, String)> = found
        .into_iter()
        .filter_map(|(name, ip)| usable_ip(ip).map(|address| (name, address)))
        .collect();
    listed.sort();
    listed.dedup();
    let mut rows = vec![BindChoice {
        label: "all interfaces".to_owned(),
        address: String::new(),
    }];
    for (name, address) in listed {
        rows.push(BindChoice {
            label: format!("{name}  {address}"),
            address,
        });
    }
    rows
}

fn usable_ip(ip: std::net::IpAddr) -> Option<String> {
    match ip {
        std::net::IpAddr::V4(ip) if !ip.is_unspecified() && !ip.is_link_local() => {
            Some(ip.to_string())
        }
        std::net::IpAddr::V6(ip) if !ip.is_unspecified() && !ip.is_unicast_link_local() => {
            Some(ip.to_string())
        }
        _ => None,
    }
}

/// Values the running session can apply without logging in again.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LivePrefs {
    pub upload_slots: u16,
    pub queue_mode: QueueMode,
    pub upload_limit_kib: Option<u32>,
    pub download_limit_kib: Option<u32>,
    pub incomplete_dir: String,
    pub download_dir: String,
    pub queue_file_limit: u32,
    pub queue_megabytes: u32,
    pub wishlist: Vec<String>,
    pub min_search_chars: u32,
    pub max_results: u32,
    pub auto_reply: String,
    pub censor: Vec<WordPair>,
    pub replace_words: Vec<WordPair>,
    pub log_rooms: bool,
    pub log_private: bool,
    pub room_log_dir: String,
    pub private_log_dir: String,
    pub auto_away_minutes: u32,
}

impl LivePrefs {
    pub fn from_config(config: &Config) -> Self {
        Self {
            upload_slots: config.upload_slots,
            queue_mode: config.queue_mode,
            upload_limit_kib: config.upload_limit_kib,
            download_limit_kib: config.download_limit_kib,
            incomplete_dir: config.incomplete_dir.clone(),
            download_dir: config.download_dir.clone(),
            queue_file_limit: config.queue_file_limit,
            queue_megabytes: config.queue_megabytes,
            wishlist: config.wishlist.clone(),
            min_search_chars: config.min_search_chars,
            max_results: config.max_results,
            auto_reply: config.auto_reply.clone(),
            censor: config.censor.clone(),
            replace_words: config.replace_words.clone(),
            log_rooms: config.log_rooms,
            log_private: config.log_private,
            room_log_dir: config.room_log_dir.clone(),
            private_log_dir: config.private_log_dir.clone(),
            auto_away_minutes: config.auto_away_minutes,
        }
    }
}

pub fn display(field: Field, config: &Config) -> String {
    match field {
        Field::Username => blank(&config.username),
        Field::Password => {
            if config.password().is_empty() {
                "empty".to_owned()
            } else {
                "set".to_owned()
            }
        }
        Field::ServerHost => config.server_host.clone(),
        Field::ServerPort => config.server_port.to_string(),
        Field::ListenPort => config.listen_port.to_string(),
        Field::BindAddress => {
            if config.bind_address.is_empty() {
                "all interfaces".to_owned()
            } else {
                config.bind_address.clone()
            }
        }
        Field::Upnp => on_off(config.upnp),
        Field::UpnpGateway => blank(&config.upnp_gateway),
        Field::UploadSlots => config.upload_slots.to_string(),
        Field::QueueMode => match config.queue_mode {
            QueueMode::Fifo => "fifo".to_owned(),
            QueueMode::RoundRobin => "round-robin".to_owned(),
        },
        Field::UploadLimit => kib(config.upload_limit_kib),
        Field::DownloadLimit => kib(config.download_limit_kib),
        Field::IncompleteDir => blank(&config.incomplete_dir),
        Field::DownloadDir => blank(&config.download_dir),
        Field::QueueFiles => config.queue_file_limit.to_string(),
        Field::QueueMegabytes => config.queue_megabytes.to_string(),
        Field::PublicShares => paths(&config.shares.public),
        Field::BuddyShares => paths(&config.shares.buddy),
        Field::TrustedShares => paths(&config.shares.trusted),
        Field::Exclude => names(&config.shares.exclude),
        Field::MinSearchChars => config.min_search_chars.to_string(),
        Field::MaxResults => config.max_results.to_string(),
        Field::Wishlist => names(&config.wishlist),
        Field::AutoAway => config.auto_away_minutes.to_string(),
        Field::AutoReply => blank(&config.auto_reply),
        Field::AutoJoin => names(&config.auto_join),
        Field::LogRooms => on_off(config.log_rooms),
        Field::LogPrivate => on_off(config.log_private),
        Field::RoomLogDir => blank(&config.room_log_dir),
        Field::PrivateLogDir => blank(&config.private_log_dir),
        Field::Censor => pairs(&config.censor),
        Field::ReplaceWords => pairs(&config.replace_words),
        Field::Buddies => names(
            &config
                .buddies
                .iter()
                .map(|buddy| buddy.name.clone())
                .collect::<Vec<_>>(),
        ),
        Field::Ignored => names(&config.ignored),
        Field::Banned => names(&config.banned),
    }
}

pub fn edit_seed(field: Field, config: &Config) -> String {
    match field {
        Field::Password => String::new(),
        Field::BindAddress => config.bind_address.clone(),
        Field::UploadLimit => config
            .upload_limit_kib
            .map(|n| n.to_string())
            .unwrap_or_default(),
        Field::DownloadLimit => config
            .download_limit_kib
            .map(|n| n.to_string())
            .unwrap_or_default(),
        Field::PublicShares | Field::BuddyShares | Field::TrustedShares => String::new(),
        other => {
            let shown = display(other, config);
            if shown == "—" || shown == "none" || shown == "unlimited" || shown == "all interfaces"
            {
                String::new()
            } else {
                shown
            }
        }
    }
}

pub fn toggle(field: Field, config: &mut Config) -> bool {
    match field {
        Field::Upnp => config.upnp = !config.upnp,
        Field::LogRooms => config.log_rooms = !config.log_rooms,
        Field::LogPrivate => config.log_private = !config.log_private,
        _ => return false,
    }
    true
}

pub fn cycle(field: Field, config: &mut Config) -> bool {
    if field != Field::QueueMode {
        return false;
    }
    config.queue_mode = match config.queue_mode {
        QueueMode::Fifo => QueueMode::RoundRobin,
        QueueMode::RoundRobin => QueueMode::Fifo,
    };
    true
}

pub fn set_folder(field: Field, config: &mut Config, path: String) {
    match field {
        Field::IncompleteDir => config.incomplete_dir = path,
        Field::DownloadDir => config.download_dir = path,
        Field::RoomLogDir => config.room_log_dir = path,
        Field::PrivateLogDir => config.private_log_dir = path,
        _ => {}
    }
}

pub fn clear(field: Field, config: &mut Config) -> bool {
    match field {
        Field::UploadLimit => config.upload_limit_kib = None,
        Field::DownloadLimit => config.download_limit_kib = None,
        Field::IncompleteDir => config.incomplete_dir.clear(),
        Field::DownloadDir => config.download_dir.clear(),
        Field::RoomLogDir => config.room_log_dir.clear(),
        Field::PrivateLogDir => config.private_log_dir.clear(),
        Field::BindAddress => config.bind_address.clear(),
        Field::UpnpGateway => config.upnp_gateway.clear(),
        Field::AutoReply => config.auto_reply.clear(),
        Field::Exclude => config.shares.exclude.clear(),
        Field::Wishlist => config.wishlist.clear(),
        Field::AutoJoin => config.auto_join.clear(),
        Field::Censor => config.censor.clear(),
        Field::ReplaceWords => config.replace_words.clear(),
        Field::Ignored => config.ignored.clear(),
        Field::Banned => config.banned.clear(),
        Field::Password => config.set_password(String::new()),
        _ => return false,
    }
    true
}

pub fn apply_text(field: Field, config: &mut Config, text: &str) -> Result<(), String> {
    let text = text.trim();
    match field {
        Field::Username => config.username = text.to_owned(),
        Field::Password => config.set_password(text.to_owned()),
        Field::ServerHost => {
            if text.is_empty() {
                return Err("server must not be empty".to_owned());
            }
            config.server_host = text.to_owned();
        }
        Field::ServerPort => config.server_port = parse_port(text)?,
        Field::ListenPort => config.listen_port = parse_port(text)?,
        Field::BindAddress => {
            if !text.is_empty() && text.parse::<std::net::IpAddr>().is_err() {
                return Err("bind address must be an IP, or empty for every interface".to_owned());
            }
            config.bind_address = text.to_owned();
        }
        Field::UpnpGateway => {
            if !text.is_empty() && text.parse::<std::net::SocketAddr>().is_err() {
                return Err("upnp gateway must be host:port, or empty".to_owned());
            }
            config.upnp_gateway = text.to_owned();
        }
        Field::UploadSlots => config.upload_slots = parse_slots(text)?,
        Field::UploadLimit => config.upload_limit_kib = parse_kib(text)?,
        Field::DownloadLimit => config.download_limit_kib = parse_kib(text)?,
        Field::QueueFiles => config.queue_file_limit = parse_count(text)?,
        Field::QueueMegabytes => config.queue_megabytes = parse_count(text)?,
        Field::MinSearchChars => config.min_search_chars = parse_count(text)?,
        Field::MaxResults => config.max_results = parse_count(text)?,
        Field::AutoAway => config.auto_away_minutes = parse_minutes(text)?,
        Field::AutoReply => config.auto_reply = text.to_owned(),
        Field::Exclude => config.shares.exclude = split_names(text),
        Field::Wishlist => config.wishlist = split_names(text),
        Field::AutoJoin => config.auto_join = split_names(text),
        Field::Censor => config.censor = split_pairs(text)?,
        Field::ReplaceWords => config.replace_words = split_pairs(text)?,
        Field::Buddies => set_buddies(config, &split_names(text)),
        Field::Ignored => config.ignored = split_names(text),
        Field::Banned => config.banned = split_names(text),
        Field::IncompleteDir => config.incomplete_dir = text.to_owned(),
        Field::DownloadDir => config.download_dir = text.to_owned(),
        Field::RoomLogDir => config.room_log_dir = text.to_owned(),
        Field::PrivateLogDir => config.private_log_dir = text.to_owned(),
        Field::Upnp | Field::QueueMode | Field::LogRooms | Field::LogPrivate => {}
        Field::PublicShares | Field::BuddyShares | Field::TrustedShares => {}
    }
    Ok(())
}

fn set_buddies(config: &mut Config, names: &[String]) {
    let old = config.buddies.clone();
    config.buddies = names
        .iter()
        .map(|name| {
            old.iter()
                .find(|buddy| buddy.name == *name)
                .cloned()
                .unwrap_or_else(|| Buddy {
                    name: name.clone(),
                    note: String::new(),
                    notify: false,
                    prioritized: false,
                    trusted: false,
                })
        })
        .collect();
}

fn parse_port(text: &str) -> Result<u16, String> {
    let port: u16 = text
        .parse()
        .map_err(|_| "port must be 1–65535".to_owned())?;
    if port == 0 {
        return Err("port must be 1–65535".to_owned());
    }
    Ok(port)
}

fn parse_count(text: &str) -> Result<u32, String> {
    let value: u32 = text
        .parse()
        .map_err(|_| "value must be at least 1".to_owned())?;
    if value == 0 {
        return Err("value must be at least 1".to_owned());
    }
    Ok(value)
}

fn parse_slots(text: &str) -> Result<u16, String> {
    text.parse()
        .map_err(|_| "upload slots must be a number".to_owned())
}

fn parse_minutes(text: &str) -> Result<u32, String> {
    text.parse()
        .map_err(|_| "minutes must be a number, 0 stays online".to_owned())
}

fn parse_kib(text: &str) -> Result<Option<u32>, String> {
    if text.is_empty() || text.eq_ignore_ascii_case("unlimited") {
        return Ok(None);
    }
    let value: u32 = text
        .parse()
        .map_err(|_| "limit must be a number of KiB/s, or empty for unlimited".to_owned())?;
    if value == 0 {
        return Err("limit must be at least 1 KiB/s, or empty for unlimited".to_owned());
    }
    Ok(Some(value))
}

fn split_names(text: &str) -> Vec<String> {
    text.split(',')
        .map(str::trim)
        .filter(|name| !name.is_empty())
        .map(str::to_owned)
        .collect()
}

fn split_pairs(text: &str) -> Result<Vec<WordPair>, String> {
    if text.is_empty() {
        return Ok(Vec::new());
    }
    let mut pairs = Vec::new();
    for item in text.split(',') {
        let item = item.trim();
        if item.is_empty() {
            continue;
        }
        let Some((from, to)) = item.split_once('=') else {
            return Err("use from=to, separated by commas".to_owned());
        };
        if from.trim().is_empty() {
            return Err("the word to replace must not be empty".to_owned());
        }
        pairs.push(WordPair {
            from: from.trim().to_owned(),
            to: to.trim().to_owned(),
        });
    }
    Ok(pairs)
}

fn blank(value: &str) -> String {
    if value.trim().is_empty() {
        "—".to_owned()
    } else {
        value.to_owned()
    }
}

fn names(values: &[String]) -> String {
    if values.is_empty() {
        "none".to_owned()
    } else {
        values.join(", ")
    }
}

fn paths(values: &[std::path::PathBuf]) -> String {
    if values.is_empty() {
        "none · shares view".to_owned()
    } else {
        format!(
            "{} · shares view",
            values
                .iter()
                .map(|path| path.to_string_lossy().into_owned())
                .collect::<Vec<_>>()
                .join(", ")
        )
    }
}

fn pairs(values: &[WordPair]) -> String {
    if values.is_empty() {
        "none".to_owned()
    } else {
        values
            .iter()
            .map(|pair| format!("{}={}", pair.from, pair.to))
            .collect::<Vec<_>>()
            .join(", ")
    }
}

fn kib(value: Option<u32>) -> String {
    match value {
        Some(n) => format!("{n} KiB/s"),
        None => "unlimited".to_owned(),
    }
}

fn on_off(value: bool) -> String {
    if value {
        "on".to_owned()
    } else {
        "off".to_owned()
    }
}

#[cfg(test)]
mod tests {
    use std::net::{Ipv4Addr, Ipv6Addr};

    use super::bind_rows;

    #[test]
    fn bind_rows_keep_real_addresses_and_drop_link_local() {
        let rows = bind_rows(vec![
            ("lo".to_owned(), Ipv4Addr::LOCALHOST.into()),
            ("eth0".to_owned(), Ipv4Addr::new(192, 168, 1, 20).into()),
            ("eth0".to_owned(), Ipv4Addr::UNSPECIFIED.into()),
            ("eth0".to_owned(), Ipv4Addr::new(169, 254, 1, 1).into()),
            (
                "eth0".to_owned(),
                Ipv6Addr::new(0xfe80, 0, 0, 0, 0, 0, 0, 1).into(),
            ),
            ("wlan0".to_owned(), Ipv4Addr::new(10, 0, 0, 8).into()),
        ]);
        let labels: Vec<&str> = rows.iter().map(|row| row.label.as_str()).collect();
        assert_eq!(
            labels,
            vec![
                "all interfaces",
                "eth0  192.168.1.20",
                "lo  127.0.0.1",
                "wlan0  10.0.0.8",
            ]
        );
        assert_eq!(rows[2].address, "127.0.0.1");
        assert!(rows[0].address.is_empty());
    }
}
