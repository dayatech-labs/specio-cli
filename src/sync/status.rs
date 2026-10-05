//! `speq status` and `speq diff`.
use super::plan::{State, classify};
use super::{remote_shas, settle};
use crate::env::Env;
use crate::error::{Error, Result};
use crate::session::Session;
use crate::workspace::Workspace;
use serde::Serialize;
use similar::TextDiff;

#[derive(Debug, Serialize)]
pub struct PathStatus {
    pub path: String,
    pub state: &'static str,
    pub local: &'static str,
    pub remote: &'static str,
}

#[derive(Debug, Serialize)]
pub struct StatusReport {
    pub project_id: String,
    pub lock_head_sha: Option<String>,
    pub remote_head_sha: String,
    /// Lock is at the remote head and nothing is pending.
    pub up_to_date: bool,
    pub pending: Vec<String>,
    pub files: Vec<PathStatus>,
    pub skipped: Vec<String>,
    pub unsafe_paths: Vec<String>,
}

pub async fn status(env: &Env, ws: &Workspace) -> Result<StatusReport> {
    let _guard = ws.guard(env, true)?;
    let session = Session::new(env);
    let auth = session.authenticate().await?;
    let result = async {
        let remote = env.client.sync(&auth.token, ws.project_id()).await?;
        let remote_paths = remote_shas(&remote)?;
        let scan = ws.scan()?;
        let manifest = ws.load_manifest()?;
        let lock = ws.load_lock()?;

        let mut paths: Vec<&String> = remote_paths
            .keys()
            .chain(manifest.files.keys())
            .chain(scan.files.keys())
            .collect();
        paths.sort();
        paths.dedup();
        let files = paths
            .into_iter()
            .map(|path| {
                let v = classify(
                    remote_paths.get(path).map(String::as_str),
                    manifest.files.get(path).map(|e| e.sha.as_str()),
                    scan.files.get(path).map(String::as_str),
                );
                PathStatus {
                    path: path.clone(),
                    state: v.state.as_str(),
                    local: v.local.as_str(),
                    remote: v.remote.as_str(),
                }
            })
            .collect();
        Ok(StatusReport {
            project_id: ws.project_id().to_string(),
            up_to_date: lock.head_sha.as_deref() == Some(remote.head_sha.as_str())
                && lock.pending.is_empty(),
            lock_head_sha: lock.head_sha,
            remote_head_sha: remote.head_sha,
            pending: lock.pending,
            files,
            skipped: scan
                .skipped
                .iter()
                .map(|(p, why)| format!("{p} ({why})"))
                .collect(),
            unsafe_paths: scan.unsafe_paths,
        })
    }
    .await;
    settle(&session, &auth, result).await
}

impl StatusReport {
    pub fn count(&self, state: State) -> usize {
        self.files
            .iter()
            .filter(|f| f.state == state.as_str())
            .count()
    }
    pub fn has_conflicts(&self) -> bool {
        self.count(State::Conflict) > 0
    }
}

#[derive(Debug, Serialize)]
pub struct DiffReport {
    pub path: String,
    /// What the local copy was compared with: `remote` or `base`.
    pub against: &'static str,
    pub identical: bool,
    pub diff: String,
}

fn text(bytes: Option<Vec<u8>>, what: &str, path: &str) -> Result<String> {
    match bytes {
        None => Ok(String::new()),
        Some(b) => String::from_utf8(b)
            .map_err(|_| Error::invalid(format!("the {what} copy of {path} is not UTF-8 text"))),
    }
}

fn unified(path: &str, old_label: &str, old: &str, new: &str) -> String {
    TextDiff::from_lines(old, new)
        .unified_diff()
        .context_radius(3)
        .header(&format!("{old_label}/{path}"), &format!("local/{path}"))
        .to_string()
}

/// `--base`: local against the last synced baseline, entirely offline.
pub fn diff_base(ws: &Workspace, path: &str) -> Result<DiffReport> {
    let base = text(ws.read_base(path)?, "baseline", path)?;
    let local = text(ws.read_local(path)?, "local", path)?;
    Ok(DiffReport {
        path: path.into(),
        against: "base",
        identical: base == local,
        diff: unified(path, "base", &base, &local),
    })
}

/// Default: local against the latest remote version.
pub async fn diff_remote(env: &Env, ws: &Workspace, path: &str) -> Result<DiffReport> {
    ws.check_target(path)?;
    let session = Session::new(env);
    let auth = session.authenticate().await?;
    let result = async {
        let remote = match env
            .client
            .document(&auth.token, ws.project_id(), path, None)
            .await
        {
            Ok(doc) => doc.content,
            // A path the remote does not have is diffed against nothing.
            Err(Error::Api(e)) if e.status == 404 => String::new(),
            Err(e) => return Err(e),
        };
        let local = text(ws.read_local(path)?, "local", path)?;
        Ok(DiffReport {
            path: path.into(),
            against: "remote",
            identical: remote == local,
            diff: unified(path, "remote", &remote, &local),
        })
    }
    .await;
    settle(&session, &auth, result).await
}
