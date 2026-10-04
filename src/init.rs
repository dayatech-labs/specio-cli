//! `specio list` and `specio init`.
use crate::api::ProjectSummary;
use crate::env::Env;
use crate::error::{Error, Result};
use crate::paths::is_valid_local_dir;
use crate::session::Session;
use crate::workspace::{Config, SPECIO_DIR, SpecSource, Workspace, ensure_gitignore};
use serde::Serialize;
use std::path::Path;

pub const REPOSITORY_TYPES: [&str; 3] = ["frontend", "backend", "cli"];

#[derive(Debug, Serialize)]
pub struct ListReport {
    pub user_id: String,
    pub expires_at: String,
    pub projects: Vec<ProjectSummary>,
}

/// From the snapshot when it is still valid; refreshed in the foreground otherwise.
pub async fn list(env: &Env) -> Result<ListReport> {
    let snapshot = Session::new(env).snapshot().await?;
    Ok(ListReport {
        user_id: snapshot.user_id,
        expires_at: snapshot
            .expires_at
            .format(&time::format_description::well_known::Rfc3339)
            .unwrap_or_default(),
        projects: snapshot.projects,
    })
}

/// `owner/repo` matches exactly; a bare name must be unique among the projects the user can use.
pub fn resolve_repo<'a>(
    projects: &'a [ProjectSummary],
    wanted: &str,
) -> Result<&'a ProjectSummary> {
    let bound: Vec<(&ProjectSummary, &str)> = projects
        .iter()
        .filter_map(|p| p.repository_full_name.as_deref().map(|r| (p, r)))
        .collect();
    let matches: Vec<&(&ProjectSummary, &str)> = if wanted.contains('/') {
        bound
            .iter()
            .filter(|(_, full)| full.eq_ignore_ascii_case(wanted))
            .collect()
    } else {
        bound
            .iter()
            .filter(|(_, full)| {
                full.rsplit('/')
                    .next()
                    .is_some_and(|name| name.eq_ignore_ascii_case(wanted))
            })
            .collect()
    };
    match matches.as_slice() {
        [] => Err(Error::invalid(format!(
            "{wanted:?} is not in your repository list; see `specio list`"
        ))),
        [(project, _)] => Ok(project),
        many => Err(Error::invalid(format!(
            "{wanted:?} is ambiguous; use the full name: {}",
            many.iter()
                .map(|(_, full)| *full)
                .collect::<Vec<_>>()
                .join(", ")
        ))),
    }
}

#[derive(Debug, Serialize)]
pub struct InitReport {
    pub project_id: String,
    pub repository: String,
    pub branch: String,
    pub local_dir: String,
    pub repository_type: String,
    pub reconfigured: bool,
    /// Lock, manifest, and baselines were cleared because the source changed.
    pub sync_state_reset: bool,
}

pub struct InitOptions<'a> {
    pub repo: &'a str,
    pub repository_type: &'a str,
    pub local_dir: &'a str,
    pub reconfigure: bool,
}

pub async fn init(env: &Env, root: &Path, options: InitOptions<'_>) -> Result<InitReport> {
    let existing = root.join(SPECIO_DIR).join("config.toml").exists();
    if existing && !options.reconfigure {
        return Err(Error::invalid(
            "this directory is already initialized; run `specio init <repo> --type <type> --reconfigure` to change it",
        ));
    }
    if !REPOSITORY_TYPES.contains(&options.repository_type) {
        return Err(Error::invalid(format!(
            "--type must be one of {}",
            REPOSITORY_TYPES.join(", ")
        )));
    }
    if !is_valid_local_dir(options.local_dir) {
        return Err(Error::invalid(
            "--local-dir must be a relative folder inside the repository, without `..` or hidden segments",
        ));
    }

    // The list comes from the API (via the snapshot); it never lets a user pick an unlisted repository.
    let snapshot = Session::new(env).snapshot().await?;
    let project = resolve_repo(&snapshot.projects, options.repo)?;
    let config = Config {
        spec_source: SpecSource {
            project_id: project.project_id.clone(),
            repository: project.repository_full_name.clone().unwrap_or_default(),
            branch: project.branch.clone(),
            local_dir: options.local_dir.to_string(),
            repository_type: options.repository_type.to_string(),
        },
    };

    let mut reset = false;
    if existing {
        let previous = Workspace::open(root)?;
        let moved = previous.config.spec_source.project_id != config.spec_source.project_id
            || previous.config.spec_source.branch != config.spec_source.branch
            || previous.config.spec_source.local_dir != config.spec_source.local_dir;
        if moved {
            previous.reset_sync_state()?;
            reset = true;
        }
    }
    let ws = Workspace::create(root, &config)?;
    ensure_gitignore(&ws.root, &config.spec_source.local_dir)?;
    Ok(InitReport {
        project_id: config.spec_source.project_id,
        repository: config.spec_source.repository,
        branch: config.spec_source.branch,
        local_dir: config.spec_source.local_dir,
        repository_type: config.spec_source.repository_type,
        reconfigured: existing,
        sync_state_reset: reset,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::{Capabilities, RuleSet};

    fn project(id: &str, repo: Option<&str>) -> ProjectSummary {
        let rules = RuleSet {
            allow: vec![],
            deny: vec![],
        };
        ProjectSummary {
            project_id: id.into(),
            display_name: id.into(),
            repository_full_name: repo.map(Into::into),
            branch: "main".into(),
            roles: vec![],
            capabilities: Capabilities {
                create: rules.clone(),
                update: rules,
                can_delete: false,
                can_restore: false,
                can_manage: false,
            },
        }
    }

    #[test]
    fn short_names_must_be_unique() {
        let projects = vec![
            project("1", Some("acme/payment-specs")),
            project("2", Some("other/payment-specs")),
            project("3", Some("acme/cart-specs")),
            project("4", None),
        ];
        assert_eq!(
            resolve_repo(&projects, "cart-specs").unwrap().project_id,
            "3"
        );
        assert_eq!(
            resolve_repo(&projects, "Other/Payment-Specs")
                .unwrap()
                .project_id,
            "2"
        );
        let ambiguous = resolve_repo(&projects, "payment-specs")
            .unwrap_err()
            .to_string();
        assert!(
            ambiguous.contains("acme/payment-specs") && ambiguous.contains("other/payment-specs")
        );
        assert!(resolve_repo(&projects, "missing").is_err());
    }
}
