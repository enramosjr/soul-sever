---
type: Capability
title: Share index
description: Local scan of configured folders and the filtered Soulseek share list.
tags: [soulseek, shares, index]
status: draft
evidence: validated-runtime
generated: { by: "cursor-agent/auto", at: 2026-10-03T01:10:00Z }
verified: { by: "process:cargo-test", at: 2026-10-04T20:56:53Z }
sources:
  - id: shares
    resource: src/shares/mod.rs
    title: ShareIndex and rescan
  - id: share-list
    resource: src/protocol/peer.rs
    title: SharedFileListRequest and SharedFileListResponse
  - id: config
    resource: src/config.rs
    title: Public, buddy, trusted folders and exclude globs
  - id: tone
    resource: tests/fixtures/tone.wav
    title: Tiny WAV used by the rescan test
---

# Share index

`rescan` copies the share folders and walks them inside `spawn_blocking`. A configured folder that cannot be read fails the scan. Symlinks inside a folder are skipped. `shares.exclude` globs are matched against the file name, and a match is dropped before insert. `*` is the only wildcard.

Each file keeps its path, size, and the level of the folder that contained it: public, buddy, or trusted. lofty 0.24 supplies duration, sample rate, and bit depth when it can read them. Bitrate is included for a file with no bit depth. Attribute numbers match Nicotine+: bitrate 0, duration 1, VBR 2, sample rate 4, bit depth 5. Lossless files omit bitrate and VBR. lofty's `FileProperties` does not report VBR, so a scan omits attribute 2.

A word index maps a lowercased filename token to file ids. `lookup` of one token returns those files.

`store_moved` drops the pre-move path and any previous copy of the filed path, then indexes the file at the new path. The virtual directory is the share root's folder name plus the relative directories, the same shape a walk builds. The file keeps the level of the longest configured root that contains it. When no root contains it, the download folder is appended to the public shares and the file is public. A name that matches `shares.exclude` is not inserted. A symlink is not inserted.

`list_for` builds a `SharedFileListResponse`. Everyone sees public files. A buddy also sees buddy files. A trusted buddy sees buddy and trusted files. Folders that requester cannot browse are the locked list. After a scan, and again after `store_moved`, the session sends server code 35, `SharedFoldersFiles`, with the public folder count and then the public file count. The upload list uses the new disk path. A peer `SharedFileListRequest` is answered with that filtered list. `UserInfoRequest` is answered with an empty description, no picture, a queued-upload count, a free-slot flag, and upload permission for everyone. `FolderContentsRequest` is answered with the directories that user can browse; a directory they cannot browse is an empty folder list. A failed rescan does not send a new count. The shares screen lists the configured paths. Enter opens a folder browser. `s` shares the open folder and `Session::rescan` replaces the index. A folder that cannot be read fails that rescan and leaves the previous index in place.
