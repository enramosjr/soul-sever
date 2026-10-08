# Soul Sever

A terminal client for the [Soulseek](https://www.slsknet.org/) network, written in Rust.

Search, browse shares, chat, and transfer files from one screen. Finished downloads are filed into a library and can be played in the client. Each song is measured for spectral quality, and a weaker file can be replaced by a better copy from the network.

## Features

- Network, room, buddy, user, and wishlist search
- Download and upload queues
- Browse another user's files
- Chat rooms
- Shared folders
- Library and playback for finished downloads
- Spectral quality check, with a second check from the quality view
- Mouse and keyboard control

## Building

[Rust](https://www.rust-lang.org/) 1.88 or newer.

```shell
cargo build --release
```

The binary is `target/release/soul-sever`. From a checkout, `cargo run` or `make run` builds and starts it.

## Usage

```shell
soul-sever
```

An empty username opens the sign-in form. Nothing is sent until you sign in. A name that is not already registered creates a Soulseek account. The password is stored in the system keyring. A saved username logs in to the server written in the config file.

The terminal needs at least 70 columns and 18 rows. Press `?` for keys. `q` or Ctrl-C quits.

| Key | View |
|-----|------|
| `1` | Dashboard |
| `2` | Search |
| `3` | Downloads |
| `4` | Uploads |
| `5` | Browse |
| `6` | Chat |
| `7` | Users |
| `8` | Shares |
| `9` | Settings |
| `0` | Library |
| `a` | Quality |

On the quality view, `r` forgets saved verdicts and measures the library again.

```text
soul-sever [--config PATH] [--bindip ADDR] [--port PORT] [--rescan] [--headless]
```

`--config` selects the account file. `--bindip` and `--port` override the listen socket for that run. `--rescan` prints the share count and exits. `--headless` logs the session to stderr without the terminal. `--rescan` wins when both are set. `soul-sever --help` and `soul-sever --version` print and exit.

## Configuration

The config file is `$XDG_CONFIG_HOME/soul-sever/config.toml`, or `~/.config/soul-sever/config.toml`. The settings and shares screens edit it. The password stays in the keyring. `library.toml` and `queue.toml` sit beside that file.

```toml
server_host = "server.slsknet.org"
server_port = 2242
listen_port = 2234

download_dir = "/home/alice/music"
incomplete_dir = "/home/alice/incomplete"

[shares]
public = ["/home/alice/music"]
```

The default server is `server.slsknet.org` on port `2242`. The default listen port is `2234`.

## Development

```shell
make test
make clippy
make run
```

`make test` is `cargo test --workspace --all-targets`. `make clippy` is `cargo clippy --workspace --all-targets -- -D warnings`.
