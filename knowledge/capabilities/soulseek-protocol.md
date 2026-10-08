---
type: Capability
title: Soulseek protocol codec
description: Frame reader and client messages through chat, matched to Nicotine+ 3.3.11.
tags: [soulseek, protocol, codec]
status: draft
evidence: validated-runtime
generated: { by: "cursor-agent/auto", at: 2026-10-02T23:50:00Z }
verified: { by: "process:cargo-test", at: 2026-10-05T00:25:28Z }
sources:
  - id: frames
    resource: src/protocol/wire.rs
    title: Length-prefixed frame codec
  - id: messages
    resource: src/protocol/messages.rs
    title: Login, search, status, ping encoders
  - id: distributed
    resource: src/protocol/distributed.rs
    title: Distributed search frames
  - id: codes
    resource: src/protocol/codes.rs
    title: Message code constants
  - id: peer-init
    resource: src/protocol/peer.rs
    title: Peer init and pierce firewall frames
  - id: chat
    resource: src/protocol/chat.rs
    title: Room, private message, and private-room frames
---

# Soulseek protocol codec

Server frames are little-endian: `uint32 size`, `uint32 code`, then `size - 4` payload bytes. `size` includes the code and excludes itself. `FrameDecoder` waits for a full frame, rejects a size below 4, and rejects a size above its cap before the body arrives. The default cap is 16 MiB. The 448 MiB, 1 MiB, and 16 KiB Nicotine+ caps are constants for later sockets.

Strings are `uint32` byte length plus UTF-8. Decode falls back to Latin-1 when the bytes are not UTF-8. IPv4 addresses are little-endian on the wire (`01 00 00 7f` is `127.0.0.1`).

Implemented messages:

| Message | Code | Direction covered |
|---------|------|-------------------|
| Login | 1 | Client encode. Server success and failure decode. |
| SetWaitPort | 2 | Client encode. |
| FileSearch | 26 | Client encode, including dropping tokens that are only `-`. Server delivery decode (`username`, token, query). |
| UserSearch | 42 | Client encode and the same server delivery parse as `FileSearch`. |
| HaveNoParent | 71 | Client encode of a bool. |
| AcceptChildren | 100 | Client encode of a bool. |
| PossibleParents | 102 | Server decode: count, then username, IPv4, port. |
| WishlistSearch | 103 | Client encode of token and query. Incoming parse matches `FileSearch`. |
| WishlistInterval | 104 | Server decode of an interval in seconds. |
| RoomSearch | 120 | Client encode: room, token, query. The obsolete server parse is username, token, query. |
| BranchLevel | 126 | Server decode of a `u32`. |
| BranchRoot | 127 | Server decode of a username. |
| SetStatus | 28 | Client encode as `int32`. Away is `1`, online is `2`. |
| ServerPing | 32 | Client encode of an empty payload. |
| GetUserStatus | 7 | Server decode: username, status `u32`, privileged bool. |
| Relogged | 41 | Recognized by the session. Empty payload. |
| GetPeerAddress | 3 | Client encode of a username. Server decode: username, IPv4, port, unknown `u32`, obfuscated port `u16`. |
| ConnectToPeer | 18 | Client encode: token, username, connection type. Server decode adds IPv4, port, token, privileged, unknown `u32`, obfuscated port `u32`. |
| CantConnectToPeer | 1001 | Client encode: token, username. Server decode reads the token. |

Login payload order is username, password, major `u32`, MD5 hex of `username + password`, minor `u32`. The byte test uses user `alice`, password `secret`, major 177, minor 1.

`src/protocol/codes.rs` lists server, peer-init, peer, and distributed codes from Nicotine+ 3.3.11. A constant is not an encoder. Tests assert each namespace has unique numbers.

Peer init is a separate header in [`src/protocol/peer.rs`](../../src/protocol/peer.rs): `uint32 size` (includes the type byte) and `uint8 type`. A size below 1 is shorter than 5 bytes and fails before the body. Type `0` is `PierceFireWall` (a `u32` token). Type `1` is `PeerInit` (username, `P`/`F`/`D`, then a zero token). After that handshake, peer messages use the 8-byte server frame and [`codes::peer`](../../src/protocol/codes.rs).

`FileSearchResponse` (peer code 9) is a zlib payload: the sharer's username, the search token, then the same file records as a share list, then a free-slot bool, upload speed, queue length, and a zero `u32`. Private files follow when that list is not empty.

`UserInfoRequest` (peer code 15) is empty. `UserInfoResponse` (peer code 16) is a description, a picture bool and bytes, upload total, queue size, a free-slot bool, and an upload-permission `u32` when four bytes remain. `FolderContentsRequest` (peer code 36) is a token and a directory. `FolderContentsResponse` (peer code 37) is zlib of a token, a directory, and the same folder records as a share list. `user_info_and_folder_contents_match_the_nicotine_layout` checks those frames, including a folder payload with `a.bin` and `b.bin`.

`WatchUser` (server code 5) is a username. The reply is the username, an exists bool, then status and counts, and a country string when the user is online. Status `0` is offline, `1` is away, and `2` is online. `GetUserStats` (code 36) is a username in the request and five `u32`s in the reply. `AddThingILike` (code 51) and `UserInterests` (code 57) carry strings. `Recommendations` (code 54) and `SimilarUsers` (code 110) match the Nicotine+ list layout. `watch_interests_and_recommendations_match_the_nicotine_layout` checks those bytes.

`JoinRoom` (server code 14) from the client is a room string and a private `u32`. The server reply is the room, then parallel user arrays (names, statuses, five-count stat blocks, slot flags, countries) and, when bytes remain, an owner and operators. `SayChatroom` (code 13) from the client is room and message; the server body adds the speaker between them. `MessageUser` (code 22) from the client is user and message; the server body is an id, a timestamp, the user, the message, and an is-new bool. `MessageAcked` (code 23) is that id. A private-room add (code 134) is room and user. The private-room toggle (code 141) is a bool. A private-room users body is a room, a count, and names. `chat_messages_match_the_nicotine_layout` checks those bytes, including a `jazz` join reply for `bob` in `US`. Room-ticker and room-list decoders live in the same module. No fixture delivered a ticker or a room list.

After the handshake, a file connection (`F`) is not framed. The first four bytes are the transfer token. The downloader then writes a `u64` offset (bytes already on disk) and reads raw file bytes. The uploader reads that offset and writes raw bytes from there. `transfer_messages_match_the_nicotine_layout` checks these peer frames: `QueueUpload` (43), `TransferRequest` (40, filesize only when the direction is upload), `TransferResponse` (41, the offset when allowed), `PlaceInQueueResponse` (44), `UploadFailed` (46), `UploadDenied` (50), and `PlaceInQueueRequest` (51). Deny reasons used on the wire are `Queued`, `File not shared.`, `Banned`, `Cancelled`, `Too many files`, `Too many megabytes`, `Pending shutdown.`, and `Filtered`.

Distributed frames use the peer-init header: `uint32 size` (includes the type byte) and `uint8` type. A size below 1 fails before the body. `DistribSearch` (type 3) is an unknown `u32`, username, token, and query. `DistribBranchLevel` (type 4) is an `i32`. `DistribBranchRoot` (type 5) is a username. An unrecognized type is consumed and ignored.

`SharedFileListRequest` (peer code 4) is an empty frame. `SharedFileListResponse` (peer code 5) is a zlib payload. Uncompressed, it is a directory count, then each directory name, file count, and files. A file is `u8` 1, a name, a `u64` size, an obsolete extension length of 0, and attribute pairs. Attribute codes are bitrate 0, duration 1, VBR 2, sample rate 4, and bit depth 5. A `u32` 0 follows the visible directories. Locked directories follow when the requester cannot browse them. A file size whose eighth byte is `0xff` is read as the low 32 bits, matching the Soulseek NS quirk. The session sends this message when a peer asks. Server code 35, `SharedFoldersFiles`, is a public folder count and then a public file count. `wait_port_status_and_ping_codes` checks one folder and one file. The filtered list is built in [Share index](share-index.md).

`src/protocol` does not open sockets. Login and peer dialing live in `src/session`, described in [Server session](server-session.md). Automated tests do not connect to `server.slsknet.org`.
