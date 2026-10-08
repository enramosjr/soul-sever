---
type: Playbook
title: Run the Soul Sever terminal
description: Start the terminal, sign in, and leave it cleanly.
tags: [playbook, tui]
status: stable
evidence: implementation-and-current-specifications
generated: { by: "cursor-agent/auto", at: 2026-10-02T23:50:00Z }
verified: { by: "process:cargo test --workspace --all-targets", at: 2026-10-06T01:20:44Z }
sources:
  - id: binary
    resource: src/main.rs
    title: Process entry and event loop
---

# Run the terminal

1. Install Rust 1.88 or newer.
2. From the repository root, run `cargo run` or `make run`. `--config PATH` selects the account file. `--bindip` and `--port` override the listen socket for that run. `--rescan` prints the share count and exits. `--headless` logs the session to stderr without the terminal. A person who wants the official server follows [Log in by hand](login-manual.md). Tests do not.
3. The terminal opens before the download folder is read. A saved `library.toml` beside the account file is the library on the first frame. The folder is checked after that, and tags are read only for files that changed. With no config file, or with an empty username, the header says `sign in`. Type the Soulseek username, press Enter, type the password, and press Enter. A name that is not already registered creates a new Soulseek account. The form says that in the title style on two lines, and the line under it says an existing name signs in with that account's password. The password is shown as `*` and stored in the system keyring. The username is written to the config file. Nothing is sent until that second Enter. A saved username skips the form, shows `signing in` on an empty shell, and logs in to the host in the file. A successful login replaces the header with the account and the server banner and clears the `signing in…` hint. A rejected login returns to the form with the server's reason.
4. Click a tab or a row, or roll the wheel over a list. Key `0` opens the library of finished downloads. The meter is at the top left and the track details are at the top right. Enter or a click on a song plays it. `x` asks before deleting the highlighted artist, album, or song from disk. The controls under the columns are a small triangle for play, two bars for pause, and a square for stop. A click starts the song after a short decode. Drag the bar to move through the song. Space pauses. `s` stops. While a song is playing the meter refreshes often enough to follow the sound. The log is hidden on this view. On the other views the log records a browse of your files, a search of your files, the listen port, and a finished transfer. `` ` `` or a click on the log title collapses it to the latest line. The `?` control at the lower right opens help. A click closes help. On Search, `d` queues a download of the selected hit when logged in. Download and upload progress is a braille rail in the same green or blue as the meters. On Users, `n`, `p`, and `t` save buddy flags, and Enter asks for that user's share list when logged in. On Chat, type a message. The line under the room shows each character as you type. Enter sends it when logged in. Esc stops typing and leaves the text on that line. Arrow keys move between rooms. Enter on a room, when you are not typing, joins it. A click on a room joins it too. A click on a person in the room opens a card. Browse files asks for their share list. Add friend saves them as a buddy. Ignore and ban save those lists. Esc closes the card.
5. Press `q` or `ctrl-c` to exit. The process disables mouse capture and leaves the alternate screen.

`soul-sever --help` and `soul-sever --version` print and exit 0. An unknown argument exits 2.

The automated stand-in for a visual pass is `cargo test --test screen`, which draws each view into a 120×40 buffer.
