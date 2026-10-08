//! Client-to-server messages this crate can encode, and the server replies it can read.
//!
//! Same-numbered messages often have a different layout in each direction. Encoders here
//! build what a client sends. Decoders read what the server sends.

use std::fmt;
use std::net::Ipv4Addr;

use md5::{Digest, Md5};

use super::codes::server;
use super::peer::ConnType;
use super::wire::{Reader, Writer, encode_frame};

/// Major version for unofficial clients. Nicotine+ reserves major version 160.
pub const MAJOR_VERSION: u32 = 177;
/// Soul Sever minor version, reported with [`MAJOR_VERSION`].
pub const MINOR_VERSION: u32 = 1;

/// Soulseek presence values. `0` offline, `1` away, `2` online.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(i32)]
pub enum UserStatus {
    Offline = 0,
    Away = 1,
    Online = 2,
}

impl UserStatus {
    pub fn as_i32(self) -> i32 {
        self as i32
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Offline => "offline",
            Self::Away => "away",
            Self::Online => "online",
        }
    }

    /// `0` offline, `1` away, `2` online. Any other value is offline.
    pub fn from_wire(value: u32) -> Self {
        match value {
            1 => Self::Away,
            2 => Self::Online,
            _ => Self::Offline,
        }
    }
}

/// Login request. The password is omitted from [`Debug`].
#[derive(Clone, PartialEq, Eq)]
pub struct LoginRequest {
    username: String,
    password: String,
    major: u32,
    minor: u32,
}

impl LoginRequest {
    pub fn new(username: impl Into<String>, password: impl Into<String>) -> Self {
        Self {
            username: username.into(),
            password: password.into(),
            major: MAJOR_VERSION,
            minor: MINOR_VERSION,
        }
    }

    pub fn username(&self) -> &str {
        &self.username
    }

    pub fn major(&self) -> u32 {
        self.major
    }

    pub fn minor(&self) -> u32 {
        self.minor
    }

    pub fn payload(&self) -> Result<Vec<u8>, super::ProtocolError> {
        let mut writer = Writer::new();
        writer.string(&self.username)?;
        writer.string(&self.password)?;
        writer.u32(self.major);
        writer.string(&login_digest(&self.username, &self.password))?;
        writer.u32(self.minor);
        Ok(writer.finish())
    }
}

impl fmt::Debug for LoginRequest {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("LoginRequest")
            .field("username", &self.username)
            .field("password", &"********")
            .field("major", &self.major)
            .field("minor", &self.minor)
            .finish()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LoginResponse {
    Success {
        banner: String,
        ip: Ipv4Addr,
        password_digest: String,
        supporter: bool,
    },
    Failure {
        reason: String,
    },
}

/// Server `GetUserStatus` (code 7): username, status `u32`, privileged bool.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UserStatusNotice {
    pub user: String,
    pub status: u32,
    pub privileged: bool,
}

pub fn decode_user_status(payload: &[u8]) -> Result<UserStatusNotice, super::ProtocolError> {
    let mut reader = Reader::new(payload);
    Ok(UserStatusNotice {
        user: reader.string()?,
        status: reader.u32()?,
        privileged: reader.bool()?,
    })
}

/// Server reply to `WatchUser` (code 5).
///
/// Nicotine+ reads the username and an exists bool. When more bytes follow, it
/// reads status, average speed, upload count, an unknown `u32`, file count, and
/// directory count. A country string is present only when the user is online.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WatchUserReply {
    pub user: String,
    pub exists: bool,
    pub status: u32,
    pub country: String,
}

pub fn decode_watch_user(payload: &[u8]) -> Result<WatchUserReply, super::ProtocolError> {
    let mut reader = Reader::new(payload);
    let user = reader.string()?;
    let exists = reader.bool()?;
    if !exists || reader.rest().is_empty() {
        return Ok(WatchUserReply {
            user,
            exists,
            status: 0,
            country: String::new(),
        });
    }
    let status = reader.u32()?;
    let _average_speed = reader.u32()?;
    let _uploads = reader.u32()?;
    let _unknown = reader.u32()?;
    let _files = reader.u32()?;
    let _dirs = reader.u32()?;
    let country = if reader.rest().is_empty() {
        String::new()
    } else {
        reader.string()?
    };
    Ok(WatchUserReply {
        user,
        exists,
        status,
        country,
    })
}

/// Server `GetUserStats` (code 36): username, then five `u32`s.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UserStats {
    pub user: String,
    pub average_speed: u32,
    pub uploads: u32,
    pub files: u32,
    pub dirs: u32,
}

pub fn decode_user_stats(payload: &[u8]) -> Result<UserStats, super::ProtocolError> {
    let mut reader = Reader::new(payload);
    Ok(UserStats {
        user: reader.string()?,
        average_speed: reader.u32()?,
        uploads: reader.u32()?,
        files: {
            let _unknown = reader.u32()?;
            reader.u32()?
        },
        dirs: reader.u32()?,
    })
}

/// One recommendation and its rating. A negative rating is an unrecommendation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RatedItem {
    pub name: String,
    pub rating: i32,
}

pub fn decode_recommendations(payload: &[u8]) -> Result<Vec<RatedItem>, super::ProtocolError> {
    let mut reader = Reader::new(payload);
    let mut items = read_rated(&mut reader)?;
    if !reader.rest().is_empty() {
        items.extend(read_rated(&mut reader)?);
    }
    Ok(items)
}

fn read_rated(reader: &mut Reader<'_>) -> Result<Vec<RatedItem>, super::ProtocolError> {
    let count = reader.u32()?;
    let mut items = Vec::new();
    for _ in 0..count {
        items.push(RatedItem {
            name: reader.string()?,
            rating: reader.i32()?,
        });
    }
    Ok(items)
}

/// Server `UserInterests` (code 57): username, liked strings, hated strings.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UserInterests {
    pub user: String,
    pub likes: Vec<String>,
    pub hates: Vec<String>,
}

pub fn decode_user_interests(payload: &[u8]) -> Result<UserInterests, super::ProtocolError> {
    let mut reader = Reader::new(payload);
    Ok(UserInterests {
        user: reader.string()?,
        likes: read_strings(&mut reader)?,
        hates: read_strings(&mut reader)?,
    })
}

fn read_strings(reader: &mut Reader<'_>) -> Result<Vec<String>, super::ProtocolError> {
    let count = reader.u32()?;
    let mut names = Vec::new();
    for _ in 0..count {
        names.push(reader.string()?);
    }
    Ok(names)
}

/// Server `SimilarUsers` (code 110): count, then username and `u32` rating pairs.
pub fn decode_similar_users(payload: &[u8]) -> Result<Vec<RatedItem>, super::ProtocolError> {
    let mut reader = Reader::new(payload);
    let count = reader.u32()?;
    let mut users = Vec::new();
    for _ in 0..count {
        users.push(RatedItem {
            name: reader.string()?,
            rating: i32::try_from(reader.u32()?)
                .map_err(|_| super::ProtocolError::IntegerOverflow)?,
        });
    }
    Ok(users)
}

pub fn decode_login_response(payload: &[u8]) -> Result<LoginResponse, super::ProtocolError> {
    let mut reader = Reader::new(payload);
    if !reader.bool()? {
        return Ok(LoginResponse::Failure {
            reason: reader.string()?,
        });
    }
    Ok(LoginResponse::Success {
        banner: reader.string()?,
        ip: reader.ipv4()?,
        password_digest: reader.string()?,
        supporter: reader.bool()?,
    })
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileSearchRequest {
    pub token: u32,
    pub query: String,
}

impl FileSearchRequest {
    pub fn payload(&self) -> Result<Vec<u8>, super::ProtocolError> {
        let mut writer = Writer::new();
        writer.u32(self.token);
        writer.string(&sanitize_search_query(&self.query))?;
        Ok(writer.finish())
    }
}

/// File search as delivered by the server (username, token, query).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IncomingFileSearch {
    pub username: String,
    pub token: u32,
    pub query: String,
}

pub fn decode_incoming_file_search(
    payload: &[u8],
) -> Result<IncomingFileSearch, super::ProtocolError> {
    let mut reader = Reader::new(payload);
    Ok(IncomingFileSearch {
        username: reader.string()?,
        token: reader.u32()?,
        query: reader.string()?,
    })
}

/// One parent the server offers in `PossibleParents`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PossibleParent {
    pub username: String,
    pub ip: Ipv4Addr,
    pub port: u32,
}

pub fn decode_possible_parents(
    payload: &[u8],
) -> Result<Vec<PossibleParent>, super::ProtocolError> {
    let mut reader = Reader::new(payload);
    let count = reader.u32()?;
    let mut parents = Vec::new();
    for _ in 0..count {
        parents.push(PossibleParent {
            username: reader.string()?,
            ip: reader.ipv4()?,
            port: reader.u32()?,
        });
    }
    Ok(parents)
}

/// Server code 104. The wishlist rotates one query after this arrives.
pub fn decode_wishlist_interval(payload: &[u8]) -> Result<u32, super::ProtocolError> {
    Reader::new(payload).u32()
}

/// Obsolete server delivery of `RoomSearch`: username, token, query.
pub fn decode_incoming_room_search(
    payload: &[u8],
) -> Result<IncomingFileSearch, super::ProtocolError> {
    decode_incoming_file_search(payload)
}

/// `UserSearch` is username, token, query in both directions.
pub fn decode_user_search(payload: &[u8]) -> Result<IncomingFileSearch, super::ProtocolError> {
    decode_incoming_file_search(payload)
}

/// `WishlistSearch` inherits `FileSearch` parsing: username, token, query.
pub fn decode_wishlist_search(payload: &[u8]) -> Result<IncomingFileSearch, super::ProtocolError> {
    decode_incoming_file_search(payload)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SetStatus {
    pub status: UserStatus,
}

/// Server reply to [`ClientMessage::GetPeerAddress`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PeerAddress {
    pub username: String,
    pub ip: Ipv4Addr,
    pub port: u32,
    pub unknown: u32,
    pub obfuscated_port: u16,
}

pub fn decode_peer_address(payload: &[u8]) -> Result<PeerAddress, super::ProtocolError> {
    let mut reader = Reader::new(payload);
    Ok(PeerAddress {
        username: reader.string()?,
        ip: reader.ipv4()?,
        port: reader.u32()?,
        unknown: reader.u32()?,
        obfuscated_port: reader.u16()?,
    })
}

/// Server delivery of `ConnectToPeer`: the peer we should pierce.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IncomingConnect {
    pub username: String,
    pub conn_type: ConnType,
    pub ip: Ipv4Addr,
    pub port: u32,
    pub token: u32,
    pub privileged: bool,
    pub unknown: u32,
    pub obfuscated_port: u32,
}

/// Server code 1001. Nicotine+ reads only the token; the username is what we send.
pub fn decode_cant_connect(payload: &[u8]) -> Result<u32, super::ProtocolError> {
    Reader::new(payload).u32()
}

pub fn decode_incoming_connect(payload: &[u8]) -> Result<IncomingConnect, super::ProtocolError> {
    let mut reader = Reader::new(payload);
    Ok(IncomingConnect {
        username: reader.string()?,
        conn_type: ConnType::parse(&reader.string()?)?,
        ip: reader.ipv4()?,
        port: reader.u32()?,
        token: reader.u32()?,
        privileged: reader.bool()?,
        unknown: reader.u32()?,
        obfuscated_port: reader.u32()?,
    })
}

/// Messages a Soul Sever client can encode today.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ClientMessage {
    Login(LoginRequest),
    SetWaitPort(u32),
    GetPeerAddress(String),
    ConnectToPeer {
        token: u32,
        username: String,
        conn_type: ConnType,
    },
    /// We could not dial the address in an inbound `ConnectToPeer`.
    CantConnectToPeer {
        token: u32,
        username: String,
    },
    FileSearch(FileSearchRequest),
    RoomSearch {
        room: String,
        token: u32,
        query: String,
    },
    UserSearch {
        username: String,
        token: u32,
        query: String,
    },
    WishlistSearch(FileSearchRequest),
    HaveNoParent(bool),
    AcceptChildren(bool),
    BranchLevel(u32),
    BranchRoot(String),
    SetStatus(SetStatus),
    Ping,
    WatchUser(String),
    GetUserStats(String),
    AddThingILike(String),
    UserInterests(String),
    Recommendations,
    SimilarUsers,
    /// Public folder count, then public file count. Buddy and trusted files stay out.
    SharedFoldersFiles {
        folders: u32,
        files: u32,
    },
}

impl ClientMessage {
    pub fn code(&self) -> u32 {
        match self {
            Self::Login(_) => server::LOGIN,
            Self::SetWaitPort(_) => server::SET_WAIT_PORT,
            Self::GetPeerAddress(_) => server::GET_PEER_ADDRESS,
            Self::ConnectToPeer { .. } => server::CONNECT_TO_PEER,
            Self::CantConnectToPeer { .. } => server::CANT_CONNECT_TO_PEER,
            Self::FileSearch(_) => server::FILE_SEARCH,
            Self::RoomSearch { .. } => server::ROOM_SEARCH,
            Self::UserSearch { .. } => server::USER_SEARCH,
            Self::WishlistSearch(_) => server::WISHLIST_SEARCH,
            Self::HaveNoParent(_) => server::HAVE_NO_PARENT,
            Self::AcceptChildren(_) => server::ACCEPT_CHILDREN,
            Self::BranchLevel(_) => server::BRANCH_LEVEL,
            Self::BranchRoot(_) => server::BRANCH_ROOT,
            Self::SetStatus(_) => server::SET_STATUS,
            Self::Ping => server::SERVER_PING,
            Self::WatchUser(_) => server::WATCH_USER,
            Self::GetUserStats(_) => server::GET_USER_STATS,
            Self::AddThingILike(_) => server::ADD_THING_I_LIKE,
            Self::UserInterests(_) => server::USER_INTERESTS,
            Self::Recommendations => server::RECOMMENDATIONS,
            Self::SimilarUsers => server::SIMILAR_USERS,
            Self::SharedFoldersFiles { .. } => server::SHARED_FOLDERS_FILES,
        }
    }

    pub fn payload(&self) -> Result<Vec<u8>, super::ProtocolError> {
        match self {
            Self::Login(login) => login.payload(),
            Self::SetWaitPort(port) => {
                let mut writer = Writer::new();
                writer.u32(*port);
                Ok(writer.finish())
            }
            Self::GetPeerAddress(username) => {
                let mut writer = Writer::new();
                writer.string(username)?;
                Ok(writer.finish())
            }
            Self::ConnectToPeer {
                token,
                username,
                conn_type,
            } => {
                let mut writer = Writer::new();
                writer.u32(*token);
                writer.string(username)?;
                writer.string(conn_type.as_str())?;
                Ok(writer.finish())
            }
            Self::CantConnectToPeer { token, username } => {
                let mut writer = Writer::new();
                writer.u32(*token);
                writer.string(username)?;
                Ok(writer.finish())
            }
            Self::FileSearch(search) | Self::WishlistSearch(search) => search.payload(),
            Self::RoomSearch { room, token, query } => {
                let mut writer = Writer::new();
                writer.string(room)?;
                writer.u32(*token);
                writer.string(&sanitize_search_query(query))?;
                Ok(writer.finish())
            }
            Self::UserSearch {
                username,
                token,
                query,
            } => {
                let mut writer = Writer::new();
                writer.string(username)?;
                writer.u32(*token);
                writer.string(&sanitize_search_query(query))?;
                Ok(writer.finish())
            }
            Self::HaveNoParent(no_parent) | Self::AcceptChildren(no_parent) => {
                let mut writer = Writer::new();
                writer.u8(u8::from(*no_parent));
                Ok(writer.finish())
            }
            Self::BranchLevel(level) => {
                let mut writer = Writer::new();
                writer.u32(*level);
                Ok(writer.finish())
            }
            Self::BranchRoot(username) => {
                let mut writer = Writer::new();
                writer.string(username)?;
                Ok(writer.finish())
            }
            Self::SetStatus(status) => {
                let mut writer = Writer::new();
                writer.i32(status.status.as_i32());
                Ok(writer.finish())
            }
            Self::Ping | Self::Recommendations | Self::SimilarUsers => Ok(Vec::new()),
            Self::SharedFoldersFiles { folders, files } => {
                let mut writer = Writer::new();
                writer.u32(*folders);
                writer.u32(*files);
                Ok(writer.finish())
            }
            Self::WatchUser(username)
            | Self::GetUserStats(username)
            | Self::AddThingILike(username)
            | Self::UserInterests(username) => {
                let mut writer = Writer::new();
                writer.string(username)?;
                Ok(writer.finish())
            }
        }
    }

    pub fn frame(&self) -> Result<Vec<u8>, super::ProtocolError> {
        encode_frame(self.code(), &self.payload()?)
    }
}

/// MD5 hex digest of `username + password`, which is what the login message carries.
pub fn login_digest(username: &str, password: &str) -> String {
    let mut hasher = Md5::new();
    hasher.update(username.as_bytes());
    hasher.update(password.as_bytes());
    let digest = hasher.finalize();
    let mut hex = String::with_capacity(digest.len() * 2);
    for byte in digest {
        hex.push(HEX[usize::from(byte >> 4)]);
        hex.push(HEX[usize::from(byte & 0x0f)]);
    }
    hex
}

const HEX: [char; 16] = [
    '0', '1', '2', '3', '4', '5', '6', '7', '8', '9', 'a', 'b', 'c', 'd', 'e', 'f',
];

/// Nicotine+ drops whitespace-delimited tokens that are only `-` before sending a search.
pub fn sanitize_search_query(query: &str) -> String {
    query
        .split_whitespace()
        .filter(|token| *token != "-")
        .collect::<Vec<_>>()
        .join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::{FrameDecoder, server};

    #[test]
    fn login_frame_matches_the_nicotine_layout() {
        let login = LoginRequest::new("alice", "secret");
        assert_eq!(
            login_digest("alice", "secret"),
            "c4e31313222cf05fcdd1fc068af5570e"
        );
        let frame = ClientMessage::Login(login).frame().unwrap();
        let expected = hex(
            "430000000100000005000000616c69636506000000736563726574b100000020000000633465333133313332323263663035666364643166633036386166353537306501000000",
        );
        assert_eq!(frame, expected);
        let debug = format!(
            "{:?}",
            ClientMessage::Login(LoginRequest::new("alice", "secret"))
        );
        assert!(!debug.contains("secret"));
    }

    #[test]
    fn login_response_success_and_failure() {
        let failure = decode_login_response(&hex("000b000000494e56414c494450415353")).unwrap();
        assert_eq!(
            failure,
            LoginResponse::Failure {
                reason: "INVALIDPASS".to_owned()
            }
        );

        let success = decode_login_response(&hex(
            "010500000068656c6c6f0100007f20000000633465333133313332323263663035666364643166633036386166353537306501ff",
        ))
        .unwrap();
        assert_eq!(
            success,
            LoginResponse::Success {
                banner: "hello".to_owned(),
                ip: Ipv4Addr::new(127, 0, 0, 1),
                password_digest: "c4e31313222cf05fcdd1fc068af5570e".to_owned(),
                supporter: true,
            }
        );
    }

    #[test]
    fn file_search_strips_lone_dash_tokens() {
        let search = FileSearchRequest {
            token: 12_345,
            query: "jazz - piano".to_owned(),
        };
        assert_eq!(
            search.payload().unwrap(),
            hex("393000000a0000006a617a7a207069616e6f")
        );
        let incoming = decode_incoming_file_search(&hex(
            "05000000616c696365393000000a0000006a617a7a207069616e6f",
        ))
        .unwrap();
        assert_eq!(incoming.username, "alice");
        assert_eq!(incoming.token, 12_345);
        assert_eq!(incoming.query, "jazz piano");
    }

    #[test]
    fn wait_port_status_and_ping_codes() {
        let port = ClientMessage::SetWaitPort(2234).frame().unwrap();
        assert_eq!(port, hex("0800000002000000ba080000"));
        let away = ClientMessage::SetStatus(SetStatus {
            status: UserStatus::Away,
        })
        .frame()
        .unwrap();
        assert_eq!(away, hex("080000001c00000001000000"));
        let ping = ClientMessage::Ping.frame().unwrap();
        assert_eq!(ping, hex("0400000020000000"));
        let online = ClientMessage::SetStatus(SetStatus {
            status: UserStatus::Online,
        })
        .frame()
        .unwrap();
        assert_eq!(online, hex("080000001c00000002000000"));
        assert_eq!(server::SET_STATUS, 28);
        let shares = ClientMessage::SharedFoldersFiles {
            folders: 1,
            files: 1,
        }
        .frame()
        .unwrap();
        assert_eq!(shares, hex("0c000000230000000100000001000000"));
        assert_eq!(server::SHARED_FOLDERS_FILES, 35);
        let ask = ClientMessage::GetPeerAddress("bob".to_owned())
            .frame()
            .unwrap();
        assert_eq!(ask, hex("0b0000000300000003000000626f62"));
        let address =
            decode_peer_address(&hex("03000000626f620100007fba080000000000000000")).unwrap();
        assert_eq!(address.username, "bob");
        assert_eq!(address.ip, Ipv4Addr::new(127, 0, 0, 1));
        assert_eq!(address.port, 2234);
        let indirect = ClientMessage::ConnectToPeer {
            token: 7,
            username: "bob".to_owned(),
            conn_type: crate::protocol::ConnType::Peer,
        }
        .frame()
        .unwrap();
        assert_eq!(
            indirect,
            hex("14000000120000000700000003000000626f620100000050")
        );
        let refused = ClientMessage::CantConnectToPeer {
            token: 9,
            username: "bob".to_owned(),
        }
        .frame()
        .unwrap();
        assert_eq!(refused, hex("0f000000e90300000900000003000000626f62"));
        assert_eq!(decode_cant_connect(&refused[8..]).unwrap(), 9);
        let status = decode_user_status(&hex("05000000616c6963650200000001")).unwrap();
        assert_eq!(
            status,
            UserStatusNotice {
                user: "alice".to_owned(),
                status: 2,
                privileged: true,
            }
        );
    }

    #[test]
    fn truncated_login_response_errors() {
        assert!(decode_login_response(&[1]).is_err());
    }

    #[test]
    fn decoder_reads_a_login_frame() {
        let frame = ClientMessage::Login(LoginRequest::new("alice", "secret"))
            .frame()
            .unwrap();
        let mut decoder = FrameDecoder::new();
        decoder.push(&frame);
        let decoded = decoder.pop().unwrap().unwrap();
        assert_eq!(decoded.code, server::LOGIN);
    }

    #[test]
    fn search_modes_match_the_nicotine_layout() {
        let room = ClientMessage::RoomSearch {
            room: "jazz".to_owned(),
            token: 7,
            query: "piano".to_owned(),
        };
        assert_eq!(
            room.payload().unwrap(),
            hex("040000006a617a7a07000000050000007069616e6f")
        );
        let user = ClientMessage::UserSearch {
            username: "bob".to_owned(),
            token: 7,
            query: "piano".to_owned(),
        };
        assert_eq!(
            user.payload().unwrap(),
            hex("03000000626f6207000000050000007069616e6f")
        );
        let wishlist = ClientMessage::WishlistSearch(FileSearchRequest {
            token: 7,
            query: "piano".to_owned(),
        });
        assert_eq!(wishlist.code(), server::WISHLIST_SEARCH);
        assert_eq!(
            wishlist.payload().unwrap(),
            hex("07000000050000007069616e6f")
        );
        assert_eq!(decode_wishlist_interval(&hex("3c000000")).unwrap(), 60);
        let parents =
            decode_possible_parents(&hex("0100000003000000626f620100007fba080000")).unwrap();
        assert_eq!(parents[0].username, "bob");
        assert_eq!(parents[0].ip, Ipv4Addr::new(127, 0, 0, 1));
        assert_eq!(parents[0].port, 2234);
        assert_eq!(
            decode_user_search(&user.payload().unwrap()).unwrap().query,
            "piano"
        );
        assert_eq!(
            ClientMessage::HaveNoParent(true).payload().unwrap(),
            hex("01")
        );
        assert_eq!(
            ClientMessage::BranchLevel(1).payload().unwrap(),
            hex("01000000")
        );
        assert_eq!(
            ClientMessage::BranchRoot("alice".to_owned())
                .payload()
                .unwrap(),
            hex("05000000616c696365")
        );
    }

    #[test]
    fn watch_interests_and_recommendations_match_the_nicotine_layout() {
        assert_eq!(
            ClientMessage::WatchUser("bob".to_owned()).frame().unwrap(),
            hex("0b0000000500000003000000626f62")
        );
        let online = decode_watch_user(&hex(
            "03000000626f6201020000000a00000001000000000000000200000001000000020000005553",
        ))
        .unwrap();
        assert_eq!(online.status, 2);
        assert_eq!(online.country, "US");
        let away = decode_watch_user(&hex(
            "03000000626f6201010000000000000000000000000000000000000000000000020000004445",
        ))
        .unwrap();
        assert_eq!(UserStatus::from_wire(away.status), UserStatus::Away);
        let offline = decode_watch_user(&hex(
            "03000000626f6201000000000000000000000000000000000000000000000000",
        ))
        .unwrap();
        assert_eq!(UserStatus::from_wire(offline.status), UserStatus::Offline);
        assert!(offline.country.is_empty());
        assert_eq!(
            ClientMessage::GetUserStats("bob".to_owned())
                .frame()
                .unwrap(),
            hex("0b0000002400000003000000626f62")
        );
        let stats = decode_user_stats(&hex(
            "03000000626f620a00000001000000000000000200000001000000",
        ))
        .unwrap();
        assert_eq!(stats.files, 2);
        assert_eq!(stats.dirs, 1);
        assert_eq!(
            ClientMessage::AddThingILike("jazz".to_owned())
                .frame()
                .unwrap(),
            hex("0c00000033000000040000006a617a7a")
        );
        assert_eq!(
            ClientMessage::UserInterests("bob".to_owned())
                .frame()
                .unwrap(),
            hex("0b0000003900000003000000626f62")
        );
        let interests = decode_user_interests(&hex(
            "03000000626f6201000000040000006a617a7a01000000050000006d6574616c",
        ))
        .unwrap();
        assert_eq!(interests.likes, vec!["jazz".to_owned()]);
        assert_eq!(interests.hates, vec!["metal".to_owned()]);
        assert_eq!(
            ClientMessage::Recommendations.frame().unwrap(),
            hex("0400000036000000")
        );
        let recommendations =
            decode_recommendations(&hex("01000000040000006a617a7a0300000000000000")).unwrap();
        assert_eq!(recommendations[0].name, "jazz");
        assert_eq!(recommendations[0].rating, 3);
        assert_eq!(
            ClientMessage::SimilarUsers.frame().unwrap(),
            hex("040000006e000000")
        );
        let similar = decode_similar_users(&hex("0100000003000000626f6204000000")).unwrap();
        assert_eq!(similar[0].name, "bob");
        assert_eq!(similar[0].rating, 4);
    }

    fn hex(text: &str) -> Vec<u8> {
        (0..text.len())
            .step_by(2)
            .map(|index| u8::from_str_radix(&text[index..index + 2], 16).unwrap())
            .collect()
    }
}
