//! The Specio access token lives only in the secure OS credential store.
use crate::error::{Error, Result};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fmt;
use std::sync::Mutex;
use time::OffsetDateTime;

const SERVICE: &str = "specio-cli";

/// What is stored per API origin. Never contains Google/GitHub credentials.
#[derive(Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct StoredCredential {
    /// Known once the first capability snapshot has been read.
    pub user_id: Option<String>,
    pub access_token: String,
    /// Signed into the token by the API. There is no refresh: this is the end of the session.
    #[serde(with = "time::serde::rfc3339")]
    pub expires_at: OffsetDateTime,
}

impl StoredCredential {
    pub fn is_expired(&self, now: OffsetDateTime) -> bool {
        self.expires_at <= now
    }
}

impl fmt::Debug for StoredCredential {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("StoredCredential")
            .field("user_id", &self.user_id)
            .field("access_token", &"[redacted]")
            .field("expires_at", &self.expires_at)
            .finish()
    }
}

pub trait CredentialStore: Send + Sync {
    fn load(&self, account: &str) -> Result<Option<StoredCredential>>;
    fn save(&self, account: &str, credential: &StoredCredential) -> Result<()>;
    fn delete(&self, account: &str) -> Result<()>;
}

/// macOS Keychain, Windows Credential Manager, or the Secret Service on Linux.
pub struct KeyringStore;

fn store_error(e: keyring::Error) -> Error {
    let hint = if cfg!(all(unix, not(target_os = "macos"))) {
        " (a Secret Service provider such as GNOME Keyring or KWallet must be running)"
    } else {
        ""
    };
    Error::CredentialStore(format!("{e}{hint}"))
}

impl KeyringStore {
    fn entry(account: &str) -> Result<keyring::Entry> {
        keyring::Entry::new(SERVICE, account).map_err(store_error)
    }
}

impl CredentialStore for KeyringStore {
    fn load(&self, account: &str) -> Result<Option<StoredCredential>> {
        match Self::entry(account)?.get_password() {
            Ok(raw) => serde_json::from_str(&raw).map(Some).map_err(|_| {
                Error::CredentialStore(
                    "the stored credential is unreadable; run `specio login` again".into(),
                )
            }),
            Err(keyring::Error::NoEntry) => Ok(None),
            Err(e) => Err(store_error(e)),
        }
    }

    fn save(&self, account: &str, credential: &StoredCredential) -> Result<()> {
        let raw = serde_json::to_string(credential).map_err(|e| Error::Other(e.to_string()))?;
        Self::entry(account)?
            .set_password(&raw)
            .map_err(store_error)
    }

    fn delete(&self, account: &str) -> Result<()> {
        match Self::entry(account)?.delete_credential() {
            Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
            Err(e) => Err(store_error(e)),
        }
    }
}

/// In-process store for tests; never used by the binary.
#[derive(Default)]
pub struct MemoryStore {
    entries: Mutex<HashMap<String, StoredCredential>>,
}

impl CredentialStore for MemoryStore {
    fn load(&self, account: &str) -> Result<Option<StoredCredential>> {
        Ok(self
            .entries
            .lock()
            .expect("store lock")
            .get(account)
            .cloned())
    }

    fn save(&self, account: &str, credential: &StoredCredential) -> Result<()> {
        self.entries
            .lock()
            .expect("store lock")
            .insert(account.to_string(), credential.clone());
        Ok(())
    }

    fn delete(&self, account: &str) -> Result<()> {
        self.entries.lock().expect("store lock").remove(account);
        Ok(())
    }
}
