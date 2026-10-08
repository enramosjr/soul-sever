---
type: Historical Record
title: Foundation slice
description: First Soul Sever commit surface — offline terminal and initial codec.
tags: [history, foundation]
status: draft
evidence: validated-runtime
generated: { by: "cursor-agent/auto", at: 2026-10-02T23:50:00Z }
verified: { by: "process:cargo-test", at: 2026-10-03T00:20:00Z }
sources:
  - id: screen-suite
    resource: tests/screen.rs
    title: View rendering tests
  - id: login-test
    resource: src/protocol/messages.rs
    title: Login frame fixture
---

# Foundation

On 2 October 2026 the repository held agent governance files and no Rust package. This slice added the package, the nine-view terminal, and the first Soulseek messages.

`cargo test --workspace --all-targets` passed: 27 library tests and 4 screen tests. `cargo clippy --workspace --all-targets -- -D warnings` passed. No server handshake was attempted.

Later the same day, the config file and a localhost login landed. `cargo test --workspace --all-targets` passed with 40 library tests and 7 screen tests. The fixture accepted the `alice` login frame and returned banner `hello`. That run did not contact `server.slsknet.org`.
