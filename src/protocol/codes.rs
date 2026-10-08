//! Numeric Soulseek message codes.
//!
//! Values match Nicotine+ 3.3.11. A code constant records the number on the wire.
//! It does not mean this crate can encode or decode that message.

/// Server message codes.
pub mod server {
    /// `Login` is code 1.
    pub const LOGIN: u32 = 1;
    /// `SetWaitPort` is code 2.
    pub const SET_WAIT_PORT: u32 = 2;
    /// `GetPeerAddress` is code 3.
    pub const GET_PEER_ADDRESS: u32 = 3;
    /// `WatchUser` is code 5.
    pub const WATCH_USER: u32 = 5;
    /// `UnwatchUser` is code 6.
    pub const UNWATCH_USER: u32 = 6;
    /// `GetUserStatus` is code 7.
    pub const GET_USER_STATUS: u32 = 7;
    /// `IgnoreUser` is code 11.
    pub const IGNORE_USER: u32 = 11;
    /// `UnignoreUser` is code 12.
    pub const UNIGNORE_USER: u32 = 12;
    /// `SayChatroom` is code 13.
    pub const SAY_CHATROOM: u32 = 13;
    /// `JoinRoom` is code 14.
    pub const JOIN_ROOM: u32 = 14;
    /// `LeaveRoom` is code 15.
    pub const LEAVE_ROOM: u32 = 15;
    /// `UserJoinedRoom` is code 16.
    pub const USER_JOINED_ROOM: u32 = 16;
    /// `UserLeftRoom` is code 17.
    pub const USER_LEFT_ROOM: u32 = 17;
    /// `ConnectToPeer` is code 18.
    pub const CONNECT_TO_PEER: u32 = 18;
    /// `MessageUser` is code 22.
    pub const MESSAGE_USER: u32 = 22;
    /// `MessageAcked` is code 23.
    pub const MESSAGE_ACKED: u32 = 23;
    /// `FileSearchRoom` is code 25. Obsolete.
    pub const FILE_SEARCH_ROOM: u32 = 25;
    /// `FileSearch` is code 26.
    pub const FILE_SEARCH: u32 = 26;
    /// `SetStatus` is code 28.
    pub const SET_STATUS: u32 = 28;
    /// `ServerPing` is code 32.
    pub const SERVER_PING: u32 = 32;
    /// `SendConnectToken` is code 33. Obsolete.
    pub const SEND_CONNECT_TOKEN: u32 = 33;
    /// `SendDownloadSpeed` is code 34. Obsolete.
    pub const SEND_DOWNLOAD_SPEED: u32 = 34;
    /// `SharedFoldersFiles` is code 35.
    pub const SHARED_FOLDERS_FILES: u32 = 35;
    /// `GetUserStats` is code 36.
    pub const GET_USER_STATS: u32 = 36;
    /// `QueuedDownloads` is code 40. Obsolete.
    pub const QUEUED_DOWNLOADS: u32 = 40;
    /// `Relogged` is code 41.
    pub const RELOGGED: u32 = 41;
    /// `UserSearch` is code 42.
    pub const USER_SEARCH: u32 = 42;
    /// `SimilarRecommendations` is code 50. Obsolete.
    pub const SIMILAR_RECOMMENDATIONS: u32 = 50;
    /// `AddThingILike` is code 51. Deprecated.
    pub const ADD_THING_I_LIKE: u32 = 51;
    /// `RemoveThingILike` is code 52. Deprecated.
    pub const REMOVE_THING_I_LIKE: u32 = 52;
    /// `Recommendations` is code 54. Deprecated.
    pub const RECOMMENDATIONS: u32 = 54;
    /// `MyRecommendations` is code 55. Obsolete.
    pub const MY_RECOMMENDATIONS: u32 = 55;
    /// `GlobalRecommendations` is code 56. Deprecated.
    pub const GLOBAL_RECOMMENDATIONS: u32 = 56;
    /// `UserInterests` is code 57. Deprecated.
    pub const USER_INTERESTS: u32 = 57;
    /// `AdminCommand` is code 58. Obsolete.
    pub const ADMIN_COMMAND: u32 = 58;
    /// `PlaceInLineResponse` is code 60. Obsolete.
    pub const PLACE_IN_LINE_RESPONSE: u32 = 60;
    /// `RoomAdded` is code 62. Obsolete.
    pub const ROOM_ADDED: u32 = 62;
    /// `RoomRemoved` is code 63. Obsolete.
    pub const ROOM_REMOVED: u32 = 63;
    /// `RoomList` is code 64.
    pub const ROOM_LIST: u32 = 64;
    /// `ExactFileSearch` is code 65. Obsolete.
    pub const EXACT_FILE_SEARCH: u32 = 65;
    /// `AdminMessage` is code 66.
    pub const ADMIN_MESSAGE: u32 = 66;
    /// `GlobalUserList` is code 67. Obsolete.
    pub const GLOBAL_USER_LIST: u32 = 67;
    /// `TunneledMessage` is code 68. Obsolete.
    pub const TUNNELED_MESSAGE: u32 = 68;
    /// `PrivilegedUsers` is code 69.
    pub const PRIVILEGED_USERS: u32 = 69;
    /// `HaveNoParent` is code 71.
    pub const HAVE_NO_PARENT: u32 = 71;
    /// `SearchParent` is code 73. Deprecated.
    pub const SEARCH_PARENT: u32 = 73;
    /// `ParentMinSpeed` is code 83.
    pub const PARENT_MIN_SPEED: u32 = 83;
    /// `ParentSpeedRatio` is code 84.
    pub const PARENT_SPEED_RATIO: u32 = 84;
    /// `ParentInactivityTimeout` is code 86. Obsolete.
    pub const PARENT_INACTIVITY_TIMEOUT: u32 = 86;
    /// `SearchInactivityTimeout` is code 87. Obsolete.
    pub const SEARCH_INACTIVITY_TIMEOUT: u32 = 87;
    /// `MinParentsInCache` is code 88. Obsolete.
    pub const MIN_PARENTS_IN_CACHE: u32 = 88;
    /// `DistribPingInterval` is code 90. Obsolete.
    pub const DISTRIB_PING_INTERVAL: u32 = 90;
    /// `AddToPrivileged` is code 91. Obsolete.
    pub const ADD_TO_PRIVILEGED: u32 = 91;
    /// `CheckPrivileges` is code 92.
    pub const CHECK_PRIVILEGES: u32 = 92;
    /// `EmbeddedMessage` is code 93.
    pub const EMBEDDED_MESSAGE: u32 = 93;
    /// `AcceptChildren` is code 100.
    pub const ACCEPT_CHILDREN: u32 = 100;
    /// `PossibleParents` is code 102.
    pub const POSSIBLE_PARENTS: u32 = 102;
    /// `WishlistSearch` is code 103.
    pub const WISHLIST_SEARCH: u32 = 103;
    /// `WishlistInterval` is code 104.
    pub const WISHLIST_INTERVAL: u32 = 104;
    /// `SimilarUsers` is code 110. Deprecated.
    pub const SIMILAR_USERS: u32 = 110;
    /// `ItemRecommendations` is code 111. Deprecated.
    pub const ITEM_RECOMMENDATIONS: u32 = 111;
    /// `ItemSimilarUsers` is code 112. Deprecated.
    pub const ITEM_SIMILAR_USERS: u32 = 112;
    /// `RoomTickerState` is code 113.
    pub const ROOM_TICKER_STATE: u32 = 113;
    /// `RoomTickerAdd` is code 114.
    pub const ROOM_TICKER_ADD: u32 = 114;
    /// `RoomTickerRemove` is code 115.
    pub const ROOM_TICKER_REMOVE: u32 = 115;
    /// `RoomTickerSet` is code 116.
    pub const ROOM_TICKER_SET: u32 = 116;
    /// `AddThingIHate` is code 117. Deprecated.
    pub const ADD_THING_I_HATE: u32 = 117;
    /// `RemoveThingIHate` is code 118. Deprecated.
    pub const REMOVE_THING_I_HATE: u32 = 118;
    /// `RoomSearch` is code 120.
    pub const ROOM_SEARCH: u32 = 120;
    /// `SendUploadSpeed` is code 121.
    pub const SEND_UPLOAD_SPEED: u32 = 121;
    /// `UserPrivileged` is code 122. Deprecated.
    pub const USER_PRIVILEGED: u32 = 122;
    /// `GivePrivileges` is code 123.
    pub const GIVE_PRIVILEGES: u32 = 123;
    /// `NotifyPrivileges` is code 124. Deprecated.
    pub const NOTIFY_PRIVILEGES: u32 = 124;
    /// `AckNotifyPrivileges` is code 125. Deprecated.
    pub const ACK_NOTIFY_PRIVILEGES: u32 = 125;
    /// `BranchLevel` is code 126.
    pub const BRANCH_LEVEL: u32 = 126;
    /// `BranchRoot` is code 127.
    pub const BRANCH_ROOT: u32 = 127;
    /// `ChildDepth` is code 129. Deprecated.
    pub const CHILD_DEPTH: u32 = 129;
    /// `ResetDistributed` is code 130.
    pub const RESET_DISTRIBUTED: u32 = 130;
    /// `PrivateRoomUsers` is code 133.
    pub const PRIVATE_ROOM_USERS: u32 = 133;
    /// `PrivateRoomAddUser` is code 134.
    pub const PRIVATE_ROOM_ADD_USER: u32 = 134;
    /// `PrivateRoomRemoveUser` is code 135.
    pub const PRIVATE_ROOM_REMOVE_USER: u32 = 135;
    /// `PrivateRoomCancelMembership` is code 136.
    pub const PRIVATE_ROOM_CANCEL_MEMBERSHIP: u32 = 136;
    /// `PrivateRoomDisown` is code 137.
    pub const PRIVATE_ROOM_DISOWN: u32 = 137;
    /// `PrivateRoomSomething` is code 138. Obsolete.
    pub const PRIVATE_ROOM_SOMETHING: u32 = 138;
    /// `PrivateRoomAdded` is code 139.
    pub const PRIVATE_ROOM_ADDED: u32 = 139;
    /// `PrivateRoomRemoved` is code 140.
    pub const PRIVATE_ROOM_REMOVED: u32 = 140;
    /// `PrivateRoomToggle` is code 141.
    pub const PRIVATE_ROOM_TOGGLE: u32 = 141;
    /// `ChangePassword` is code 142.
    pub const CHANGE_PASSWORD: u32 = 142;
    /// `PrivateRoomAddOperator` is code 143.
    pub const PRIVATE_ROOM_ADD_OPERATOR: u32 = 143;
    /// `PrivateRoomRemoveOperator` is code 144.
    pub const PRIVATE_ROOM_REMOVE_OPERATOR: u32 = 144;
    /// `PrivateRoomOperatorAdded` is code 145.
    pub const PRIVATE_ROOM_OPERATOR_ADDED: u32 = 145;
    /// `PrivateRoomOperatorRemoved` is code 146.
    pub const PRIVATE_ROOM_OPERATOR_REMOVED: u32 = 146;
    /// `PrivateRoomOperators` is code 148.
    pub const PRIVATE_ROOM_OPERATORS: u32 = 148;
    /// `MessageUsers` is code 149.
    pub const MESSAGE_USERS: u32 = 149;
    /// `JoinGlobalRoom` is code 150. Deprecated.
    pub const JOIN_GLOBAL_ROOM: u32 = 150;
    /// `LeaveGlobalRoom` is code 151. Deprecated.
    pub const LEAVE_GLOBAL_ROOM: u32 = 151;
    /// `GlobalRoomMessage` is code 152. Deprecated.
    pub const GLOBAL_ROOM_MESSAGE: u32 = 152;
    /// `RelatedSearch` is code 153. Obsolete.
    pub const RELATED_SEARCH: u32 = 153;
    /// `ExcludedSearchPhrases` is code 160.
    pub const EXCLUDED_SEARCH_PHRASES: u32 = 160;
    /// `CantConnectToPeer` is code 1001.
    pub const CANT_CONNECT_TO_PEER: u32 = 1001;
    /// `CantCreateRoom` is code 1003.
    pub const CANT_CREATE_ROOM: u32 = 1003;
}

/// Peer initialization message codes.
pub mod peer_init {
    /// `PierceFireWall` is code 0.
    pub const PIERCE_FIRE_WALL: u32 = 0;
    /// `PeerInit` is code 1.
    pub const PEER_INIT: u32 = 1;
}

/// Peer message codes.
pub mod peer {
    /// `SharedFileListRequest` is code 4.
    pub const SHARED_FILE_LIST_REQUEST: u32 = 4;
    /// `SharedFileListResponse` is code 5.
    pub const SHARED_FILE_LIST_RESPONSE: u32 = 5;
    /// `FileSearchRequest` is code 8. Obsolete.
    pub const FILE_SEARCH_REQUEST: u32 = 8;
    /// `FileSearchResponse` is code 9.
    pub const FILE_SEARCH_RESPONSE: u32 = 9;
    /// `UserInfoRequest` is code 15.
    pub const USER_INFO_REQUEST: u32 = 15;
    /// `UserInfoResponse` is code 16.
    pub const USER_INFO_RESPONSE: u32 = 16;
    /// `PMessageUser` is code 22. Obsolete.
    pub const P_MESSAGE_USER: u32 = 22;
    /// `FolderContentsRequest` is code 36.
    pub const FOLDER_CONTENTS_REQUEST: u32 = 36;
    /// `FolderContentsResponse` is code 37.
    pub const FOLDER_CONTENTS_RESPONSE: u32 = 37;
    /// `TransferRequest` is code 40.
    pub const TRANSFER_REQUEST: u32 = 40;
    /// `TransferResponse` is code 41.
    pub const TRANSFER_RESPONSE: u32 = 41;
    /// `PlaceholdUpload` is code 42. Obsolete.
    pub const PLACEHOLD_UPLOAD: u32 = 42;
    /// `QueueUpload` is code 43.
    pub const QUEUE_UPLOAD: u32 = 43;
    /// `PlaceInQueueResponse` is code 44.
    pub const PLACE_IN_QUEUE_RESPONSE: u32 = 44;
    /// `UploadFailed` is code 46.
    pub const UPLOAD_FAILED: u32 = 46;
    /// `UploadDenied` is code 50.
    pub const UPLOAD_DENIED: u32 = 50;
    /// `PlaceInQueueRequest` is code 51.
    pub const PLACE_IN_QUEUE_REQUEST: u32 = 51;
    /// `UploadQueueNotification` is code 52. Deprecated.
    pub const UPLOAD_QUEUE_NOTIFICATION: u32 = 52;
    /// `UnknownPeerMessage` is code 12547.
    pub const UNKNOWN_PEER_MESSAGE: u32 = 12547;
}

/// Distributed-network message codes.
pub mod distributed {
    /// `DistribPing` is code 0. Deprecated.
    pub const DISTRIB_PING: u32 = 0;
    /// `DistribSearch` is code 3.
    pub const DISTRIB_SEARCH: u32 = 3;
    /// `DistribBranchLevel` is code 4.
    pub const DISTRIB_BRANCH_LEVEL: u32 = 4;
    /// `DistribBranchRoot` is code 5.
    pub const DISTRIB_BRANCH_ROOT: u32 = 5;
    /// `DistribChildDepth` is code 7. Deprecated.
    pub const DISTRIB_CHILD_DEPTH: u32 = 7;
    /// `DistribEmbeddedMessage` is code 93. Deprecated.
    pub const DISTRIB_EMBEDDED_MESSAGE: u32 = 93;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn server_codes_are_unique() {
        let mut codes = vec![
            server::LOGIN,
            server::SET_WAIT_PORT,
            server::GET_PEER_ADDRESS,
            server::WATCH_USER,
            server::UNWATCH_USER,
            server::GET_USER_STATUS,
            server::IGNORE_USER,
            server::UNIGNORE_USER,
            server::SAY_CHATROOM,
            server::JOIN_ROOM,
            server::LEAVE_ROOM,
            server::USER_JOINED_ROOM,
            server::USER_LEFT_ROOM,
            server::CONNECT_TO_PEER,
            server::MESSAGE_USER,
            server::MESSAGE_ACKED,
            server::FILE_SEARCH_ROOM,
            server::FILE_SEARCH,
            server::SET_STATUS,
            server::SERVER_PING,
            server::SEND_CONNECT_TOKEN,
            server::SEND_DOWNLOAD_SPEED,
            server::SHARED_FOLDERS_FILES,
            server::GET_USER_STATS,
            server::QUEUED_DOWNLOADS,
            server::RELOGGED,
            server::USER_SEARCH,
            server::SIMILAR_RECOMMENDATIONS,
            server::ADD_THING_I_LIKE,
            server::REMOVE_THING_I_LIKE,
            server::RECOMMENDATIONS,
            server::MY_RECOMMENDATIONS,
            server::GLOBAL_RECOMMENDATIONS,
            server::USER_INTERESTS,
            server::ADMIN_COMMAND,
            server::PLACE_IN_LINE_RESPONSE,
            server::ROOM_ADDED,
            server::ROOM_REMOVED,
            server::ROOM_LIST,
            server::EXACT_FILE_SEARCH,
            server::ADMIN_MESSAGE,
            server::GLOBAL_USER_LIST,
            server::TUNNELED_MESSAGE,
            server::PRIVILEGED_USERS,
            server::HAVE_NO_PARENT,
            server::SEARCH_PARENT,
            server::PARENT_MIN_SPEED,
            server::PARENT_SPEED_RATIO,
            server::PARENT_INACTIVITY_TIMEOUT,
            server::SEARCH_INACTIVITY_TIMEOUT,
            server::MIN_PARENTS_IN_CACHE,
            server::DISTRIB_PING_INTERVAL,
            server::ADD_TO_PRIVILEGED,
            server::CHECK_PRIVILEGES,
            server::EMBEDDED_MESSAGE,
            server::ACCEPT_CHILDREN,
            server::POSSIBLE_PARENTS,
            server::WISHLIST_SEARCH,
            server::WISHLIST_INTERVAL,
            server::SIMILAR_USERS,
            server::ITEM_RECOMMENDATIONS,
            server::ITEM_SIMILAR_USERS,
            server::ROOM_TICKER_STATE,
            server::ROOM_TICKER_ADD,
            server::ROOM_TICKER_REMOVE,
            server::ROOM_TICKER_SET,
            server::ADD_THING_I_HATE,
            server::REMOVE_THING_I_HATE,
            server::ROOM_SEARCH,
            server::SEND_UPLOAD_SPEED,
            server::USER_PRIVILEGED,
            server::GIVE_PRIVILEGES,
            server::NOTIFY_PRIVILEGES,
            server::ACK_NOTIFY_PRIVILEGES,
            server::BRANCH_LEVEL,
            server::BRANCH_ROOT,
            server::CHILD_DEPTH,
            server::RESET_DISTRIBUTED,
            server::PRIVATE_ROOM_USERS,
            server::PRIVATE_ROOM_ADD_USER,
            server::PRIVATE_ROOM_REMOVE_USER,
            server::PRIVATE_ROOM_CANCEL_MEMBERSHIP,
            server::PRIVATE_ROOM_DISOWN,
            server::PRIVATE_ROOM_SOMETHING,
            server::PRIVATE_ROOM_ADDED,
            server::PRIVATE_ROOM_REMOVED,
            server::PRIVATE_ROOM_TOGGLE,
            server::CHANGE_PASSWORD,
            server::PRIVATE_ROOM_ADD_OPERATOR,
            server::PRIVATE_ROOM_REMOVE_OPERATOR,
            server::PRIVATE_ROOM_OPERATOR_ADDED,
            server::PRIVATE_ROOM_OPERATOR_REMOVED,
            server::PRIVATE_ROOM_OPERATORS,
            server::MESSAGE_USERS,
            server::JOIN_GLOBAL_ROOM,
            server::LEAVE_GLOBAL_ROOM,
            server::GLOBAL_ROOM_MESSAGE,
            server::RELATED_SEARCH,
            server::EXCLUDED_SEARCH_PHRASES,
            server::CANT_CONNECT_TO_PEER,
            server::CANT_CREATE_ROOM,
        ];
        let count = codes.len();
        codes.sort_unstable();
        codes.dedup();
        assert_eq!(codes.len(), count);
    }

    #[test]
    fn peer_init_codes_are_unique() {
        let mut codes = vec![peer_init::PIERCE_FIRE_WALL, peer_init::PEER_INIT];
        let count = codes.len();
        codes.sort_unstable();
        codes.dedup();
        assert_eq!(codes.len(), count);
    }

    #[test]
    fn peer_codes_are_unique() {
        let mut codes = vec![
            peer::SHARED_FILE_LIST_REQUEST,
            peer::SHARED_FILE_LIST_RESPONSE,
            peer::FILE_SEARCH_REQUEST,
            peer::FILE_SEARCH_RESPONSE,
            peer::USER_INFO_REQUEST,
            peer::USER_INFO_RESPONSE,
            peer::P_MESSAGE_USER,
            peer::FOLDER_CONTENTS_REQUEST,
            peer::FOLDER_CONTENTS_RESPONSE,
            peer::TRANSFER_REQUEST,
            peer::TRANSFER_RESPONSE,
            peer::PLACEHOLD_UPLOAD,
            peer::QUEUE_UPLOAD,
            peer::PLACE_IN_QUEUE_RESPONSE,
            peer::UPLOAD_FAILED,
            peer::UPLOAD_DENIED,
            peer::PLACE_IN_QUEUE_REQUEST,
            peer::UPLOAD_QUEUE_NOTIFICATION,
            peer::UNKNOWN_PEER_MESSAGE,
        ];
        let count = codes.len();
        codes.sort_unstable();
        codes.dedup();
        assert_eq!(codes.len(), count);
    }

    #[test]
    fn distributed_codes_are_unique() {
        let mut codes = vec![
            distributed::DISTRIB_PING,
            distributed::DISTRIB_SEARCH,
            distributed::DISTRIB_BRANCH_LEVEL,
            distributed::DISTRIB_BRANCH_ROOT,
            distributed::DISTRIB_CHILD_DEPTH,
            distributed::DISTRIB_EMBEDDED_MESSAGE,
        ];
        let count = codes.len();
        codes.sort_unstable();
        codes.dedup();
        assert_eq!(codes.len(), count);
    }
}
