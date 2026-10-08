# Implementation Plan: Nicotine+ 3.3.11 parity

## Objective

Make Soul Sever able to log in, share, search, transfer, chat, and manage users the way Nicotine+ 3.3.11 does, using the terminal that already exists. The GTK shell, tray icon, and native file chooser stay out. Those have terminal equivalents (a path field, a bell or desktop notification, an `slsk://` handler) rather than a literal port.

## Success criteria

- With a config file that contains credentials, `soul-sever` logs into a Soulseek server fixture, shows the username and banner, and drops the `OFFLINE PREVIEW` label.
- With no credentials, `soul-sever` still opens the current preview and does not open a socket.
- A user can share folders at public, buddy, and trusted levels, rescan them, and answer a browse and a search from that index.
- Global, room, buddy, user, and wishlist searches return results that the existing search table can render, including the filters already drawn (size, bitrate, duration, free slot, country, extension, include, exclude).
- A download and an upload move bytes, resume from an offset, and land in a finished state. Queue order honors slots, FIFO or round-robin, privileged users, and prioritized buddies.
- Public rooms, private rooms, and private messages send and receive on the fixture, including tickers and an away auto-reply.
- Buddy, ignore, and ban lists survive a restart.
- `cargo test --workspace --all-targets` and `cargo clippy --workspace --all-targets -- -D warnings` pass after every step.
- Automated tests never connect to `server.slsknet.org`. A manual playbook covers one operator login.

## Current state

One package, `soul-sever` 0.1.0, Rust 1.88, edition 2024 (`Cargo.toml`). Dependencies are `ratatui` 0.30.2, `chrono`, `md-5`, and `thiserror`. There is no Tokio runtime and no `TcpStream`.

| Piece | Where | What it does today |
|-------|--------|--------------------|
| Frame codec | `src/protocol/wire.rs` `FrameDecoder`, `encode_frame` | Server frames: `uint32` size, `uint32` code, payload. Default cap 16 MiB. |
| Client messages | `src/protocol/messages.rs` `ClientMessage` | `Login`, `SetWaitPort`, `FileSearch`, `SetStatus`, `Ping`. |
| Login | `LoginRequest`, `decode_login_response`, `MAJOR_VERSION` = 177, `MINOR_VERSION` = 1 | Byte fixture for user `alice`. `Debug` redacts the password. |
| Codes | `src/protocol/codes.rs` modules `server`, `peer_init`, `peer`, `distributed` | Numbers only. A constant is not an encoder. |
| UI state | `src/app.rs` `App::preview`, `Target`, `on_pointer`, `on_scroll` | Keyboard and mouse. Data comes from `src/preview.rs`. |
| Screens | `src/ui/mod.rs` `draw`, `src/ui/views.rs`, `src/ui/dashboard.rs` | Nine views. Header text `OFFLINE PREVIEW` is built in `src/ui/chrome.rs` `header_line`. |
| Display records | `src/model.rs` | `Transfer`, `SearchHit`, `BrowseRow`, `Room`, `ChatLine`, `ListedUser`, `ShareGroup`, `SettingGroup`. |
| Process | `src/main.rs` `event_loop` | Crossterm poll every 200 ms, then `App::on_tick`. |
| Tests | `tests/screen.rs`, protocol unit tests | `make test` / `make clippy` in the `Makefile`. |
| Rules | `knowledge/project/boundaries.md`, `AGENTS.md` §8.3 | No official-server scripting. Version 160 is reserved for Nicotine+. A login claim needs a completed login, not a painted header. |

`App` accessors (`transfers`, `visible_hits`, `browse`, `rooms`, `users`, `shares`, `settings`) are what the widgets read. A live session should keep those accessors and change where the rows come from.

## Proposed approach

Stay in one package. Add `src/session` for sockets and `src/config.rs` for the file on disk. Keep `src/protocol` free of Tokio and Ratatui. Keep `src/ui` free of sockets. `App` holds a `Feed`:

- `Feed::Preview` is today's `src/preview.rs` data. Used when the config has no username.
- `Feed::Live` is a snapshot the session task publishes.

One Tokio runtime, started in `main`. The session task owns the server TCP stream, the listen socket, and peer tasks. It sends `SessionEvent` values on an `mpsc` channel. The UI loop drains that channel and calls `App` methods. Shutdown drops the channel and awaits the task. No detached `tokio::spawn` whose error is ignored.

Peer-init and distributed frames are not the server frame layout. Nicotine+ treats peer-init and distributed messages as 5 bytes or greater (`pynicotine/slskproto.py`). Before writing those codecs, copy the unpack from that file into a byte fixture. Do not guess the layout from the code constant.

Each new message gets an encoder or decoder next to `ClientMessage`, plus a test vector. Unknown codes stay in `ServerFrame` and are ignored by the session.

Config path: `$XDG_CONFIG_HOME/soul-sever/config.toml`, defaulting to `~/.config/soul-sever/config.toml`. Mode `0600` after every write. Fields: username, password, server host and port (default `server.slsknet.org:2242`), listen port (default `2234`), bind address, share folders by level, buddy/ignore/ban lists, auto-join rooms, wishlist, upload slots, queue mode, speed limits. Password remains plaintext in that file, matching the current boundary. `LoginRequest`'s `Debug` stays redacted.

Automated tests use `src/session/fixture.rs`: a localhost TCP peer that speaks the frames we implement. The official server is only in `knowledge/playbooks/login-manual.md` (new), run by a person.

When `Feed::Live` is connected, `header_line` prints the username and banner instead of `OFFLINE PREVIEW`. Preview labeling stays for `Feed::Preview`.

## Boundaries and invariants

- Dependency direction: `protocol` has no UI or Tokio imports. `ui` does not open sockets. `session` may use `protocol` and Tokio.
- `MAJOR_VERSION` stays 177. Do not send 160.
- Do not add protocol messages that are absent from `src/protocol/codes.rs` and from Nicotine+ 3.3.11.
- CI must not resolve or connect to `server.slsknet.org`.
- `unsafe_code` stays forbidden.
- A step is done only when its new test fails if the behavior is removed. Painting a label is not evidence of a transfer or a login.
- Preview mode remains the default with an empty config so `cargo run` and `tests/screen.rs` keep working.
- One runtime. No nested runtime. No lock held across `.await`.
- Peer sockets are bounded: a cap on concurrent peer connections (start at 100, make it config) so a search cannot spawn without limit.

## Implementation steps

### Step 1: Config file

**Files and symbols**

- New `src/config.rs`: `Config`, `load`, `save`
- `src/lib.rs`: `pub mod config`
- `Cargo.toml`: add `toml` and `serde` with `derive`

**Changes**

`Config` round-trips the fields in Proposed approach. Missing file yields `Config::default()` (empty username, listen port 2234, server `server.slsknet.org:2242`). `save` creates the directory and sets file mode `0600` on Unix. Password is skipped by any `Debug` impl.

**Rationale**

Login, shares, and lists need a place to live before a socket is useful. The UI already shows these defaults in `setting_groups`.

**Verification**

`cargo test --lib config::`: load, save, reload; default when the path is missing; file mode `0600`; `Debug` output does not contain the password.

### Step 2: Login session against a fixture

**Files and symbols**

- New `src/session/mod.rs`: `Session`, `SessionEvent`
- New `src/session/fixture.rs`: localhost server
- `src/main.rs` `event_loop`: start the runtime and the session task only when `Config.username` is non-empty
- `Cargo.toml`: add `tokio` with `macros`, `rt-multi-thread`, `net`, `io-util`, `sync`, `time`

**Changes**

`Session::connect` dials the configured host with a 10 second timeout, writes `ClientMessage::Login`, and reads one `FrameDecoder` frame. Success emits `SessionEvent::LoggedIn { banner, ip, supporter }`. Failure emits `SessionEvent::LoginFailed { reason }`. The fixture accepts the alice login vector already in `messages.rs` tests and replies with the success bytes `decode_login_response` already parses. Dropping `Session` closes the socket.

`App` stores the last `SessionEvent` but does not yet replace preview rows.

**Rationale**

`AGENTS.md` §8.3 requires a finished login, not a header string. The fixture keeps that off the public server.

**Verification**

`cargo test --lib session::login_against_fixture`: fixture answers; event is `LoggedIn` with banner `hello` and ip `127.0.0.1`. A second test feeds the `INVALIDPASS` payload and expects `LoginFailed`. A timeout test expects a distinct error when the fixture accepts and sends nothing.

### Step 3: Show the session in the header

**Files and symbols**

- `src/app.rs`: `apply_session`, `connection_label`
- `src/ui/chrome.rs` `header_line`
- `src/main.rs`: drain `SessionEvent` before `on_tick`
- `tests/screen.rs`

**Changes**

Empty username: header stays `OFFLINE PREVIEW` and no task starts. After `LoggedIn`, the header shows the username and banner and does not contain `OFFLINE PREVIEW`. After `LoginFailed`, the header shows the reason and the hints row shows it via the existing `notice` path.

**Rationale**

The screen test already locks the preview wording. This step is the first user-visible session state.

**Verification**

Extend `tests/screen.rs`: preview render still contains `OFFLINE PREVIEW`. A render after `apply_session(LoggedIn)` contains the username and banner and does not contain `OFFLINE PREVIEW`.

### Step 4: Listen port and presence

**Files and symbols**

- `src/session/mod.rs`
- `src/protocol/messages.rs` `ClientMessage::SetWaitPort`, `SetStatus`, `Ping`
- New decode for server `GetUserStatus` (`codes::server::GET_USER_STATUS`) if the fixture sends it

**Changes**

After `LoggedIn`, the session binds `Config.listen_port` (or the next port in a single-port range of one, matching the default 2234), sends `SetWaitPort`, sends `SetStatus` online, and emits `SessionEvent::Listening { port }`. Every 60 seconds it may send `Ping` only if no other write happened. Incoming server frames are decoded in a loop. `Relogged` (`codes::server::RELOGGED`) emits `SessionEvent::Kicked` and stops the task.

**Rationale**

Peers cannot connect until the listen port is registered. These three client messages already encode.

**Verification**

Fixture records the bytes after login and asserts they are `SetWaitPort` for 2234 and `SetStatus` online (`messages.rs` already has those vectors). Bind failure emits `SessionEvent::ListenFailed` and does not send `SetWaitPort`.

### Step 5: Peer connections

**Files and symbols**

- New `src/protocol/peer.rs`: `PierceFireWall`, `PeerInit`, connection-type byte `P` / `F` / `D`
- New `src/session/peers.rs`
- `src/protocol/codes.rs` `peer_init::PIERCE_FIRE_WALL`, `PEER_INIT`

**Changes**

Confirm the 5-byte-minimum peer-init layout in Nicotine+ `slskproto.py` and freeze it as a fixture before any other peer work. `GetPeerAddress` (`codes::server::GET_PEER_ADDRESS`) asks the server for a username. The session tries a direct TCP connect, then `ConnectToPeer` (`codes::server::CONNECT_TO_PEER`) and accepts the inbound `PierceFireWall` on the listen socket. After init, peer message frames use the server-style 8-byte header with `codes::peer` values. Cap in-flight peer tasks.

**Rationale**

Search results, browses, and transfers all arrive on peer sockets. The code constants already exist and the layout does not.

**Verification**

Two fixture processes: one plays the server and answers `GetPeerAddress` with the other's address; the other completes `PeerInit`. The test asserts both directions and that a refused direct connect falls back to the indirect token. A test that claims a peer-init size below the Nicotine+ minimum fails the decoder.

### Step 6: Share index

**Files and symbols**

- New `src/shares/mod.rs`: `ShareIndex`, `rescan`
- `src/config.rs`: three folder lists
- `src/protocol/peer.rs`: `SharedFileListRequest` / `SharedFileListResponse` (`codes::peer::SHARED_FILE_LIST_REQUEST`, `SHARED_FILE_LIST_RESPONSE`)
- `Cargo.toml`: add `lofty` for audio tags

**Changes**

`rescan` walks configured folders on `spawn_blocking`. Each file records path, size, and, when lofty can read them, bitrate, duration, VBR, sample rate, and bit depth using `FileAttribute` numbers from Nicotine+ (0, 1, 2, 4, 5). Files are tagged public, buddy, or trusted from the folder that contained them. A word index maps normalized tokens to file ids. Exclude patterns in config drop matches before insert. `SharedFileListResponse` serves only the files the requester is allowed to see. Buddy and trusted checks use the lists from config.

**Rationale**

Search responses and browses are views of this index. The shares screen already has the three groups and currently shows zero files.

**Verification**

Temp directory with one text file and one tiny tagged audio file if a fixture file is checked in under `tests/fixtures/`. `rescan` counts both. A buddy-only file is absent from a public `SharedFileListResponse` and present for a username on the buddy list. Word lookup for a token in the filename returns that file.

### Step 7: Search

**Files and symbols**

- `src/protocol/messages.rs` `FileSearchRequest`, `decode_incoming_file_search`
- New decoders for `RoomSearch` (`codes::server::ROOM_SEARCH`), `UserSearch` (`codes::server::USER_SEARCH`), `WishlistSearch` (`codes::server::WISHLIST_SEARCH`), `WishlistInterval` (`codes::server::WISHLIST_INTERVAL`)
- New `src/protocol/distributed.rs` for `DistribSearch`, `DistribBranchLevel`, `DistribBranchRoot` after confirming the distributed frame layout in `slskproto.py`
- New `src/session/search.rs`
- `src/app.rs` search field and `SearchMode`
- `src/model.rs` `SearchHit`

**Changes**

Enter on the search view sends the matching server message instead of setting the notice `server connection is not implemented`. Results arrive as peer `FileSearchResponse` (`codes::peer::FILE_SEARCH_RESPONSE`) and append to the live hit list. Filters already labeled in `draw_search` (size, bitrate, duration, free slot, country, extension, include, exclude) apply to that list. Wishlist entries in config rotate one item per `WishlistInterval`. Our own index answers incoming searches through the word index, capped by config `max_results` (default 300) and `min_search_chars` (default 3).

Distributed parent/child: handle `PossibleParents`, `BranchLevel`, `BranchRoot`, `HaveNoParent`, and `AcceptChildren`. Relay `DistribSearch` to children and answer from the index. The dashboard network panel reads parent and child counts from this state.

**Rationale**

`FileSearch` encode and the search screen already exist. This step connects them and adds the modes the mode line already cycles.

**Verification**

Fixture peer returns one `FileSearchResponse`. The hit shows in `visible_hits` and honors an exclude token. A wishlist item is sent only after the interval message. An incoming search for a shared token produces a response; a one-character query produces none.

### Step 8: Transfers

**Files and symbols**

- New `src/session/transfers.rs`
- `src/protocol/peer.rs`: `QueueUpload`, `TransferRequest`, `TransferResponse`, `PlaceInQueueRequest`, `PlaceInQueueResponse`, `UploadDenied`, `UploadFailed` (`codes::peer`)
- `src/model.rs` `Transfer`, `TransferState`
- `src/ui/dashboard.rs`, `src/ui/views.rs` `draw_transfers`

**Changes**

Choosing a search hit or a browse row queues a download: `QueueUpload` to that peer, then a file connection (`F`) on `TransferRequest`. Bytes append to the incomplete path from config and move to the download folder when the count matches the size. A second attempt sends the current length as the offset. Upload side: at most `upload_slots` (default 2) active sends; queue mode FIFO or round-robin; privileged users and prioritized buddies jump the queue; file-count and byte caps reject with `UploadDenied` and the Nicotine+ reason strings (`Queued`, `Too many files`, `Too many megabytes`, `File not shared.`). Speed limits, when set, pace writes. `TransferState` values already in `model.rs` are set from these outcomes. Dashboard rates use real byte deltas instead of `preview::rate_at` while `Feed::Live`.

**Rationale**

The transfer tables, states, and progress bars already render. They need a byte stream.

**Verification**

Fixture upload of a known 64 KiB payload: client file bytes match, state becomes `Finished`. A socket close at 1 KiB, then a retry, resumes at offset 1024 and still matches. A third queued upload waits while two slots are busy. A banned username receives `UploadDenied` with `Banned`.

### Step 9: Browse, user info, and lists

**Files and symbols**

- `src/protocol/peer.rs`: `UserInfoRequest`, `UserInfoResponse`, `FolderContentsRequest`, `FolderContentsResponse`
- Server messages `WatchUser`, `GetUserStatus`, `GetUserStats`, `AddThingILike`, `UserInterests`, `SimilarUsers`, `Recommendations` (codes already in `codes::server`)
- `src/model.rs` `BrowseRow`, `ListedUser`
- `src/config.rs` buddy, ignore, and ban lists
- `src/ui/views.rs` `draw_browse`, `draw_users`

**Changes**

Browse opens a peer and fills `BrowseRow` from `SharedFileListResponse` or `FolderContentsResponse`. User info fills the detail pane. Watch list tracks online, away, and offline. Adding or removing a buddy, ignore, or ban updates config on the same key the UI already uses (`n` notify, `p` priority, `t` trusted) and is written through `Config::save`. Ignore and ban drop that user's search hits and reject their uploads.

**Rationale**

The users and browse screens already have columns for these fields.

**Verification**

Fixture browse of two files builds two `BrowseRow`s. Toggling trusted on a buddy persists across `load`. A banned peer's search hit is removed and their `QueueUpload` is answered with `Banned`.

### Step 10: Chat

**Files and symbols**

- New `src/session/chat.rs`
- Server messages `JoinRoom`, `LeaveRoom`, `SayChatroom`, `UserJoinedRoom`, `UserLeftRoom`, `MessageUser`, `MessageAcked`, `RoomList`, ticker messages `RoomTickerState`, `RoomTickerAdd`, `RoomTickerSet`, private-room messages `133`–`148` (`codes::server`)
- `src/model.rs` `Room`, `ChatLine`
- `src/ui/views.rs` `draw_chat`
- `src/config.rs`: auto-join, auto-away minutes, auto-reply, log paths

**Changes**

Join sends `JoinRoom` and renders members and lines into the existing room panes. Say sends `SayChatroom`. Private messages send `MessageUser` and `MessageAcked` on receipt. Tickers use the ticker line already drawn. Auto-join runs after `LoggedIn`. Auto-away sends `SetStatus` away after the configured idle minutes. Auto-reply sends the configured string when away. Room and private logs append to the configured directories when logging is on. Censor and word-replace tables in config apply before display and before send.

**Rationale**

The chat layout (rooms, transcript, user list, ticker, input hint) is already in `draw_chat`.

**Verification**

Fixture joins `preview-music`, delivers one line, and the `ChatLine` contains that text. `MessageUser` is acked with `MessageAcked`. A censored token is replaced in the rendered line. Auto-reply is sent only while status is away.

### Step 11: Reachability and CLI

**Files and symbols**

- New `src/session/portmap.rs`
- `src/main.rs` argument parser
- `knowledge/playbooks/login-manual.md` (new)
- `knowledge/playbooks/index.md`

**Changes**

When config `upnp` is true, try NAT-PMP, then UPnP, for the listen port, and renew on a timer. Failure is a `SessionEvent` and does not drop the server connection. CLI, in addition to `--help` and `--version`: `--config PATH`, `--bindip ADDR`, `--port PORT`, `--rescan`, `--headless`. `--headless` runs the session and logs events to stderr without Ratatui. `--rescan` builds the index and exits 0, or exits 1 if a folder is unreadable. The manual playbook states that a person may point config at `server.slsknet.org` and what the header looks like after success. Tests do not follow that playbook.

**Rationale**

Nicotine+'s documented flags and port mapping are the remaining operational surface once transfers work. Headless is how a rescan and a future service mode run without a tty.

**Verification**

Argument parser unit tests for each flag. A fixture NAT-PMP UDP responder records a map request for port 2234. `--rescan` on a temp config exits 0 and the index file or in-memory count matches the temp tree. `--headless` against the login fixture prints the banner and exits 0.

### Step 12: Live feed on every view

**Files and symbols**

- `src/app.rs` `Feed`
- `src/preview.rs` unchanged as the preview feed
- `src/ui/dashboard.rs` meters
- `tests/screen.rs`

**Changes**

`Feed::Live` supplies transfers, hits, browse rows, rooms, users, and share counts to the existing accessors. `Feed::Preview` stays on the current sample rows. Dashboard meter samples come from transfer byte deltas in live mode and from `rate_at` in preview mode. No view still mixes a live login with `preview-` usernames.

**Rationale**

Steps 2–11 can land one screen at a time. This step removes the last preview rows from a connected session so the UI cannot lie.

**Verification**

Screen test: live feed with one real transfer row does not contain `preview-alice`. Preview feed still contains `preview-alice` and `OFFLINE PREVIEW`.

## Testing strategy

- Keep every current protocol vector and `tests/screen.rs` preview assertion green.
- Add fixture tests beside the session modules. Fixtures bind `127.0.0.1:0`.
- Failure paths required at the step that introduces them: bad password, timeout, listen bind failure, direct peer refused, resume after a short write, banned upload, censored chat.
- Boundary: empty config, empty share folder, search shorter than 3 characters, zero-byte file, transfer size that crosses the 4-byte length fields already handled by the codec.
- Do not add a test that dials a public host. Review `tests/` for `server.slsknet.org` before calling a step done.

## Validation commands

```shell
cargo test --workspace --all-targets
cargo clippy --workspace --all-targets -- -D warnings
```

`make test` and `make clippy` call those. Run both at the end of each step.

## Risks and mitigations

- Peer-init and distributed framing differs from server frames. Mitigation: fixture taken from Nicotine+ `slskproto.py` before any peer socket merges.
- Share scans can block the runtime. Mitigation: `spawn_blocking`, and the UI keeps drawing.
- Search fan-out can open too many sockets. Mitigation: the connection cap in Step 5.
- A live header that still shows preview rows would look like a real network. Step 12 forbids that mix, and the screen test locks it.
- Official server rules disallow scripted logins. The fixture is the automated proof. The manual playbook is the only official-server path.

## Out of scope

- Implementing a Soulseek server.
- Protocol obfuscation. Nicotine+ 3.3.11 does not speak it.
- Importing an existing Nicotine+ config file.
- GTK tray icon, native file chooser, and a bundled audio player. Terminal equivalents are the path field, a notification, and opening a file with the system handler.
- Embedding CPython. See open decisions.
- Sending major version 160.

## Open decisions

### Plugin host

Nicotine+ plugins are Python, with an event API and `/help` commands. `knowledge/project/boundaries.md` says embedding CPython is not part of the first network slice. Full parity still includes plugins, and the host changes packaging (a Python runtime, the plugin API surface, and how `/help` is dispatched).

- Options: embed CPython with PyO3 and mirror the Nicotine+ events; or ship a Rust command registry that covers `/help` and the same events without loading Python files.
- Recommended default: finish Steps 1–12 first. Then add the Rust command registry so `/help` works, and treat loading `.py` plugins as a follow-on only if you want bytecode-compatible Nicotine+ plugins.
- Consequence of the default: a Nicotine+ `.py` plugin will not load after this plan. Chat and transfer behavior still match. The Python host remains a later plan.

No other decision blocks Steps 1–12.

## Final quality check

- New paths are marked new: `src/config.rs`, `src/session/`, `src/shares/`, `src/protocol/peer.rs`, `src/protocol/distributed.rs`, `knowledge/playbooks/login-manual.md`.
- Every step has a verification command or a named test.
- Existing symbols cited above are in the tree today.
- Plugin hosting is the only open decision, and it is not an implementation step.
