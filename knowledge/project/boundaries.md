---
type: Product Boundary
title: Soul Sever boundaries
description: What this client will and will not do on the Soulseek network.
tags: [boundaries, soulseek, protocol]
status: draft
evidence: project-documentation
generated: { by: "cursor-agent/auto", at: 2026-10-02T23:50:00Z }
verified: { by: "process:cargo test --workspace --all-targets", at: 2026-10-06T01:20:44Z }
sources:
  - id: nicotine-protocol-notes
    resource: src/protocol/messages.rs
    title: Version constants and login encoding
  - id: keyring
    resource: src/secrets.rs
    title: System keyring for the account password
---

# Boundaries

- Soul Sever is a client. It does not implement a Soulseek server.
- Do not invent protocol messages. New codecs follow Nicotine+ `slskmessages.py` layouts and land with byte tests.
- Do not send major version 160. That number belongs to Nicotine+.
- Login passwords are MD5-hexed the way the official protocol requires. That hash is not a modern password store. The account password is stored in the system keyring: Secret Service on Linux, Keychain on macOS, Credential Manager on Windows. The config file does not contain it. A file that still has a `password` field is moved into the keyring and rewritten without that field on load. `Config` and `LoginRequest` redact it in `Debug`. The process does not wipe the password from memory. The account file remains mode `0600`.
- There is no offline Soulseek catalog. The library view reads files already in the download folder; it does not stand in for a server search. An empty username shows the sign-in form and does not open a socket. Soulseek login creates the account when the name is unused. A failed or timed-out login returns to that form with the reason. After `LoggedIn`, the header shows the username and banner.
- Automated tests bind `127.0.0.1`. They do not connect to `server.slsknet.org`. An empty username does not open a socket. A configured username dials only `Config.server_host`.
- Plugin compatibility with Nicotine+'s Python plugin API is a product goal, not a promise of embedding CPython in the first network slice.
