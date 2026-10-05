//! `speq pull` and `speq update`: apply the remote manifest without ever overwriting local work.
use super::plan::{Change, State, classify};
use super::{PULL_ATTEMPTS, remote_shas, require_safe, settle};
use crate::api::SyncManifest;
use crate::env::Env;
use crate::error::{Error, Result};
use crate::hashing::git_blob_sha;
use crate::session::{Authed, Session};
use crate::workspace::{BaseEntry, LockState, Manifest, Workspace};
use secrecy::{ExposeSecret, SecretString};
use serde::Serialize;
use std::collections::BTreeMap;
use tokio::task::JoinSet;

const DOWNLOAD_CONCURRENCY: usize = 8;

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct Conflict {
    pub path: String,
    pub reason: String,
}

#[derive(Debug, Serialize)]
pub struct PullReport {
    pub head_sha: String,
    pub added: Vec<String>,
    pub updated: Vec<String>,
    pub deleted: Vec<String>,
    /// Local content already equalled the remote: only the baseline was recorded.
    pub adopted: Vec<String>,
    pub conflicts: Vec<Conflict>,
    pub unchanged: usize,
    /// True only when every path was reconciled without conflict.
    pub lock_advanced: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Action {
    Add,
    Update,
    Delete,
    /// Local file equals the remote; record the baseline without touching the file.
    Adopt,
    /// Both sides removed the file; forget the baseline.
    Forget,
}

#[derive(Debug, Default)]
struct Plan {
    actions: Vec<(String, Action)>,
    conflicts: Vec<Conflict>,
    unchanged: usize,
}

fn conflict_reason(local: Change, remote: Change) -> &'static str {
    match (local, remote) {
        (Change::Added, Change::Added) => {
            "the remote added a file where an untracked local file exists"
        }
        (Change::Modified, Change::Deleted) => "deleted on the remote but changed locally",
        (Change::Deleted, Change::Modified) => "changed on the remote but deleted locally",
        _ => "changed both locally and on the remote",
    }
}

fn build_plan(
    remote: &BTreeMap<String, String>,
    manifest: &Manifest,
    local: &BTreeMap<String, String>,
) -> Plan {
    let mut paths: Vec<&String> = remote
        .keys()
        .chain(manifest.files.keys())
        .chain(local.keys())
        .collect();
    paths.sort();
    paths.dedup();
    let mut plan = Plan::default();
    for path in paths {
        let r = remote.get(path).map(String::as_str);
        let b = manifest.files.get(path).map(|e| e.sha.as_str());
        let l = local.get(path).map(String::as_str);
        let verdict = classify(r, b, l);
        if verdict.converged {
            plan.actions.push((
                path.clone(),
                if r.is_some() {
                    Action::Adopt
                } else {
                    Action::Forget
                },
            ));
            continue;
        }
        match verdict.state {
            State::RemoteOnly => {
                let action = match verdict.remote {
                    Change::Added => Action::Add,
                    Change::Modified => Action::Update,
                    _ => Action::Delete,
                };
                plan.actions.push((path.clone(), action));
            }
            State::Conflict => plan.conflicts.push(Conflict {
                path: path.clone(),
                reason: conflict_reason(verdict.local, verdict.remote).into(),
            }),
            _ => plan.unchanged += 1,
        }
    }
    plan
}

enum Fetch {
    Blobs(BTreeMap<String, Vec<u8>>),
    /// The head moved or a blob did not match the manifest: start over from a new manifest.
    Retry,
}

/// Download every path at one pinned head and verify each blob against the manifest.
async fn fetch_blobs(
    env: &Env,
    token: &SecretString,
    project_id: &str,
    head: &str,
    wanted: &BTreeMap<String, String>,
) -> Result<Fetch> {
    let mut blobs = BTreeMap::new();
    let mut queue = wanted.iter();
    let mut tasks: JoinSet<Result<(String, Option<Vec<u8>>)>> = JoinSet::new();
    loop {
        while tasks.len() < DOWNLOAD_CONCURRENCY {
            let Some((path, sha)) = queue.next() else {
                break;
            };
            let (client, token, project, head) = (
                env.client.clone(),
                SecretString::from(token.expose_secret().to_owned()),
                project_id.to_owned(),
                head.to_owned(),
            );
            let (path, sha) = (path.clone(), sha.clone());
            tasks.spawn(async move {
                match client.document(&token, &project, &path, Some(&head)).await {
                    Ok(doc) => {
                        let bytes = doc.content.into_bytes();
                        let ok = doc.path == path
                            && doc.head_sha == head
                            && git_blob_sha(&bytes) == sha
                            && doc.document_sha == sha;
                        Ok((path, ok.then_some(bytes)))
                    }
                    Err(Error::Api(e)) if e.status == 409 && e.code == "head_changed" => {
                        Ok((path, None))
                    }
                    Err(e) => Err(e),
                }
            });
        }
        let Some(joined) = tasks.join_next().await else {
            break;
        };
        match joined.map_err(|e| Error::Other(format!("download task failed: {e}")))?? {
            (path, Some(bytes)) => {
                blobs.insert(path, bytes);
            }
            (_, None) => {
                tasks.abort_all();
                return Ok(Fetch::Retry);
            }
        }
    }
    Ok(Fetch::Blobs(blobs))
}

pub async fn pull(env: &Env, ws: &Workspace) -> Result<PullReport> {
    let _guard = ws.guard(env, false)?;
    let session = Session::new(env);
    let auth = session.authenticate().await?;
    let result = pull_inner(env, ws, &auth).await;
    settle(&session, &auth, result).await
}

async fn pull_inner(env: &Env, ws: &Workspace, auth: &Authed) -> Result<PullReport> {
    let scan = ws.scan()?;
    require_safe(&scan)?;
    let mut manifest = ws.load_manifest()?;

    for _ in 0..PULL_ATTEMPTS {
        let remote: SyncManifest = env.client.sync(&auth.token, ws.project_id()).await?;
        let remote_paths = remote_shas(&remote)?;
        let plan = build_plan(&remote_paths, &manifest, &scan.files);
        for (path, _) in &plan.actions {
            ws.check_target(path)?;
        }

        let wanted: BTreeMap<String, String> = plan
            .actions
            .iter()
            .filter(|(_, a)| matches!(a, Action::Add | Action::Update))
            .map(|(p, _)| (p.clone(), remote_paths[p].clone()))
            .collect();
        let blobs = match fetch_blobs(env, &auth.token, ws.project_id(), &remote.head_sha, &wanted)
            .await?
        {
            Fetch::Blobs(blobs) => blobs,
            Fetch::Retry => continue,
        };
        return apply(ws, &mut manifest, &remote, &remote_paths, plan, blobs);
    }
    Err(Error::Other(
        "the remote kept changing while pulling; nothing was changed locally, try again".into(),
    ))
}

fn apply(
    ws: &Workspace,
    manifest: &mut Manifest,
    remote: &SyncManifest,
    remote_paths: &BTreeMap<String, String>,
    plan: Plan,
    blobs: BTreeMap<String, Vec<u8>>,
) -> Result<PullReport> {
    let mut report = PullReport {
        head_sha: remote.head_sha.clone(),
        added: vec![],
        updated: vec![],
        deleted: vec![],
        adopted: vec![],
        conflicts: plan.conflicts.clone(),
        unchanged: plan.unchanged,
        lock_advanced: false,
    };
    let sizes: BTreeMap<&str, u64> = remote
        .files
        .iter()
        .map(|f| (f.path.as_str(), f.size))
        .collect();

    let outcome = (|| -> Result<()> {
        for (path, action) in &plan.actions {
            match action {
                Action::Add | Action::Update => {
                    let bytes = &blobs[path];
                    // Baseline first: a crash between the two writes is repaired by the next pull.
                    ws.write_base(path, bytes)?;
                    ws.write_local(path, bytes)?;
                    manifest.files.insert(
                        path.clone(),
                        BaseEntry {
                            sha: remote_paths[path].clone(),
                            size: bytes.len() as u64,
                        },
                    );
                    if *action == Action::Add {
                        report.added.push(path.clone())
                    } else {
                        report.updated.push(path.clone())
                    }
                }
                Action::Delete => {
                    ws.remove_local(path)?;
                    ws.remove_base(path)?;
                    manifest.files.remove(path);
                    report.deleted.push(path.clone());
                }
                Action::Adopt => {
                    let bytes = ws
                        .read_local(path)?
                        .ok_or_else(|| Error::Other(format!("{path} disappeared while pulling")))?;
                    if git_blob_sha(&bytes) != remote_paths[path] {
                        return Err(Error::Other(format!(
                            "{path} changed while pulling; run `speq pull` again"
                        )));
                    }
                    ws.write_base(path, &bytes)?;
                    manifest.files.insert(
                        path.clone(),
                        BaseEntry {
                            sha: remote_paths[path].clone(),
                            size: sizes
                                .get(path.as_str())
                                .copied()
                                .unwrap_or(bytes.len() as u64),
                        },
                    );
                    report.adopted.push(path.clone());
                }
                Action::Forget => {
                    ws.remove_base(path)?;
                    manifest.files.remove(path);
                }
            }
        }
        Ok(())
    })();

    // Persist what was really applied even after a failure, so the state never claims more.
    let saved = ws.save_manifest(manifest);
    outcome?;
    saved?;

    // The lock moves last, and only when the whole manifest reconciled cleanly.
    if report.conflicts.is_empty() {
        ws.save_lock(&LockState {
            head_sha: Some(remote.head_sha.clone()),
            pending: vec![],
        })?;
        report.lock_advanced = true;
    } else {
        let mut lock = ws.load_lock()?;
        lock.pending = report.conflicts.iter().map(|c| c.path.clone()).collect();
        ws.save_lock(&lock)?;
    }
    Ok(report)
}

#[derive(Debug, Serialize)]
pub struct UpdateReport {
    pub path: String,
    pub result: &'static str,
    pub head_sha: String,
}

/// Safe-apply one file: refuse when the local copy changed. There is no force option.
pub async fn update(env: &Env, ws: &Workspace, path: &str) -> Result<UpdateReport> {
    ws.check_target(path)?;
    let _guard = ws.guard(env, false)?;
    let session = Session::new(env);
    let auth = session.authenticate().await?;
    let result = update_inner(env, ws, &auth, path).await;
    settle(&session, &auth, result).await
}

async fn update_inner(
    env: &Env,
    ws: &Workspace,
    auth: &Authed,
    path: &str,
) -> Result<UpdateReport> {
    let scan = ws.scan()?;
    require_safe(&scan)?;
    let mut manifest = ws.load_manifest()?;
    for _ in 0..PULL_ATTEMPTS {
        let remote = env.client.sync(&auth.token, ws.project_id()).await?;
        let remote_paths = remote_shas(&remote)?;
        let r = remote_paths.get(path).map(String::as_str);
        let b = manifest.files.get(path).map(|e| e.sha.clone());
        let l = scan.files.get(path).map(String::as_str);
        let verdict = classify(r, b.as_deref(), l);

        let done = |result: &'static str| UpdateReport {
            path: path.to_string(),
            result,
            head_sha: remote.head_sha.clone(),
        };
        if verdict.state == State::Conflict || verdict.local != Change::None && !verdict.converged {
            return Err(Error::conflict(format!(
                "{path} has local changes; update refuses to overwrite them (see `speq diff {path}`)"
            )));
        }
        if r.is_none() && b.is_none() && l.is_none() {
            return Err(Error::invalid(format!(
                "{path} does not exist locally or on the remote"
            )));
        }
        match (verdict.converged, verdict.remote) {
            (true, _) if r.is_some() => {
                let bytes = ws
                    .read_local(path)?
                    .ok_or_else(|| Error::Other(format!("{path} disappeared")))?;
                ws.write_base(path, &bytes)?;
                manifest.files.insert(
                    path.to_string(),
                    BaseEntry {
                        sha: remote_paths[path].clone(),
                        size: bytes.len() as u64,
                    },
                );
            }
            (true, _) => {
                ws.remove_base(path)?;
                manifest.files.remove(path);
            }
            (false, Change::None) => return Ok(done("up-to-date")),
            (false, Change::Deleted) => {
                ws.remove_local(path)?;
                ws.remove_base(path)?;
                manifest.files.remove(path);
            }
            (false, _) => {
                let wanted = BTreeMap::from([(path.to_string(), remote_paths[path].clone())]);
                let Fetch::Blobs(mut blobs) =
                    fetch_blobs(env, &auth.token, ws.project_id(), &remote.head_sha, &wanted)
                        .await?
                else {
                    continue;
                };
                let bytes = blobs.remove(path).expect("fetched path");
                ws.write_base(path, &bytes)?;
                ws.write_local(path, &bytes)?;
                manifest.files.insert(
                    path.to_string(),
                    BaseEntry {
                        sha: remote_paths[path].clone(),
                        size: bytes.len() as u64,
                    },
                );
            }
        }
        ws.save_manifest(&manifest)?;
        // Only a full pull may move the lock; resolving one path just clears its pending marker.
        let mut lock = ws.load_lock()?;
        if lock.pending.iter().any(|p| p == path) {
            lock.pending.retain(|p| p != path);
            ws.save_lock(&lock)?;
        }
        return Ok(done("updated"));
    }
    Err(Error::Other(
        "the remote kept changing; nothing was changed locally, try again".into(),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(sha: &str) -> BaseEntry {
        BaseEntry {
            sha: sha.into(),
            size: 1,
        }
    }

    #[test]
    fn plan_applies_remote_only_and_flags_conflicts() {
        let remote: BTreeMap<String, String> = [
            ("new.md", "n"),
            ("edit.md", "e2"),
            ("both.md", "b2"),
            ("kept.md", "k"),
        ]
        .iter()
        .map(|(p, s)| (p.to_string(), s.to_string()))
        .collect();
        let manifest = Manifest {
            files: [
                ("edit.md", "e1"),
                ("both.md", "b1"),
                ("kept.md", "k"),
                ("gone.md", "g"),
            ]
            .iter()
            .map(|(p, s)| (p.to_string(), entry(s)))
            .collect(),
        };
        let local: BTreeMap<String, String> = [
            ("edit.md", "e1"),
            ("both.md", "bX"),
            ("kept.md", "kX"),
            ("gone.md", "g"),
            ("mine.md", "m"),
        ]
        .iter()
        .map(|(p, s)| (p.to_string(), s.to_string()))
        .collect();
        let plan = build_plan(&remote, &manifest, &local);
        let get = |p: &str| plan.actions.iter().find(|(x, _)| x == p).map(|(_, a)| *a);
        assert_eq!(get("new.md"), Some(Action::Add));
        assert_eq!(get("edit.md"), Some(Action::Update));
        assert_eq!(get("gone.md"), Some(Action::Delete));
        assert_eq!(get("kept.md"), None); // local-only change is kept
        assert_eq!(get("mine.md"), None);
        assert_eq!(
            plan.conflicts
                .iter()
                .map(|c| c.path.as_str())
                .collect::<Vec<_>>(),
            vec!["both.md"]
        );
    }
}
