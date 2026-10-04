//! Sync commands that talk to the API and keep the workspace consistent.
pub mod plan;
pub mod pull;
pub mod push;
pub mod status;

use crate::api::SyncManifest;
use crate::error::{Error, Result};
use crate::paths::is_canonical_markdown;
use crate::workspace::{LocalScan, Workspace};
use std::collections::BTreeMap;

/// Pulls retry from a fresh manifest at most this many times when the head moves underneath.
pub const PULL_ATTEMPTS: u32 = 3;

/// The remote manifest as `path → blob SHA`; every path is validated before it can touch disk.
pub fn remote_shas(manifest: &SyncManifest) -> Result<BTreeMap<String, String>> {
    let mut shas = BTreeMap::new();
    for file in &manifest.files {
        if !is_canonical_markdown(&file.path) {
            return Err(Error::Other(format!(
                "the API returned a non-canonical path {:?}; refusing to sync",
                file.path
            )));
        }
        shas.insert(file.path.clone(), file.sha.clone());
    }
    Ok(shas)
}

/// Symlinks inside the working copy stop any command that would read or write through them.
pub fn require_safe(scan: &LocalScan) -> Result<()> {
    if scan.unsafe_paths.is_empty() {
        Ok(())
    } else {
        Err(Error::invalid(format!(
            "the working copy contains symbolic links or special files, which Specio never follows: {}",
            scan.unsafe_paths.join(", ")
        )))
    }
}

/// A short, stable summary of the lock for human output.
pub fn lock_summary(ws: &Workspace) -> Result<String> {
    let lock = ws.load_lock()?;
    Ok(match lock.head_sha {
        Some(sha) => sha.chars().take(12).collect(),
        None => "none (not pulled yet)".to_string(),
    })
}

/// After a `403` the capability snapshot may be stale: refresh it once, then surface the denial.
/// The failed request is never retried here.
pub async fn settle<T>(
    session: &crate::session::Session<'_>,
    auth: &crate::session::Authed,
    result: Result<T>,
) -> Result<T> {
    if let Err(e) = &result
        && e.api_status() == Some(403)
    {
        session.after_forbidden(auth).await;
    }
    result
}
