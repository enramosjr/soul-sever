//! Account password in the system keyring.
//!
//! Linux uses the Secret Service (GNOME Keyring, KWallet, or another provider).
//! macOS uses Keychain. Windows uses Credential Manager. The account file never
//! receives the password. Tests use an in-memory stand-in so they do not touch
//! the signed-in user's keyring.

use std::fmt;

#[derive(Debug)]
pub(crate) struct SecretError(String);

impl SecretError {
    #[cfg(not(test))]
    fn new(action: &str, err: impl fmt::Display) -> Self {
        Self(format!(
            "could not {action} the password in the system keyring: {err}"
        ))
    }
}

impl fmt::Display for SecretError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

pub(crate) fn load(username: &str) -> Result<Option<String>, SecretError> {
    backend::load(username)
}

pub(crate) fn store(username: &str, password: &str) -> Result<(), SecretError> {
    backend::store(username, password)
}

pub(crate) fn delete(username: &str) -> Result<(), SecretError> {
    backend::delete(username)
}

#[cfg(not(test))]
mod backend {
    use super::SecretError;

    const SERVICE: &str = "soul-sever";

    fn entry(username: &str) -> Result<keyring::v1::Entry, SecretError> {
        keyring::v1::Entry::new(SERVICE, username).map_err(|err| SecretError::new("open", err))
    }

    pub(super) fn load(username: &str) -> Result<Option<String>, SecretError> {
        match entry(username)?.get_password() {
            Ok(password) => Ok(Some(password)),
            Err(keyring::v1::Error::NoEntry) => Ok(None),
            Err(err) => Err(SecretError::new("read", err)),
        }
    }

    pub(super) fn store(username: &str, password: &str) -> Result<(), SecretError> {
        entry(username)?
            .set_password(password)
            .map_err(|err| SecretError::new("save", err))
    }

    pub(super) fn delete(username: &str) -> Result<(), SecretError> {
        match entry(username)?.delete_credential() {
            Ok(()) | Err(keyring::v1::Error::NoEntry) => Ok(()),
            Err(err) => Err(SecretError::new("remove", err)),
        }
    }
}

#[cfg(test)]
mod backend {
    use std::collections::HashMap;
    use std::sync::{LazyLock, Mutex};

    use super::SecretError;

    fn memory() -> std::sync::MutexGuard<'static, HashMap<String, String>> {
        static MEMORY: LazyLock<Mutex<HashMap<String, String>>> =
            LazyLock::new(|| Mutex::new(HashMap::new()));
        MEMORY.lock().unwrap_or_else(|err| err.into_inner())
    }

    pub(super) fn load(username: &str) -> Result<Option<String>, SecretError> {
        Ok(memory().get(username).cloned())
    }

    pub(super) fn store(username: &str, password: &str) -> Result<(), SecretError> {
        memory().insert(username.to_owned(), password.to_owned());
        Ok(())
    }

    pub(super) fn delete(username: &str) -> Result<(), SecretError> {
        memory().remove(username);
        Ok(())
    }
}
