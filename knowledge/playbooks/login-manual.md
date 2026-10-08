---
type: Playbook
title: Log in to the Soulseek server by hand
description: How a person points the config at server.slsknet.org and reads the header.
tags: [playbook, login, soulseek]
status: draft
evidence: implementation-and-current-specifications
generated: { by: "cursor-agent/auto", at: 2026-10-03T01:51:12Z }
verified: { by: "process:cargo test --workspace --all-targets", at: 2026-10-06T01:20:44Z }
sources:
  - id: header
    resource: src/app.rs
    title: Connection label after LoggedIn
  - id: config
    resource: src/config.rs
    title: Server host and port
---

# Log in by hand

Automated tests do not follow this playbook. They bind `127.0.0.1` and do not dial `server.slsknet.org`.

1. Run `soul-sever`. If the config has no username, the sign-in form asks for one and a password. The password is stored in the system keyring. The username is saved in the account file. The default server is `server.slsknet.org:2242`. You can also write `server_host` and `server_port` in the config file before launching.
2. Run `soul-sever`, or `soul-sever --config PATH`. `--bindip` and `--port` change the listen socket for that run. They do not change the server host.
3. After the server accepts the login, the header shows `username · banner`, using the account name and the banner from the server. Lists stay empty until the session fills them.
4. Press `8` for shares. `j` and `k` pick public, buddy, or trusted. Enter opens a folder browser in your home directory. Enter opens a directory, `s` shares the folder you are in, and Esc closes the browser. `[` and `]` mark a listed folder, and `x` removes it. Dashboard meters follow transfer progress. Press `9` for settings. `j` and `k` pick a section, `[` and `]` pick a field, and Enter edits it. Slots and speed limits apply immediately. Server, port, and account changes ask for a restart. An empty username shows the sign-in form. A name that is not already registered creates a new Soulseek account. The form says that in the title style.

A failed login returns to that form and shows the server's reason in the header. `logged in elsewhere` means the server sent `Relogged`.
