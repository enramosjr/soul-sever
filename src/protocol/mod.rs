//! Soulseek wire format.
//!
//! Framing and the messages encoded here follow the layout Nicotine+ 3.3.11 uses.
//! This module does not open sockets.

pub mod chat;
mod codes;
pub mod distributed;
mod messages;
pub mod peer;
mod wire;

pub use codes::distributed as distributed_codes;
pub use codes::{peer_init, server};
pub use distributed::{
    DistribDecoder, DistribMessage, encode_distrib_branch_level, encode_distrib_branch_root,
    encode_distrib_search,
};
pub use messages::{
    ClientMessage, FileSearchRequest, IncomingConnect, IncomingFileSearch, LoginRequest,
    LoginResponse, MAJOR_VERSION, MINOR_VERSION, PeerAddress, PossibleParent, RatedItem, SetStatus,
    UserInterests, UserStats, UserStatus, UserStatusNotice, WatchUserReply, decode_cant_connect,
    decode_incoming_connect, decode_incoming_file_search, decode_incoming_room_search,
    decode_login_response, decode_peer_address, decode_possible_parents, decode_recommendations,
    decode_similar_users, decode_user_interests, decode_user_search, decode_user_stats,
    decode_user_status, decode_watch_user, decode_wishlist_interval, decode_wishlist_search,
    login_digest, sanitize_search_query,
};
pub use peer::{
    ConnType, FileSearchResponse, PeerHandshake, PeerInitDecoder, SearchResultFile,
    encode_peer_init, encode_pierce_firewall,
};
pub use wire::{
    FrameDecoder, MAX_MESSAGE_1M, MAX_MESSAGE_16K, MAX_MESSAGE_16M, MAX_MESSAGE_448M,
    ProtocolError, ServerFrame, encode_frame,
};
