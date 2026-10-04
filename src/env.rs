//! Everything a command needs from the outside world, so tests can substitute each piece.
use crate::agent::{Scheduler, SystemScheduler};
use crate::api::{self, Client};
use crate::credentials::{CredentialStore, KeyringStore};
use crate::dirs::AppDirs;
use crate::error::Result;
use std::path::PathBuf;
use std::sync::Arc;
use url::Url;

pub struct Env {
    pub api_url: Url,
    pub client: Client,
    pub dirs: AppDirs,
    pub creds: Arc<dyn CredentialStore>,
    pub scheduler: Arc<dyn Scheduler>,
    pub open_browser: bool,
}

impl Env {
    pub fn production(api_url: Option<&str>, open_browser: bool) -> Result<Env> {
        let api_url = api::parse_api_url(api_url.unwrap_or(api::DEFAULT_API_URL))?;
        Ok(Env {
            client: Client::new(api_url.clone())?,
            dirs: AppDirs::discover()?,
            creds: Arc::new(KeyringStore),
            scheduler: Arc::new(SystemScheduler::new(&api_url)),
            api_url,
            open_browser,
        })
    }

    /// Credential-store account: the API origin, so each environment has its own session.
    pub fn account(&self) -> String {
        self.api_url.as_str().trim_end_matches('/').to_string()
    }

    pub fn state_dir(&self) -> PathBuf {
        self.dirs.origin_dir(&self.api_url)
    }

    pub fn snapshot_path(&self) -> PathBuf {
        self.state_dir().join("snapshot.json")
    }

    /// Serialises credential rotation across CLI processes and the background agent.
    pub fn auth_lock_path(&self) -> PathBuf {
        self.state_dir().join("auth.lock")
    }
}
