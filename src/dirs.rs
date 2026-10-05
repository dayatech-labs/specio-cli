//! Application-data directory for per-OS-user state: capability snapshot and locks.
//! It is separate from any workspace and from the credential store.
use crate::error::{Error, Result};
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};
use url::Url;

#[derive(Debug, Clone)]
pub struct AppDirs {
    root: PathBuf,
}

impl AppDirs {
    pub fn discover() -> Result<AppDirs> {
        let dirs = directories::ProjectDirs::from("com", "dayatech", "speq").ok_or_else(|| {
            Error::Other("cannot determine the application data directory".into())
        })?;
        Ok(AppDirs {
            root: dirs.data_local_dir().to_path_buf(),
        })
    }

    pub fn at(root: impl Into<PathBuf>) -> AppDirs {
        AppDirs { root: root.into() }
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    /// State separated per API origin so production and a local API never share a session.
    pub fn origin_dir(&self, api_url: &Url) -> PathBuf {
        let host = api_url.host_str().unwrap_or("unknown");
        let slug = match api_url.port() {
            Some(port) => format!("{host}_{port}"),
            None => host.to_string(),
        };
        let slug: String = slug
            .chars()
            .map(|c| {
                if c.is_ascii_alphanumeric() || c == '.' || c == '-' || c == '_' {
                    c
                } else {
                    '_'
                }
            })
            .collect();
        self.root.join("origins").join(slug)
    }

    /// Lock file coordinating CLI processes that operate on one workspace.
    pub fn workspace_lock(&self, workspace_root: &Path) -> PathBuf {
        let digest = Sha256::digest(workspace_root.to_string_lossy().as_bytes());
        let short: String = digest.iter().take(8).map(|b| format!("{b:02x}")).collect();
        self.root.join("workspaces").join(format!("{short}.lock"))
    }
}
