---
type: Architecture
title: Soul Sever architecture
description: Crate layout for the terminal Soulseek client.
tags: [architecture, rust, ratatui, soulseek]
status: draft
evidence: implementation-and-current-specifications
generated: { by: "cursor-agent/auto", at: 2026-10-02T23:50:00Z }
verified: { by: "process:cargo test --workspace --all-targets", at: 2026-10-06T15:47:39Z }
sources:
  - id: crate-manifest
    resource: Cargo.toml
    title: Package manifest
  - id: library-root
    resource: src/lib.rs
    title: Library module tree
  - id: config
    resource: src/config.rs
    title: TOML account file
  - id: session
    resource: src/session/mod.rs
    title: Server login task
  - id: shares
    resource: src/shares/mod.rs
    title: Local share index
  - id: quality
    resource: src/quality.rs
    title: Spectral measurement
---

# Architecture

Soul Sever is one Rust package. The library is `soul_sever`. The binary is `soul-sever`.

| Module | Responsibility |
|--------|----------------|
| `src/protocol` | Little-endian Soulseek frames and the messages this crate can encode or decode. No sockets, no Tokio. |
| `src/config.rs` | TOML account file. Load and save. No sockets. |
| `src/session` | Server TCP login, listen socket, at most 100 peer handshakes, search, file transfers, browse, chat, and port mapping. |
| `src/cli.rs` | `--config`, `--bindip`, `--port`, `--rescan`, and `--headless`. |
| `src/shares` | Folder walk, word index, and the filtered share list. The walk uses `spawn_blocking`. A moved download is indexed at its new path. |
| `src/library.rs` | Tag read and the artist / album / song catalog. A finished audio file is filed here, and that path is the one shared. A saved quality verdict rides with the catalog stamp. |
| `src/quality.rs` | Spectral cutoff, lossy percent, and whether a staged file may replace a library file. No sockets, and it does not write the audio file it measures. |
| `src/playback.rs` | Decode one song, play it on the default output device, and publish spectrum bands. |
| `src/settings.rs` | Settings-screen fields and the prefs the session can apply without a new login. |
| `src/app.rs` | View, cursor, search field, `Feed::Preview` or `Feed::Live`, and the last `SessionEvent`. Key handling does not draw. |
| `src/panels.rs` | Session-only panel sizes. A drag grows one panel and returns only to its original size. |
| `src/ui` | Ratatui layout. `draw` reads `App` and does not change it or open a socket. Key `a` draws the quality columns from that state. |
| `src/model.rs` | Display records for transfers, search hits, chat, users, shares, and settings. |
| `src/preview.rs` | Test-fixture rows. The binary does not load them. |
| `src/main.rs` | Argument parsing, terminal setup, event loop. Starts Tokio only when the username is non-empty. |

`src/protocol/distributed.rs` encodes the 5-byte distributed header. `src/session/search.rs` matches the share index and builds `FileSearchResponse`. `src/session/transfers.rs` queues and cancels downloads and uploads. `src/session/browse.rs` turns a share list into browse rows. `src/protocol/chat.rs` encodes room and private messages. `src/session/chat.rs` applies censor, acknowledgements, and auto-reply. `src/session/portmap.rs` maps the listen port with NAT-PMP and then UPnP. `src/cli.rs` loads the config for a rescan or a headless login. Tokio is built with the `fs` feature so a transfer task can read and write the file. Tests enable Tokio's `test-util` feature so a fixture can move that clock.

Dependency direction is `main` → `session`/`ui`/`app` → `config`/`model`, and `protocol` stands alone. `session` may use `protocol` and Tokio. `shares` uses `config`, `protocol`, Tokio's `spawn_blocking`, and lofty. `library` uses lofty and does not open a socket. `quality` uses symphonia and calls `library::shelf` only after a staged replacement is accepted. `session` calls `library::shelf` when a normal download finishes. A quality download is renamed into `incomplete_dir/quality/` and is not shelved on that path. After a library scan is applied, the UI loop starts up to two threads named `quality` and only polls them. `playback` uses symphonia and cpal and is private to the crate. `ui` does not import the wire codec. `protocol` does not import Ratatui or Tokio. The session tells the server the public share counts and answers a share-list request from the index. It also answers an incoming file search from the index.

The client identifies itself as major version **177**, minor **1**. Nicotine+ documents 177 as the major version for unofficial clients and reserves 160 for itself.

One Tokio runtime is built in `main` when `Config.username` is non-empty. That task dials `Config.server_host`. An empty username does not build the runtime and does not open a socket; the sign-in form is the first screen. A saved username starts on an empty shell whose header says `signing in` until the banner arrives. `LoggedIn` clears the `signing in…` hint. `LoggedIn` switches to `Feed::Live`. A failed login returns to the form. A peer `FileSearchResponse` for the current search token is a search hit on that feed, including one read after `PierceFireWall`. A new search closes the previous interactive token. Wishlist tokens stay open.
