//! Command-line flags. `--headless` logs session events and does not draw.

use std::io::{self, Write};
use std::net::IpAddr;
use std::path::PathBuf;
use std::process::ExitCode;

use crate::config::{Config, ConfigError, queue_path};
use crate::session::{Session, SessionEvent};

/// Parsed arguments, before the config file is read.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Mode {
    Help,
    Version,
    Run(Invocation),
}

/// Flags that select a config file and a run mode.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Invocation {
    pub config_path: Option<PathBuf>,
    pub bindip: Option<String>,
    pub port: Option<u16>,
    pub rescan: bool,
    pub headless: bool,
}

/// What `main` should start after a successful load.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Launch {
    Rescan(Config),
    Headless(Config),
    Terminal { path: PathBuf, config: Config },
}

/// Reads flags. `--help` and `--version` win over the run flags.
pub fn parse<I, S>(args: I) -> Result<Mode, String>
where
    I: IntoIterator<Item = S>,
    S: AsRef<str>,
{
    let mut invocation = Invocation {
        config_path: None,
        bindip: None,
        port: None,
        rescan: false,
        headless: false,
    };
    let mut args = args.into_iter().peekable();
    while let Some(arg) = args.next() {
        match arg.as_ref() {
            "-h" | "--help" => return Ok(Mode::Help),
            "-V" | "--version" => return Ok(Mode::Version),
            "--config" => {
                invocation.config_path = Some(PathBuf::from(next_value("--config", &mut args)?));
            }
            "--bindip" => {
                let value = next_value("--bindip", &mut args)?;
                if value.parse::<IpAddr>().is_err() {
                    return Err(format!("--bindip {value} is not an IP address"));
                }
                invocation.bindip = Some(value);
            }
            "--port" => {
                let value = next_value("--port", &mut args)?;
                let port = value
                    .parse::<u16>()
                    .map_err(|_| format!("--port {value} is not a port"))?;
                if port == 0 {
                    return Err("--port must be between 1 and 65535".to_owned());
                }
                invocation.port = Some(port);
            }
            "--rescan" => invocation.rescan = true,
            "--headless" => invocation.headless = true,
            other => return Err(format!("unknown argument {other}")),
        }
    }
    Ok(Mode::Run(invocation))
}

fn next_value<I, S>(flag: &str, args: &mut std::iter::Peekable<I>) -> Result<String, String>
where
    I: Iterator<Item = S>,
    S: AsRef<str>,
{
    match args.next() {
        Some(value) if !value.as_ref().starts_with('-') => Ok(value.as_ref().to_owned()),
        _ => Err(format!("{flag} needs a value")),
    }
}

/// Loads the config and applies `--bindip` and `--port`. `--rescan` wins over `--headless`.
pub fn load(invocation: &Invocation) -> Result<Launch, ConfigError> {
    let path = invocation
        .config_path
        .clone()
        .unwrap_or_else(Config::default_path);
    let mut config = Config::load(&path)?;
    config.queue_file = Some(queue_path(&path));
    if let Some(bindip) = &invocation.bindip {
        config.bind_address.clone_from(bindip);
    }
    if let Some(port) = invocation.port {
        config.listen_port = port;
    }
    if invocation.rescan {
        Ok(Launch::Rescan(config))
    } else if invocation.headless {
        Ok(Launch::Headless(config))
    } else {
        Ok(Launch::Terminal { path, config })
    }
}

/// File count from a share scan.
pub fn share_count(config: &Config) -> Result<usize, String> {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|err| err.to_string())?;
    runtime
        .block_on(crate::shares::rescan(&config.shares))
        .map(|index| index.len())
        .map_err(|err| err.to_string())
}

/// Builds the share index. Exit 0 prints the file count. An unreadable folder exits 1.
pub fn rescan(config: &Config) -> ExitCode {
    match share_count(config) {
        Ok(count) => {
            println!("shares: {count}");
            ExitCode::SUCCESS
        }
        Err(err) => {
            eprintln!("soul-sever: {err}");
            ExitCode::from(1)
        }
    }
}

/// Runs the session and writes events to `out`. Exit 0 after a login that later disconnects.
pub fn headless(config: Config, out: &mut dyn Write) -> ExitCode {
    let runtime = match tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
    {
        Ok(runtime) => runtime,
        Err(err) => {
            let _ = writeln!(out, "soul-sever: {err}");
            return ExitCode::from(1);
        }
    };
    runtime.block_on(async {
        let (tx, mut events) = tokio::sync::mpsc::channel(32);
        let session = Session::spawn(config, tx);
        let mut logged_in = false;
        let code = loop {
            match events.recv().await {
                Some(SessionEvent::LoggedIn { banner, .. }) => {
                    logged_in = true;
                    let _ = writeln!(out, "logged in: {banner}");
                }
                Some(SessionEvent::LoginFailed { reason }) => {
                    let _ = writeln!(out, "login failed: {reason}");
                    break ExitCode::from(1);
                }
                Some(SessionEvent::TimedOut) => {
                    let _ = writeln!(out, "login timed out");
                    break ExitCode::from(1);
                }
                Some(SessionEvent::Kicked) => {
                    let _ = writeln!(out, "logged in elsewhere");
                    break ExitCode::from(1);
                }
                Some(SessionEvent::Disconnected { message }) => {
                    let _ = writeln!(out, "disconnected: {message}");
                    break if logged_in {
                        ExitCode::SUCCESS
                    } else {
                        ExitCode::from(1)
                    };
                }
                Some(other) => {
                    let _ = writeln!(out, "{other:?}");
                }
                None => {
                    break if logged_in {
                        ExitCode::SUCCESS
                    } else {
                        ExitCode::from(1)
                    };
                }
            }
        };
        drop(session);
        code
    })
}

/// Maps a config load failure to exit 1.
pub fn report_config(err: ConfigError) -> ExitCode {
    eprintln!("soul-sever: {err}");
    ExitCode::from(1)
}

/// Maps a usage error to exit 2.
pub fn report_usage(err: impl std::fmt::Display) -> ExitCode {
    eprintln!("soul-sever: {err}");
    eprintln!("try soul-sever --help");
    ExitCode::from(2)
}

pub fn write_help(out: &mut dyn Write) -> io::Result<()> {
    writeln!(
        out,
        "\
soul-sever {version}
terminal Soulseek client

usage: soul-sever [--config PATH] [--bindip ADDR] [--port PORT] [--rescan] [--headless]
       soul-sever --help
       soul-sever --version

--config PATH   account file (default $XDG_CONFIG_HOME/soul-sever/config.toml)
--bindip ADDR   listen address for this run
--port PORT     listen port for this run
--rescan        count the share folders and exit
--headless      log in and write events to stderr, without the terminal

An empty username asks for a Soulseek username and password and does not open a socket.
A username logs in to the server written in the config file.
The password is stored in the system keyring, not in the account file.
--rescan exits before --headless when both are set.
",
        version = env!("CARGO_PKG_VERSION")
    )
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::net::Ipv4Addr;

    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;

    use super::*;
    use crate::protocol::{encode_frame, server};

    #[test]
    fn each_flag_is_accepted() {
        let mode = parse([
            "--config",
            "account.toml",
            "--bindip",
            "127.0.0.1",
            "--port",
            "2234",
            "--rescan",
            "--headless",
        ])
        .unwrap();
        let Mode::Run(invocation) = mode else {
            panic!("expected a run");
        };
        assert_eq!(invocation.config_path, Some(PathBuf::from("account.toml")));
        assert_eq!(invocation.bindip.as_deref(), Some("127.0.0.1"));
        assert_eq!(invocation.port, Some(2234));
        assert!(invocation.rescan);
        assert!(invocation.headless);
        assert!(matches!(parse(["--help"]), Ok(Mode::Help)));
        assert!(matches!(parse(["-V"]), Ok(Mode::Version)));
        assert!(parse(["--nope"]).unwrap_err().contains("unknown argument"));
        assert!(parse(["--port", "0"]).is_err());
        assert!(parse(["--bindip", "not-an-ip"]).is_err());
        assert!(parse(["--config"]).is_err());
    }

    #[test]
    fn rescan_wins_over_headless() {
        let dir = std::env::temp_dir().join(format!(
            "soul-sever-cli-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("config.toml");
        fs::write(&path, "username = \"alice\"\n").unwrap();
        let mode = parse(["--config", path.to_str().unwrap(), "--headless", "--rescan"]).unwrap();
        let Mode::Run(invocation) = mode else {
            panic!("expected a run");
        };
        assert!(matches!(load(&invocation).unwrap(), Launch::Rescan(_)));
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn rescan_counts_the_temp_tree_and_rejects_a_missing_folder() {
        let root = std::env::temp_dir().join(format!(
            "soul-sever-rescan-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir_all(&root).unwrap();
        fs::write(root.join("note.txt"), b"hello").unwrap();
        let config = Config {
            shares: crate::config::Shares {
                public: vec![root.clone()],
                ..crate::config::Shares::default()
            },
            ..Config::default()
        };
        assert_eq!(share_count(&config).unwrap(), 1);
        assert_eq!(rescan(&config), ExitCode::SUCCESS);
        let missing = Config {
            shares: crate::config::Shares {
                public: vec![root.join("missing")],
                ..crate::config::Shares::default()
            },
            ..Config::default()
        };
        assert_eq!(rescan(&missing), ExitCode::from(1));
        let _ = fs::remove_dir_all(root);
    }

    #[tokio::test]
    async fn headless_prints_the_fixture_banner_and_exits_0() {
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).await.unwrap();
        let addr = listener.local_addr().unwrap();
        let listen = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).await.unwrap();
        let listen_port = listen.local_addr().unwrap().port();
        drop(listen);
        let reply = encode_frame(
            server::LOGIN,
            &hex(
                "010500000068656c6c6f0100007f20000000633465333133313332323263663035666364643166633036386166353537306501",
            ),
        )
        .unwrap();
        tokio::spawn(async move {
            let (mut sock, _) = listener.accept().await.unwrap();
            let mut buf = [0u8; 512];
            let _ = sock.read(&mut buf).await.unwrap();
            sock.write_all(&reply).await.unwrap();
        });
        let config = Config {
            username: "alice".to_owned(),
            password: crate::config::Password::new("secret"),
            server_host: addr.ip().to_string(),
            server_port: addr.port(),
            listen_port,
            bind_address: "127.0.0.1".to_owned(),
            ..Config::default()
        };
        let (code, output) = tokio::task::spawn_blocking(move || {
            let mut output = Vec::new();
            let code = headless(config, &mut output);
            (code, output)
        })
        .await
        .unwrap();
        assert_eq!(code, ExitCode::SUCCESS);
        let text = String::from_utf8(output).unwrap();
        assert!(text.contains("logged in: hello"), "{text}");
    }

    fn hex(text: &str) -> Vec<u8> {
        (0..text.len())
            .step_by(2)
            .map(|index| u8::from_str_radix(&text[index..index + 2], 16).unwrap())
            .collect()
    }
}
