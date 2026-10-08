---
type: Development Guide
title: Building Soul Sever
description: Toolchain and commands for the Rust client.
tags: [development, rust, cargo]
status: stable
evidence: validated-runtime
generated: { by: "cursor-agent/auto", at: 2026-10-02T23:50:00Z }
verified: { by: "process:cargo test --workspace --all-targets", at: 2026-10-06T01:20:44Z }
sources:
  - id: makefile
    resource: Makefile
    title: Test and clippy targets
  - id: manifest
    resource: Cargo.toml
    title: Rust 1.88 package manifest
  - id: config
    resource: src/config.rs
    title: Config path, load, and save
  - id: keyring
    resource: src/secrets.rs
    title: System keyring for the account password
---

# Development

Rust 1.88 or newer. Ratatui 0.30 declares that floor.

```shell
cargo test --workspace --all-targets
cargo clippy --workspace --all-targets -- -D warnings
cargo run
```

`make test`, `make clippy`, and `make run` call those commands. Interface addresses for the bind-address popup come from the `if-addrs` crate.

The screen suite is `tests/screen.rs`. It renders with `TestBackend` and fails if a view marker is missing. It does not open a terminal device. The download-retry fixture pauses Tokio's clock through the `test-util` feature.

## Config file

`Config::default_path` is `$XDG_CONFIG_HOME/soul-sever/config.toml`, or `~/.config/soul-sever/config.toml` when `XDG_CONFIG_HOME` is unset. A missing file loads `Config::default`: empty username, listen port `2234`, server `server.slsknet.org:2242`. `Config::save` creates parent directories and, on Unix, sets the file mode to `0600`. The password is written to the system keyring under the service name `soul-sever` and the Soulseek username, and `skip_serializing` keeps it out of the account file. A changed password, a cleared password, or a renamed username updates that entry. Other saves leave the keyring alone. An account file that still contains `password` is moved into the keyring and rewritten on load. `queue.toml` in that same directory is the transfer list. It is not part of the account file. `Debug` prints `Password(********)`.

`cargo run` reads that path before the terminal starts. An empty username opens the sign-in form and does not start a socket. A non-empty username starts the login task against the host in the file, with empty lists until the session fills them. Unit tests pass an explicit path or `127.0.0.1` and do not dial `server.slsknet.org`.

`cargo test --lib config::` covers load, save, reload, the missing-file default, mode `0600`, password redaction, and that the password stays out of the account file. Those tests use an in-memory stand-in for the keyring. `cargo test --lib session::login_against_fixture` covers the localhost login.

## Share index

`shares.exclude` is a list of file-name globs. `*` is the wildcard. An empty list is the default, so an older config file still loads. `rescan` walks the three folder lists inside `spawn_blocking`. lofty 0.24 reads audio tags. Playback decodes with symphonia 0.5 (wav, flac, mp3, vorbis, aac, alac, isomp4) and writes f32 samples with cpal 0.15. The device starts after a short preroll, and the rest of the file decodes while it plays. A machine without an f32 output device still moves the spectrum on the UI tick. `SharedFileListResponse` is zlib-compressed with `flate2`. After a scan the session sends that list when a peer asks, and tells the server the public folder and file counts.

`max_results` defaults to 300 and `min_search_chars` defaults to 3. Zero is rejected. `wishlist` is a list of queries. The session sends one of them after each `WishlistInterval`, and none at login.

`incomplete_dir` and `download_dir` default to empty strings. A download with either unset is rejected before a peer write. `queue_file_limit` defaults to 100 and `queue_megabytes` defaults to 10000. Zero for either is a validation error. `upload_slots` defaults to 2. Tokio's `fs` feature is enabled for the file copy.

`auto_away_minutes` defaults to 0, which stays online. `auto_reply` defaults to empty, which sends no reply. `log_rooms` and `log_private` default to false. `room_log_dir` and `private_log_dir` default to empty. `censor` and `replace_words` are lists of `from` / `to` pairs; an empty `from` is a validation error. Censor runs before word replacement, on displayed lines and on text the client sends.

`upnp` defaults to false. `upnp_gateway` defaults to empty, which means the default route and then UPnP. A `host:port` value is NAT-PMP only. `--config`, `--bindip`, `--port`, `--rescan`, and `--headless` are accepted with `--help` and `--version`. `--rescan` prints `shares: N` and exits 0, or exits 1 when a folder cannot be read. `--headless` writes session events to stderr and does not draw.
