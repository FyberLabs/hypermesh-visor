//! One keychain entry for the refresh token.
//!
//! Service `hypermesh`, account `session`. The Rust `keyring` crate writes that
//! entry to the macOS Keychain, Windows Credential Manager, or the Linux Secret
//! Service. The Go CLI uses the same service and account. Access tokens are
//! never stored here.

use std::fmt;
use std::sync::Mutex;

use crate::AuthError;

pub const SERVICE: &str = "hypermesh";
pub const ACCOUNT: &str = "session";

pub trait SessionStore {
    fn put_refresh_token(&self, token: &str) -> Result<(), AuthError>;
    fn refresh_token(&self) -> Result<Option<String>, AuthError>;
    fn delete(&self) -> Result<(), AuthError>;
}

/// In-memory stand-in used by tests. It never touches a file or the OS keychain.
#[derive(Default)]
pub struct MemoryStore {
    token: Mutex<Option<String>>,
}

impl MemoryStore {
    pub fn new() -> Self {
        Self::default()
    }
}

impl fmt::Debug for MemoryStore {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let present = self
            .token
            .lock()
            .map(|guard| guard.is_some())
            .unwrap_or(false);
        f.debug_struct("MemoryStore")
            .field("token", &if present { "present" } else { "absent" })
            .finish()
    }
}

impl SessionStore for MemoryStore {
    fn put_refresh_token(&self, token: &str) -> Result<(), AuthError> {
        if token.is_empty() {
            return Err(AuthError::NoRefreshToken);
        }
        *self
            .token
            .lock()
            .map_err(|_| AuthError::message("session store lock"))? = Some(token.to_string());
        Ok(())
    }

    fn refresh_token(&self) -> Result<Option<String>, AuthError> {
        Ok(self
            .token
            .lock()
            .map_err(|_| AuthError::message("session store lock"))?
            .clone())
    }

    fn delete(&self) -> Result<(), AuthError> {
        *self
            .token
            .lock()
            .map_err(|_| AuthError::message("session store lock"))? = None;
        Ok(())
    }
}

/// OS keychain. A missing Secret Service, Keychain, or Credential Manager is an
/// error. This type does not write a file.
pub struct KeyringStore;

impl SessionStore for KeyringStore {
    fn put_refresh_token(&self, token: &str) -> Result<(), AuthError> {
        if token.is_empty() {
            return Err(AuthError::NoRefreshToken);
        }
        let entry = keyring::Entry::new(SERVICE, ACCOUNT).map_err(map_keyring)?;
        entry.set_password(token).map_err(map_keyring)
    }

    fn refresh_token(&self) -> Result<Option<String>, AuthError> {
        let entry = keyring::Entry::new(SERVICE, ACCOUNT).map_err(map_keyring)?;
        match entry.get_password() {
            Ok(token) if !token.is_empty() => Ok(Some(token)),
            Ok(_) => Ok(None),
            Err(keyring::Error::NoEntry) => Ok(None),
            Err(err) => Err(map_keyring(err)),
        }
    }

    fn delete(&self) -> Result<(), AuthError> {
        let entry = keyring::Entry::new(SERVICE, ACCOUNT).map_err(map_keyring)?;
        match entry.delete_credential() {
            Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
            Err(err) => Err(map_keyring(err)),
        }
    }
}

fn map_keyring(err: keyring::Error) -> AuthError {
    match err {
        keyring::Error::NoEntry => AuthError::NoSession,
        keyring::Error::NoStorageAccess(_) | keyring::Error::PlatformFailure(_) => {
            AuthError::NoKeychain
        }
        other => AuthError::message(other.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn memory_store_round_trips_and_deletes() {
        let store = MemoryStore::new();
        assert_eq!(store.refresh_token().unwrap(), None);
        store.put_refresh_token("refresh-1").unwrap();
        assert_eq!(store.refresh_token().unwrap().as_deref(), Some("refresh-1"));
        store.put_refresh_token("refresh-2").unwrap();
        assert_eq!(store.refresh_token().unwrap().as_deref(), Some("refresh-2"));
        store.delete().unwrap();
        assert_eq!(store.refresh_token().unwrap(), None);
        let rendered = format!("{store:?}");
        assert!(!rendered.contains("refresh"));
    }

    #[test]
    fn empty_refresh_token_is_refused() {
        let store = MemoryStore::new();
        assert!(store.put_refresh_token("").is_err());
        assert_eq!(store.refresh_token().unwrap(), None);
    }
}
