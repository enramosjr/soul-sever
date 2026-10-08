---
type: Historical Record
title: Library and playback slice
description: The slice that files downloads by tags and plays them with a spectrum meter.
tags: [library, playback, history]
status: draft
evidence: validated-runtime
generated: { by: cursor-agent/auto, at: 2026-10-04T20:44:52Z }
sources:
  - id: src-library-rs
    resource: ../../src/library.rs
    title: Tag filing
  - id: src-playback-rs
    resource: ../../src/playback.rs
    title: Playback
verified: { by: "process:cargo test --workspace --all-targets", at: 2026-10-05T18:56:48Z }
---

# Library and playback slice

Delivered with the library screen. A tagged download of `Basket Case` finishes at `download_dir/Green Day/Dookie/01 Basket Case.wav`. The play view lists that artist, both of that artist's albums, and the song. Enter starts playback, and the braille spectrum draws a full cell. That filed path is the share-index path and the path announced with `SharedFoldersFiles`. The playback screen puts the meter on the left and the track details on the right, hides the log, and uses that pane for a seek bar and icon-sized play, pause, and stop marks. A click on a song starts it after a short decode, and the rest of the file decodes while it plays. While a song is playing the meter redraws about every 33 ms and eases between frames. Artist and album names that differ only by case are one library entry, and the chosen spelling is written back into the tag. The output stream stays open until playback stops, so a click that says playing is actually sending samples. `x` on the library asks before deleting the highlighted artist, album, or song from disk. The folder name is the album artist. Guest wording, including `with`, `f`, and `w`, does not create an artist folder. `Anthony Green feat. Prentiss` is filed under `Anthony Green`. `Simon & Garfunkel` stays one artist, and a shorter `Simon` on that album joins the duo. A file left in a guest folder is renamed on the next library scan, and the emptied folder is removed with that move. The catalog is written to `library.toml` beside the account file and shown on the next launch before the music folder is read. Unchanged files keep their saved names. While a song is playing, organizing a finished download scans the library off the meter thread. The rename itself runs on a thread named `move`, so a finished download does not stall the download meter.

Verified by `cargo test --workspace --all-targets` → PASS (205 library tests, 29 `tests/screen.rs` tests, including `a_saved_library_is_on_screen_before_the_folder_is_reread`, `a_cached_library_reads_tags_only_for_files_that_changed`, `a_guest_folder_is_renamed_to_the_album_artist`, `guest_wording_files_under_the_album_artist_without_a_sibling`, `an_album_artist_tag_overrides_the_track_artist`, `a_library_relocation_replaces_the_share_path`, `a_tight_feature_credit_joins_the_artist_already_on_the_album`, `a_feature_credit_files_under_the_primary_artist_and_album`, and `the_library_nests_albums_under_the_artist_and_draws_the_spectrum`). `cargo clippy --workspace --all-targets -- -D warnings` → PASS. No test dials `server.slsknet.org`.
