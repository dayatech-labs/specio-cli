//! `specio context <epic>/<feature>`: a deterministic list of paths from the local working copy.
use crate::error::{Error, Result};
use crate::paths::FEATURE_FILES;
use crate::workspace::{Workspace, ensure_valid_epic_feature};
use serde::Serialize;

#[derive(Debug, Serialize)]
pub struct ContextReport {
    pub project_id: String,
    pub head_sha: String,
    pub paths: Vec<String>,
}

pub fn context(ws: &Workspace, target: &str) -> Result<ContextReport> {
    let (epic, feature) = target
        .split_once('/')
        .filter(|(_, f)| !f.contains('/'))
        .ok_or_else(|| {
            Error::invalid(
                "use `specio context <epic>/<feature>`, for example `epic-payment/create-payment`",
            )
        })?;
    ensure_valid_epic_feature(epic, feature)?;

    let lock = ws.load_lock()?;
    let head_sha = lock.head_sha.clone().filter(|_| lock.pending.is_empty()).ok_or_else(|| Error::conflict("the workspace is not clean; run `specio pull` and resolve conflicts before generating context"))?;

    let mut candidates = vec!["llms.txt".to_string()];
    candidates.extend(
        ["product-brief.md", "prd.md", "architecture.md"]
            .iter()
            .map(|f| format!("{epic}/{f}")),
    );
    let feature_paths: Vec<String> = FEATURE_FILES
        .iter()
        .map(|f| format!("{epic}/features/{feature}/{f}"))
        .collect();

    let exists = |p: &str| ws.local_path(p).is_file() && ws.check_target(p).is_ok();
    if !feature_paths.iter().any(|p| exists(p)) {
        return Err(Error::invalid(format!(
            "no documents found for {epic}/{feature}; check the names with `specio status`"
        )));
    }
    candidates.extend(feature_paths);

    let prefix = &ws.config.spec_source.local_dir;
    let paths = candidates
        .into_iter()
        .filter(|p| exists(p))
        .map(|p| format!("{prefix}/{p}"))
        .collect();
    Ok(ContextReport {
        project_id: ws.project_id().to_string(),
        head_sha,
        paths,
    })
}
