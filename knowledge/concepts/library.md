---
type: Concept
title: "Artist, album, and song"
description: "A song is an artist, an album, and a title, and may carry a saved spectral verdict for that file stamp."
tags: [library, tags, catalog]
status: draft
evidence: validated-runtime
generated: { by: cursor-agent/auto, at: 2026-10-04T20:44:52Z }
sources:
  - id: src-library-rs
    resource: ../../src/library.rs
    title: Catalog grouping
  - id: src-quality-rs
    resource: ../../src/quality.rs
    title: Saved spectral verdict
verified: { by: "process:cargo test --offline --workspace --all-targets -- --test-threads=1", at: 2026-10-08T20:24:28Z }
---

# Artist, album, and song

A song in the library has an artist, an album, a title, a track number, and a path. The catalog groups songs by artist, then by album. Albums are ordered by name. Songs in an album are ordered by track number, and a missing track number sorts after numbered songs. Tags win over the folders already on disk. A file with no tags takes the artist and album from the two directories above it, or `Unknown Artist` and `Unknown Album` when those directories are absent.

The folder artist is the album artist. An AlbumArtist tag (`TPE2`, Vorbis `ALBUMARTIST`, or MP4 `aART`) wins over the track artist. Guest wording is cut out of that credit: `featuring`, `feat.`, `feat`, `ft.`, `ft`, `with`, `w/`, `w.`, `w`, `f/`, `f.`, `f`, `vs.`, `vs`, and `versus`, including when the guest name sits against the period (`feat.Miranda`). `Drake with X`, `Drake f/ X`, and `Drake w X` are `Drake`. `Avalon (feat. Someone)` and `Avalon (with Someone)` are `Avalon`. A collaborative credit stays one name when the shorter name is not already an artist on a different album. `Simon & Garfunkel` stays, and a shorter `Simon` on that same album adopts the duo. `Coheed and Cambria` stays. `Nine Inch Nails & David Bowie` and `Nine Inch Nails and David Bowie` join `Nine Inch Nails` when that artist already has another album. `and`, `&`, `+`, and `x` count as that extension. `Green Day` and `Radiohead` on one album title stay two artists. The song title keeps its own credit. The chosen artist is written into the artist tag and the album-artist tag. A file already sitting in a guest folder is renamed into the album-artist folder on the next library scan. An emptied guest folder is removed when its last file moves. The catalog is saved as `library.toml` beside the account file. The next launch shows that list before the music folder is read. A file whose size and modification time match the saved stamp keeps its names until the chosen artist or album changes, and tags are read only when a file arrives, leaves, or is rewritten.

Artist and album names that differ only by case, or by an apostrophe or dash that looks like another, are one name. `Jack's Mannequin` and `Jack’s Mannequin` are one artist. The spelling used by the most files wins. A tie keeps the spelling with more lowercase letters, so `Hail the Sun` wins over `Hail The Sun`, and then the spelling with more ASCII characters. A mixed-case name capitalizes a fully lowercase word, so `Nine inch Nails` is written `Nine Inch Nails`. A small word in the middle stays lowercase. A single letter stays as written. When every spelling is all capitals or all lowercase, the written form is title case and small words stay lowercase (`Hail the Sun`). A name that has only one spelling is otherwise left unchanged, including a lone `RADIOHEAD`. Album names are reconciled inside one artist. The chosen album is written back when that tag was already present. A new download adopts the spelling already in the library and is filed under that folder. A quality replacement is filed under the original song's artist, album, title, and track.

Each saved song can also carry a quality record for that path's size and modification time: bit depth, sample rate, lossy percent, the highest frequency that still had energy, duration, a verdict of lossless, lossy, or unmeasured, and whether an upgrade was already attempted. An older `library.toml` that has no quality field still loads. A library check that sees the same stamp keeps the record. A changed file is measured again. The verdict is display data. It is not an audio-file tag.

The on-disk layout written at the end of a tagged download is the same tree: artist directory, album directory, then `NN Title.ext`. A library scan that renames a guest folder uses the file's existing name under the resolved artist and album. That file is the share-index entry and the path advertised to the network. The path it moved from is not. Deleting an artist, album, or song from the library removes those files from disk and drops them from the share index. Empty album and artist folders go with them. A path outside the download folder is left alone.
