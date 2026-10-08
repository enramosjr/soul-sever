---
type: Evidence Policy
title: Evidence and lifecycle
description: How claims about Soul Sever are allowed to be written down.
tags: [evidence, okf, testing]
status: stable
evidence: project-documentation
generated: { by: "cursor-agent/auto", at: 2026-10-02T23:50:00Z }
verified: { by: "process:repository-scan", at: 2026-10-02T23:50:00Z }
sources:
  - id: agents
    resource: AGENTS.md
    title: Evidence hierarchy and test matrix
---

# Evidence and lifecycle

1. Source under `src/` is the strongest evidence of behavior.
2. `Cargo.toml`, `Cargo.lock`, and `Makefile` are evidence of the toolchain.
3. A passing `cargo test` or `cargo clippy` command is verification evidence. Name the command.
4. Docs and the preview UI are not evidence of a network session.

`verified.by: process:repository-scan` means the files were read. It does not mean a Soulseek login succeeded.

A feature slice is done when the behavior works, unit tests cover the pure logic, an end-to-end check fails if the user-visible result is missing, and this corpus quotes that command. See `AGENTS.md` §8.
