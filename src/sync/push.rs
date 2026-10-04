//! `specio push`: send local changes as one batch through the API. Never merges, never retries
//! a mutation on its own, and never moves the lock unless the API accepted the whole batch.
use super::{require_safe, settle};
use crate::api::{ChangeOperation, ChangesRequest};
use crate::env::Env;
use crate::error::{Error, Result};
use crate::hashing::{git_blob_sha, sha256_hex};
use crate::session::Session;
use crate::workspace::{BaseEntry, LockState, Workspace};
use serde::Serialize;
use std::collections::BTreeMap;

const MAX_OPERATIONS: usize = 50;
const MAX_DOCUMENT_BYTES: usize = 1024 * 1024;

#[derive(Debug, Serialize)]
pub struct PlannedChange {
    pub action: &'static str,
    pub path: String,
}

#[derive(Debug, Serialize)]
pub struct PushReport {
    pub changes: Vec<PlannedChange>,
    pub commit_sha: Option<String>,
    pub head_sha: Option<String>,
    pub replayed: bool,
    pub nothing_to_push: bool,
}

/// What would be sent, with the content kept in memory so the baseline equals what was sent.
pub struct Draft {
    pub operations: Vec<ChangeOperation>,
    pub contents: BTreeMap<String, Vec<u8>>,
}

pub fn build_draft(ws: &Workspace) -> Result<Draft> {
    let scan = ws.scan()?;
    require_safe(&scan)?;
    let manifest = ws.load_manifest()?;
    let mut operations = Vec::new();
    let mut contents = BTreeMap::new();

    let mut paths: Vec<&String> = scan.files.keys().chain(manifest.files.keys()).collect();
    paths.sort();
    paths.dedup();
    for path in paths {
        let base = manifest.files.get(path);
        match (base, scan.files.get(path)) {
            (Some(b), None) => operations.push(ChangeOperation::Delete {
                path: path.clone(),
                document_sha: b.sha.clone(),
            }),
            (b, Some(local_sha)) if b.map(|e| e.sha.as_str()) != Some(local_sha.as_str()) => {
                let bytes = ws
                    .read_local(path)?
                    .ok_or_else(|| Error::Other(format!("{path} disappeared while scanning")))?;
                if git_blob_sha(&bytes) != *local_sha {
                    return Err(Error::conflict(format!(
                        "{path} changed while scanning; run `specio push` again"
                    )));
                }
                if bytes.len() > MAX_DOCUMENT_BYTES {
                    return Err(Error::invalid(format!(
                        "{path} is larger than 1 MiB, the limit for one document"
                    )));
                }
                let content = String::from_utf8(bytes.clone())
                    .map_err(|_| Error::invalid(format!("{path} is not valid UTF-8 text")))?;
                if content.is_empty() || content.contains('\0') {
                    return Err(Error::invalid(format!(
                        "{path} must not be empty or contain NUL characters"
                    )));
                }
                operations.push(match b {
                    Some(b) => ChangeOperation::Update {
                        path: path.clone(),
                        content,
                        document_sha: b.sha.clone(),
                    },
                    None => ChangeOperation::Create {
                        path: path.clone(),
                        content,
                    },
                });
                contents.insert(path.clone(), bytes);
            }
            _ => {}
        }
    }
    Ok(Draft {
        operations,
        contents,
    })
}

/// Same intent ⇒ same key, so repeating an interrupted push finalises it instead of committing twice.
pub fn idempotency_key(base_commit_sha: &str, operations: &[ChangeOperation]) -> String {
    let canonical =
        serde_json::to_string(&(base_commit_sha, operations)).expect("operations serialise");
    format!("specio-{}", &sha256_hex(canonical.as_bytes())[..40])
}

pub async fn push(env: &Env, ws: &Workspace) -> Result<PushReport> {
    let _guard = ws.guard(env, false)?;
    let lock = ws.load_lock()?;
    let Some(base_head) = lock.head_sha.clone().filter(|_| lock.pending.is_empty()) else {
        let detail = if lock.pending.is_empty() {
            "no pull has completed yet".to_string()
        } else {
            format!("unresolved: {}", lock.pending.join(", "))
        };
        return Err(Error::conflict(format!(
            "the workspace is not clean ({detail}); resolve with `specio status`, `specio diff <path>`, and `specio pull` before pushing"
        )));
    };

    let draft = build_draft(ws)?;
    let changes: Vec<PlannedChange> = draft
        .operations
        .iter()
        .map(|o| PlannedChange {
            action: o.action(),
            path: o.path().to_string(),
        })
        .collect();
    if draft.operations.is_empty() {
        return Ok(PushReport {
            changes,
            commit_sha: None,
            head_sha: None,
            replayed: false,
            nothing_to_push: true,
        });
    }
    if draft.operations.len() > MAX_OPERATIONS {
        return Err(Error::invalid(format!(
            "{} changed files exceed the batch limit of {MAX_OPERATIONS}; push fewer files at a time",
            draft.operations.len()
        )));
    }
    // The summary is shown before anything is sent.
    eprintln!("Pushing {} change(s):", changes.len());
    for c in &changes {
        eprintln!("  {:<7} {}", c.action, c.path);
    }

    let session = Session::new(env);
    let auth = session.authenticate().await?;
    let key = idempotency_key(&base_head, &draft.operations);
    let request = ChangesRequest {
        base_commit_sha: &base_head,
        operations: &draft.operations,
    };
    let result = env
        .client
        .changes(&auth.token, ws.project_id(), &key, &request)
        .await;
    let result = match settle(&session, &auth, result).await {
        Ok(r) => r,
        Err(Error::Api(e)) if e.status == 409 => {
            return Err(Error::conflict(format!(
                "the push was rejected ({}) and nothing was committed; your local files are untouched. Run `specio status`, `specio diff <path>`, then `specio pull`",
                e.message
            )));
        }
        Err(e) => return Err(e),
    };

    // The API committed on top of our base head, so the new head contains exactly our changes.
    let mut manifest = ws.load_manifest()?;
    let applied = (|| -> Result<()> {
        for op in &result.operations {
            match op.action.as_str() {
                "delete" => {
                    ws.remove_base(&op.path)?;
                    manifest.files.remove(&op.path);
                }
                _ => {
                    let bytes = draft.contents.get(&op.path).ok_or_else(|| {
                        Error::Other(format!("the API reported an unexpected path {}", op.path))
                    })?;
                    ws.write_base(&op.path, bytes)?;
                    let sha = op
                        .document_sha
                        .clone()
                        .unwrap_or_else(|| git_blob_sha(bytes));
                    manifest.files.insert(
                        op.path.clone(),
                        BaseEntry {
                            sha,
                            size: bytes.len() as u64,
                        },
                    );
                }
            }
        }
        Ok(())
    })();
    let saved = ws.save_manifest(&manifest);
    if let Err(e) = applied.and(saved) {
        return Err(Error::Other(format!(
            "the push succeeded (commit {}) but local state could not be updated: {e}; run `specio pull`",
            result.commit_sha
        )));
    }
    ws.save_lock(&LockState {
        head_sha: Some(result.head_sha.clone()),
        pending: vec![],
    })?;
    Ok(PushReport {
        changes,
        commit_sha: Some(result.commit_sha),
        head_sha: Some(result.head_sha),
        replayed: result.replayed,
        nothing_to_push: false,
    })
}
