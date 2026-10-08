//! Account file for Soul Sever.
//!
//! The path is `$XDG_CONFIG_HOME/soul-sever/config.toml`, or
//! `~/.config/soul-sever/config.toml` when that variable is unset.
//! A missing file is [`Config::default`]. A present file that does not parse,
//! or that fails validation, is an error.

use std::collections::HashSet;
use std::fmt;
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::net::IpAddr;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

const DEFAULT_SERVER_HOST: &str = "server.slsknet.org";
const DEFAULT_SERVER_PORT: u16 = 2242;
const DEFAULT_LISTEN_PORT: u16 = 2234;
const DEFAULT_UPLOAD_SLOTS: u16 = 2;

fn default_server_host() -> String {
    DEFAULT_SERVER_HOST.to_owned()
}

fn default_server_port() -> u16 {
    DEFAULT_SERVER_PORT
}

fn default_listen_port() -> u16 {
    DEFAULT_LISTEN_PORT
}

fn default_upload_slots() -> u16 {
    DEFAULT_UPLOAD_SLOTS
}

fn default_max_results() -> u32 {
    300
}

fn default_min_search_chars() -> u32 {
    3
}

fn default_queue_file_limit() -> u32 {
    100
}

fn default_queue_megabytes() -> u32 {
    10_000
}

/// How queued uploads are chosen when a slot frees.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "kebab-case")]
pub enum QueueMode {
    #[default]
    Fifo,
    RoundRobin,
}

/// Password kept in memory for the session. [`Debug`] prints a redacted placeholder.
/// The account file does not store it. [`Config::save`] writes it to the system keyring.
#[derive(Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Password(String);

impl Password {
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }

    pub fn expose(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for Password {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("Password(********)")
    }
}

/// Folders shared at each Soulseek access level.
///
/// `exclude` entries are file-name globs. `*` is the wildcard. A match is
/// dropped before it enters the share index.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct Shares {
    #[serde(default)]
    pub public: Vec<PathBuf>,
    #[serde(default)]
    pub buddy: Vec<PathBuf>,
    #[serde(default)]
    pub trusted: Vec<PathBuf>,
    #[serde(default)]
    pub exclude: Vec<String>,
}

/// A censor or word-replace pair. `from` is the token; `to` is what replaces it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WordPair {
    pub from: String,
    pub to: String,
}

/// One buddy. Flags match the users view (`notify`, `prioritized`, `trusted`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Buddy {
    pub name: String,
    #[serde(default)]
    pub note: String,
    #[serde(default)]
    pub notify: bool,
    #[serde(default)]
    pub prioritized: bool,
    #[serde(default)]
    pub trusted: bool,
}

/// Saved client settings. The password is redacted in [`Debug`] and stored in the system keyring.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Config {
    #[serde(default)]
    pub username: String,
    /// Read from an older account file so it can be moved into the keyring.
    /// Never written back.
    #[serde(default, skip_serializing)]
    pub(crate) password: Password,
    /// Set by [`Config::set_password`]. The next [`Config::save`] updates the keyring.
    #[serde(skip)]
    pub(crate) password_dirty: bool,
    #[serde(default = "default_server_host")]
    pub server_host: String,
    #[serde(default = "default_server_port")]
    pub server_port: u16,
    #[serde(default = "default_listen_port")]
    pub listen_port: u16,
    /// Empty means every interface.
    #[serde(default)]
    pub bind_address: String,
    #[serde(default)]
    pub shares: Shares,
    #[serde(default)]
    pub buddies: Vec<Buddy>,
    #[serde(default)]
    pub ignored: Vec<String>,
    #[serde(default)]
    pub banned: Vec<String>,
    #[serde(default)]
    pub auto_join: Vec<String>,
    #[serde(default)]
    pub wishlist: Vec<String>,
    #[serde(default = "default_max_results")]
    pub max_results: u32,
    #[serde(default = "default_min_search_chars")]
    pub min_search_chars: u32,
    #[serde(default = "default_upload_slots")]
    pub upload_slots: u16,
    #[serde(default)]
    pub queue_mode: QueueMode,
    /// Kibibytes per second. Absent means unlimited.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub upload_limit_kib: Option<u32>,
    /// Kibibytes per second. Absent means unlimited.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub download_limit_kib: Option<u32>,
    /// Where a download grows until its byte count matches the offered size.
    #[serde(default)]
    pub incomplete_dir: String,
    /// Where a finished download is moved.
    #[serde(default)]
    pub download_dir: String,
    /// Queued plus active uploads at or above this count are rejected.
    #[serde(default = "default_queue_file_limit")]
    pub queue_file_limit: u32,
    /// Queued plus active upload bytes at or above this many mebibytes are rejected.
    #[serde(default = "default_queue_megabytes")]
    pub queue_megabytes: u32,
    /// Minutes without a key or click before `SetStatus` away. Zero stays online.
    #[serde(default)]
    pub auto_away_minutes: u32,
    /// Sent as a private reply only while the session is away. Empty sends nothing.
    #[serde(default)]
    pub auto_reply: String,
    #[serde(default)]
    pub log_rooms: bool,
    #[serde(default)]
    pub log_private: bool,
    #[serde(default)]
    pub room_log_dir: String,
    #[serde(default)]
    pub private_log_dir: String,
    #[serde(default)]
    pub censor: Vec<WordPair>,
    #[serde(default)]
    pub replace_words: Vec<WordPair>,
    /// When true, map `listen_port` with NAT-PMP and then UPnP.
    #[serde(default)]
    pub upnp: bool,
    /// Empty discovers the default gateway. `host:port` is a NAT-PMP endpoint and skips UPnP.
    #[serde(default)]
    pub upnp_gateway: String,
    /// Transfer list beside the account file. Not written into the account file.
    #[serde(skip)]
    pub queue_file: Option<PathBuf>,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            username: String::new(),
            password: Password::default(),
            password_dirty: false,
            server_host: default_server_host(),
            server_port: DEFAULT_SERVER_PORT,
            listen_port: DEFAULT_LISTEN_PORT,
            bind_address: String::new(),
            shares: Shares::default(),
            buddies: Vec::new(),
            ignored: Vec::new(),
            banned: Vec::new(),
            auto_join: Vec::new(),
            wishlist: Vec::new(),
            max_results: default_max_results(),
            min_search_chars: default_min_search_chars(),
            upload_slots: DEFAULT_UPLOAD_SLOTS,
            queue_mode: QueueMode::Fifo,
            upload_limit_kib: None,
            download_limit_kib: None,
            incomplete_dir: String::new(),
            download_dir: String::new(),
            queue_file_limit: default_queue_file_limit(),
            queue_megabytes: default_queue_megabytes(),
            auto_away_minutes: 0,
            auto_reply: String::new(),
            log_rooms: false,
            log_private: false,
            room_log_dir: String::new(),
            private_log_dir: String::new(),
            censor: Vec::new(),
            replace_words: Vec::new(),
            upnp: false,
            upnp_gateway: String::new(),
            queue_file: None,
        }
    }
}

/// `queue.toml` in the same directory as the account file.
pub fn queue_path(config_path: &Path) -> PathBuf {
    match config_path.parent() {
        Some(parent) if !parent.as_os_str().is_empty() => parent.join("queue.toml"),
        _ => PathBuf::from("queue.toml"),
    }
}

impl fmt::Debug for Config {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("Config")
            .field("username", &self.username)
            .field("password", &self.password)
            .field("server_host", &self.server_host)
            .field("server_port", &self.server_port)
            .field("listen_port", &self.listen_port)
            .field("bind_address", &self.bind_address)
            .field("shares", &self.shares)
            .field("buddies", &self.buddies)
            .field("ignored", &self.ignored)
            .field("banned", &self.banned)
            .field("auto_join", &self.auto_join)
            .field("wishlist", &self.wishlist)
            .field("max_results", &self.max_results)
            .field("min_search_chars", &self.min_search_chars)
            .field("upload_slots", &self.upload_slots)
            .field("queue_mode", &self.queue_mode)
            .field("upload_limit_kib", &self.upload_limit_kib)
            .field("download_limit_kib", &self.download_limit_kib)
            .field("incomplete_dir", &self.incomplete_dir)
            .field("download_dir", &self.download_dir)
            .field("queue_file_limit", &self.queue_file_limit)
            .field("queue_megabytes", &self.queue_megabytes)
            .field("auto_away_minutes", &self.auto_away_minutes)
            .field("auto_reply", &self.auto_reply)
            .field("log_rooms", &self.log_rooms)
            .field("log_private", &self.log_private)
            .field("room_log_dir", &self.room_log_dir)
            .field("private_log_dir", &self.private_log_dir)
            .field("censor", &self.censor)
            .field("replace_words", &self.replace_words)
            .field("upnp", &self.upnp)
            .field("upnp_gateway", &self.upnp_gateway)
            .finish()
    }
}

impl Config {
    pub fn password(&self) -> &str {
        self.password.expose()
    }

    pub fn set_password(&mut self, password: impl Into<String>) {
        self.password = Password::new(password);
        self.password_dirty = true;
    }

    /// `$XDG_CONFIG_HOME/soul-sever/config.toml`, else `$HOME/.config/soul-sever/config.toml`.
    pub fn default_path() -> PathBuf {
        let xdg = std::env::var_os("XDG_CONFIG_HOME").filter(|value| !value.is_empty());
        let home = std::env::var_os("HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from("."));
        path_for(xdg.as_deref().map(Path::new), &home)
    }

    /// A missing file yields [`Config::default`]. A file that fails to parse or validate does not.
    pub fn load(path: &Path) -> Result<Self, ConfigError> {
        if !path.exists() {
            return Ok(Self::default());
        }
        let text = fs::read_to_string(path).map_err(|source| ConfigError::Read {
            path: path.to_owned(),
            source,
        })?;
        let mut config: Self = toml::from_str(&text).map_err(|source| ConfigError::Parse {
            path: path.to_owned(),
            source,
        })?;
        config.validate()?;
        if !config.password().is_empty() {
            config.password_dirty = true;
            config.save(path)?;
            return Ok(config);
        }
        if !config.username.is_empty()
            && let Some(password) = crate::secrets::load(&config.username)?
        {
            config.password = Password::new(password);
        }
        Ok(config)
    }

    /// Creates parent directories and writes the file. On Unix the mode is `0600`.
    /// A changed password is written to the system keyring and left out of the file.
    pub fn save(&mut self, path: &Path) -> Result<(), ConfigError> {
        self.validate()?;
        self.persist_password(path)?;
        if let Some(parent) = path
            .parent()
            .filter(|parent| !parent.as_os_str().is_empty())
        {
            fs::create_dir_all(parent).map_err(|source| ConfigError::Write {
                path: parent.to_owned(),
                source,
            })?;
        }
        let text = toml::to_string_pretty(self)?;
        let mut options = OpenOptions::new();
        options.write(true).create(true).truncate(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let mut file = options.open(path).map_err(|source| ConfigError::Write {
            path: path.to_owned(),
            source,
        })?;
        file.write_all(text.as_bytes())
            .map_err(|source| ConfigError::Write {
                path: path.to_owned(),
                source,
            })?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(path, fs::Permissions::from_mode(0o600)).map_err(|source| {
                ConfigError::Write {
                    path: path.to_owned(),
                    source,
                }
            })?;
        }
        Ok(())
    }

    /// Writes a changed password to the keyring, and moves it when the username changes.
    fn persist_password(&mut self, path: &Path) -> Result<(), ConfigError> {
        let previous = username_in_file(path);
        let renamed = previous
            .as_deref()
            .is_some_and(|name| name != self.username);
        if self.username.is_empty() {
            if self.password_dirty && !self.password().is_empty() {
                return Err(ConfigError::Keyring(
                    "set a username before storing a password in the system keyring".to_owned(),
                ));
            }
            if let Some(previous) = previous.filter(|name| !name.is_empty()) {
                crate::secrets::delete(&previous)?;
            }
            self.password_dirty = false;
            return Ok(());
        }
        if self.password_dirty || renamed {
            if self.password().is_empty() {
                crate::secrets::delete(&self.username)?;
            } else {
                crate::secrets::store(&self.username, self.password())?;
            }
        }
        if let Some(previous) = previous.filter(|name| name != &self.username) {
            crate::secrets::delete(&previous)?;
        }
        self.password_dirty = false;
        Ok(())
    }

    fn validate(&self) -> Result<(), ConfigError> {
        if self.server_host.trim().is_empty() {
            return Err(ConfigError::Invalid(
                "server_host must not be empty".to_owned(),
            ));
        }
        if self.server_port == 0 {
            return Err(ConfigError::Invalid(
                "server_port must be between 1 and 65535".to_owned(),
            ));
        }
        if self.listen_port == 0 {
            return Err(ConfigError::Invalid(
                "listen_port must be between 1 and 65535".to_owned(),
            ));
        }
        if !self.bind_address.is_empty() && self.bind_address.parse::<IpAddr>().is_err() {
            return Err(ConfigError::Invalid(format!(
                "bind_address {} is not an IP address",
                self.bind_address
            )));
        }
        reject_zero_limit("upload_limit_kib", self.upload_limit_kib)?;
        reject_zero_limit("download_limit_kib", self.download_limit_kib)?;
        let mut buddies = HashSet::new();
        for buddy in &self.buddies {
            if buddy.name.trim().is_empty() {
                return Err(ConfigError::Invalid(
                    "buddy names must not be empty".to_owned(),
                ));
            }
            if !buddies.insert(buddy.name.as_str()) {
                return Err(ConfigError::Invalid(format!(
                    "duplicate buddy {}",
                    buddy.name
                )));
            }
        }
        require_names("ignored", &self.ignored)?;
        require_names("banned", &self.banned)?;
        require_names("auto_join", &self.auto_join)?;
        require_names("wishlist", &self.wishlist)?;
        require_pairs("censor", &self.censor)?;
        require_pairs("replace_words", &self.replace_words)?;
        require_names("shares.exclude", &self.shares.exclude)?;
        if self.max_results == 0 {
            return Err(ConfigError::Invalid(
                "max_results must be at least 1".to_owned(),
            ));
        }
        if self.min_search_chars == 0 {
            return Err(ConfigError::Invalid(
                "min_search_chars must be at least 1".to_owned(),
            ));
        }
        if self.queue_file_limit == 0 {
            return Err(ConfigError::Invalid(
                "queue_file_limit must be at least 1".to_owned(),
            ));
        }
        if !self.upnp_gateway.is_empty()
            && self.upnp_gateway.parse::<std::net::SocketAddr>().is_err()
        {
            return Err(ConfigError::Invalid(format!(
                "upnp_gateway {} is not host:port",
                self.upnp_gateway
            )));
        }
        if self.queue_megabytes == 0 {
            return Err(ConfigError::Invalid(
                "queue_megabytes must be at least 1".to_owned(),
            ));
        }
        Ok(())
    }
}

fn reject_zero_limit(name: &str, limit: Option<u32>) -> Result<(), ConfigError> {
    if limit == Some(0) {
        return Err(ConfigError::Invalid(format!(
            "{name} must be omitted for unlimited, or at least 1"
        )));
    }
    Ok(())
}

fn require_pairs(label: &str, pairs: &[WordPair]) -> Result<(), ConfigError> {
    if pairs.iter().any(|pair| pair.from.trim().is_empty()) {
        return Err(ConfigError::Invalid(format!(
            "{label} entries must not be empty"
        )));
    }
    Ok(())
}

fn require_names(label: &str, names: &[String]) -> Result<(), ConfigError> {
    if names.iter().any(|name| name.trim().is_empty()) {
        return Err(ConfigError::Invalid(format!(
            "{label} entries must not be empty"
        )));
    }
    Ok(())
}

fn username_in_file(path: &Path) -> Option<String> {
    let text = fs::read_to_string(path).ok()?;
    let config: Config = toml::from_str(&text).ok()?;
    if config.username.is_empty() {
        None
    } else {
        Some(config.username)
    }
}

/// `xdg/soul-sever/config.toml` when `xdg` is set, otherwise `home/.config/soul-sever/config.toml`.
pub fn path_for(xdg_config_home: Option<&Path>, home: &Path) -> PathBuf {
    match xdg_config_home {
        Some(dir) if !dir.as_os_str().is_empty() => dir.join("soul-sever").join("config.toml"),
        _ => home.join(".config").join("soul-sever").join("config.toml"),
    }
}

#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    #[error("failed to read {path}: {source}")]
    Read {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("failed to write {path}: {source}")]
    Write {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("failed to parse {path}: {source}")]
    Parse {
        path: PathBuf,
        source: toml::de::Error,
    },
    #[error("failed to serialize config: {0}")]
    Serialize(#[from] toml::ser::Error),
    #[error("{0}")]
    Invalid(String),
    #[error("{0}")]
    Keyring(String),
}

impl From<crate::secrets::SecretError> for ConfigError {
    fn from(err: crate::secrets::SecretError) -> Self {
        Self::Keyring(err.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("soul-sever-{}-{name}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir.join("config.toml")
    }

    fn sample() -> Config {
        let mut config = Config {
            username: "alice".to_owned(),
            bind_address: "127.0.0.1".to_owned(),
            shares: Shares {
                public: vec![PathBuf::from("/music/public")],
                buddy: vec![PathBuf::from("/music/buddy")],
                trusted: vec![PathBuf::from("/music/trusted")],
                exclude: vec!["*.tmp".to_owned()],
            },
            buddies: vec![Buddy {
                name: "bob".to_owned(),
                note: "desk".to_owned(),
                notify: true,
                prioritized: true,
                trusted: true,
            }],
            ignored: vec!["spam".to_owned()],
            banned: vec!["leech".to_owned()],
            auto_join: vec!["jazz".to_owned()],
            wishlist: vec!["piano".to_owned()],
            upload_slots: 4,
            queue_mode: QueueMode::RoundRobin,
            upload_limit_kib: Some(64),
            download_limit_kib: Some(128),
            ..Config::default()
        };
        config.set_password("secret-value");
        config
    }

    #[test]
    fn missing_file_returns_the_default() {
        let path = scratch("missing");
        let _ = fs::remove_file(&path);
        let config = Config::load(&path).unwrap();
        assert_eq!(config, Config::default());
        assert!(config.username.is_empty());
        assert!(config.password().is_empty());
        assert_eq!(config.server_host, "server.slsknet.org");
        assert_eq!(config.server_port, 2242);
        assert_eq!(config.listen_port, 2234);
        assert_eq!(config.upload_slots, 2);
        assert_eq!(config.queue_mode, QueueMode::Fifo);
    }

    #[test]
    fn save_load_reload_round_trips() {
        let path = scratch("round").join("nested").join("config.toml");
        let mut config = sample();
        config.username = "round-trip".to_owned();
        config.save(&path).unwrap();
        let text = fs::read_to_string(&path).unwrap();
        assert!(!text.contains("secret-value"), "{text}");
        assert!(!text.contains("password"), "{text}");
        let mut loaded = Config::load(&path).unwrap();
        assert_eq!(loaded, config);
        assert_eq!(loaded.password(), "secret-value");
        loaded.save(&path).unwrap();
        assert_eq!(Config::load(&path).unwrap(), config);
    }

    #[test]
    fn omitted_keys_keep_defaults() {
        let path = scratch("partial");
        fs::write(&path, "username = \"alice\"\n").unwrap();
        let config = Config::load(&path).unwrap();
        assert_eq!(config.username, "alice");
        assert_eq!(config.server_host, "server.slsknet.org");
        assert_eq!(config.server_port, 2242);
        assert_eq!(config.listen_port, 2234);
        assert_eq!(config.upload_slots, 2);
    }

    #[cfg(unix)]
    #[test]
    fn save_sets_mode_0600() {
        use std::os::unix::fs::PermissionsExt;
        let path = scratch("mode");
        let mut config = sample();
        config.username = "mode-user".to_owned();
        config.save(&path).unwrap();
        let mode = fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600);
    }

    #[test]
    fn debug_omits_the_password() {
        let config = sample();
        let text = format!("{config:?}");
        assert!(!text.contains("secret-value"));
        assert!(text.contains("Password(********)"));
        assert!(text.contains("alice"));
    }

    #[test]
    fn invalid_toml_is_an_error() {
        let path = scratch("bad");
        fs::write(&path, "username = [\n").unwrap();
        let err = Config::load(&path).unwrap_err();
        assert!(matches!(err, ConfigError::Parse { .. }));
    }

    #[test]
    fn listen_port_zero_is_rejected() {
        let path = scratch("port");
        fs::write(&path, "listen_port = 0\npassword = \"secret-value\"\n").unwrap();
        let err = Config::load(&path).unwrap_err();
        let text = err.to_string();
        assert!(text.contains("listen_port"));
        assert!(!text.contains("secret-value"));
    }

    #[test]
    fn path_for_prefers_xdg_config_home() {
        let xdg = Path::new("/tmp/xdg");
        let home = Path::new("/home/alice");
        assert_eq!(
            path_for(Some(xdg), home),
            PathBuf::from("/tmp/xdg/soul-sever/config.toml")
        );
        assert_eq!(
            path_for(None, home),
            PathBuf::from("/home/alice/.config/soul-sever/config.toml")
        );
    }

    #[test]
    fn a_plaintext_password_is_moved_into_the_keyring() {
        let path = scratch("legacy");
        fs::write(
            &path,
            "username = \"legacy-user\"\npassword = \"secret-value\"\n",
        )
        .unwrap();
        let loaded = Config::load(&path).unwrap();
        assert_eq!(loaded.password(), "secret-value");
        let text = fs::read_to_string(&path).unwrap();
        assert!(!text.contains("secret-value"), "{text}");
        assert!(!text.contains("password"), "{text}");
        assert_eq!(Config::load(&path).unwrap().password(), "secret-value");
    }

    #[test]
    fn clearing_the_password_removes_it_from_the_keyring() {
        let path = scratch("clear");
        let mut config = sample();
        config.username = "clear-user".to_owned();
        config.save(&path).unwrap();
        config.set_password("");
        config.save(&path).unwrap();
        assert!(Config::load(&path).unwrap().password().is_empty());
    }

    #[test]
    fn renaming_the_account_moves_the_keyring_entry() {
        let path = scratch("rename");
        let mut config = sample();
        config.username = "rename-old".to_owned();
        config.save(&path).unwrap();
        config.username = "rename-new".to_owned();
        config.save(&path).unwrap();
        assert!(crate::secrets::load("rename-old").unwrap().is_none());
        let loaded = Config::load(&path).unwrap();
        assert_eq!(loaded.username, "rename-new");
        assert_eq!(loaded.password(), "secret-value");
    }

    #[test]
    fn saving_other_settings_leaves_the_stored_password() {
        let path = scratch("untouched");
        let mut config = sample();
        config.username = "untouched-user".to_owned();
        config.save(&path).unwrap();
        config.listen_port = 2235;
        config.save(&path).unwrap();
        let loaded = Config::load(&path).unwrap();
        assert_eq!(loaded.listen_port, 2235);
        assert_eq!(loaded.password(), "secret-value");
        let text = fs::read_to_string(&path).unwrap();
        assert!(!text.contains("secret-value"), "{text}");
    }

    #[test]
    fn a_password_without_a_username_is_not_stored() {
        let path = scratch("nameless");
        let mut config = Config::default();
        config.set_password("secret-value");
        let err = config.save(&path).unwrap_err();
        assert!(!err.to_string().contains("secret-value"));
        assert!(!path.exists());
    }
}
