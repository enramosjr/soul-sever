# Soul Sever

A terminal client for the [Soulseek](https://www.slsknet.org/) network, written in Rust.

Search, browse shares, chat, and transfer files from one screen. Finished downloads can be played in the client.

## Features

- Network, room, buddy, user, and wishlist search
- Download and upload queues
- Browse another user's files
- Chat rooms
- Shared folders
- Library and playback for finished downloads
- Mouse and keyboard control

## Building

[Rust](https://www.rust-lang.org/) 1.88 or newer.

```shell
cargo build --release
```

The binary is `target/release/soul-sever`. From a checkout, `cargo run` builds and starts it.

## Usage

```shell
soul-sever
```

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

`soul-sever --help` lists the command-line options.

## Configuration

The config file is `$XDG_CONFIG_HOME/soul-sever/config.toml`, or `~/.config/soul-sever/config.toml`. The settings and shares screens edit it. `--config` selects another path.

```toml
server_host = "server.slsknet.org"
server_port = 2242
listen_port = 2234

download_dir = "/home/alice/music"
incomplete_dir = "/home/alice/incomplete"

[shares]
public = ["/home/alice/music"]
```

The default server is `server.slsknet.org` on port `2242`.

## Development

```shell
make test
make clippy
```
