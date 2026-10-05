//! The workspace: `.speq/` metadata (config, lock, manifest, baseline copies) and the
//! `specs/` working copy, with safe path handling and atomic writes throughout.
use crate::env::Env;
use crate::error::{Error, IoContext, Result};
use crate::fsx::{self, FileLock, Visibility};
use crate::hashing::git_blob_sha;
use crate::paths::{self, is_canonical_markdown, is_valid_local_dir, join_canonical};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SpecSource {
    pub project_id: String,
    pub repository: String,
    pub branch: String,
    pub local_dir: String,
    pub repository_type: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Config {
    pub spec_source: SpecSource,
}

/// Baseline of one file as last applied from the remote.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct BaseEntry {
    /// Git blob SHA of the baseline content (also of the file when it is unchanged locally).
    pub sha: String,
    pub size: u64,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct Manifest {
    #[serde(default)]
    pub files: BTreeMap<String, BaseEntry>,
}

/// `head_sha` advances only after a pull reconciled every path; `pending` lists unresolved conflicts.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct LockState {
    pub head_sha: Option<String>,
    #[serde(default)]
    pub pending: Vec<String>,
}

impl LockState {
    pub fn is_clean(&self) -> bool {
        self.head_sha.is_some() && self.pending.is_empty()
    }
}

#[derive(Debug, Default)]
pub struct LocalScan {
    /// Canonical path → Git blob SHA of the local content.
    pub files: BTreeMap<String, String>,
    /// Files ignored with the reason (not canonical Markdown, hidden directory, non-UTF-8 name).
    pub skipped: Vec<(String, &'static str)>,
    /// Symlinks and special files inside the working copy: never followed, never sent.
    pub unsafe_paths: Vec<String>,
}

#[derive(Debug, Clone)]
pub struct Workspace {
    pub root: PathBuf,
    pub config: Config,
}

pub const SPEQ_DIR: &str = ".speq";

impl Workspace {
    pub fn discover(start: &Path) -> Result<Workspace> {
        let start = fs::canonicalize(start).ctx(format!("resolve {}", start.display()))?;
        for dir in start.ancestors() {
            if dir.join(SPEQ_DIR).join("config.toml").is_file() {
                return Workspace::open(dir);
            }
        }
        Err(Error::NotInitialized)
    }

    pub fn open(root: &Path) -> Result<Workspace> {
        let path = root.join(SPEQ_DIR).join("config.toml");
        let raw = fs::read_to_string(&path).ctx(format!("read {}", path.display()))?;
        let config: Config = toml::from_str(&raw)
            .map_err(|e| Error::invalid(format!("{} is invalid: {e}", path.display())))?;
        if !is_valid_local_dir(&config.spec_source.local_dir) {
            return Err(Error::invalid(format!(
                "{}: local_dir {:?} must be a relative directory inside the workspace",
                path.display(),
                config.spec_source.local_dir
            )));
        }
        Ok(Workspace {
            root: root.to_path_buf(),
            config,
        })
    }

    pub fn create(root: &Path, config: &Config) -> Result<Workspace> {
        let dir = root.join(SPEQ_DIR);
        let raw = toml::to_string_pretty(config).map_err(|e| Error::Other(e.to_string()))?;
        fsx::atomic_write(&dir.join("config.toml"), raw.as_bytes(), Visibility::Shared)?;
        let ws = Workspace {
            root: root.to_path_buf(),
            config: config.clone(),
        };
        fs::create_dir_all(ws.specs_dir()).ctx(format!("create {}", ws.specs_dir().display()))?;
        Ok(ws)
    }

    pub fn speq_dir(&self) -> PathBuf {
        self.root.join(SPEQ_DIR)
    }
    pub fn project_id(&self) -> &str {
        &self.config.spec_source.project_id
    }
    pub fn specs_dir(&self) -> PathBuf {
        self.config
            .spec_source
            .local_dir
            .split('/')
            .fold(self.root.clone(), |acc, s| acc.join(s))
    }
    fn lock_path(&self) -> PathBuf {
        self.speq_dir().join("lock.json")
    }
    fn manifest_path(&self) -> PathBuf {
        self.speq_dir().join("manifest.json")
    }
    pub fn base_dir(&self) -> PathBuf {
        self.speq_dir().join("base")
    }

    /// Serialise CLI processes working on this workspace; fails after a short grace when another one is running.
    pub fn guard(&self, env: &Env, shared: bool) -> Result<FileLock> {
        FileLock::try_acquire_within(
            &env.dirs.workspace_lock(&self.root),
            shared,
            std::time::Duration::from_secs(1),
        )?
        .ok_or_else(|| {
            Error::conflict(
                "another speq command is already running in this workspace; wait for it to finish",
            )
        })
    }

    // ------------------------------------------------------------ metadata

    fn load_json<T: for<'de> Deserialize<'de> + Default>(&self, path: &Path) -> Result<T> {
        match fsx::read_optional(path)? {
            None => Ok(T::default()),
            Some(bytes) => serde_json::from_slice(&bytes).map_err(|e| {
                Error::invalid(format!(
                    "{} is corrupt ({e}); remove it and run `speq pull`",
                    path.display()
                ))
            }),
        }
    }

    fn save_json<T: Serialize>(&self, path: &Path, value: &T) -> Result<()> {
        let bytes = serde_json::to_vec_pretty(value).map_err(|e| Error::Other(e.to_string()))?;
        fsx::atomic_write(path, &bytes, Visibility::Shared)
    }

    pub fn load_manifest(&self) -> Result<Manifest> {
        self.load_json(&self.manifest_path())
    }
    pub fn save_manifest(&self, manifest: &Manifest) -> Result<()> {
        self.save_json(&self.manifest_path(), manifest)
    }
    pub fn load_lock(&self) -> Result<LockState> {
        self.load_json(&self.lock_path())
    }
    pub fn save_lock(&self, lock: &LockState) -> Result<()> {
        self.save_json(&self.lock_path(), lock)
    }
    /// Reconfiguring to another project must not reuse sync state that belongs to the old one.
    pub fn reset_sync_state(&self) -> Result<()> {
        fsx::remove_optional(&self.lock_path())?;
        fsx::remove_optional(&self.manifest_path())?;
        match fs::remove_dir_all(self.base_dir()) {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(Error::Io {
                context: "remove baseline copies".into(),
                source: e,
            }),
        }
    }

    // ------------------------------------------------------ working copy

    pub fn local_path(&self, canonical: &str) -> PathBuf {
        join_canonical(&self.specs_dir(), canonical)
    }
    fn base_path(&self, canonical: &str) -> PathBuf {
        join_canonical(&self.base_dir(), canonical)
    }

    /// Reject targets that are not canonical documents, or whose path crosses a symlink.
    pub fn check_target(&self, canonical: &str) -> Result<()> {
        if !is_canonical_markdown(canonical) {
            return Err(Error::invalid(format!(
                "{canonical:?} is not a canonical specs document path"
            )));
        }
        let mut current = self.root.clone();
        let parts = self
            .config
            .spec_source
            .local_dir
            .split('/')
            .chain(canonical.split('/'));
        for part in parts {
            current.push(part);
            match fs::symlink_metadata(&current) {
                Ok(meta) if meta.file_type().is_symlink() => {
                    return Err(Error::invalid(format!(
                        "refusing to use {}: it is a symbolic link",
                        current.display()
                    )));
                }
                Ok(_) => {}
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => break,
                Err(e) => {
                    return Err(Error::Io {
                        context: format!("inspect {}", current.display()),
                        source: e,
                    });
                }
            }
        }
        if current == self.local_path(canonical) && current.exists() && !current.is_file() {
            return Err(Error::invalid(format!(
                "{} exists but is not a regular file",
                current.display()
            )));
        }
        Ok(())
    }

    pub fn read_local(&self, canonical: &str) -> Result<Option<Vec<u8>>> {
        self.check_target(canonical)?;
        fsx::read_optional(&self.local_path(canonical))
    }

    pub fn write_local(&self, canonical: &str, bytes: &[u8]) -> Result<()> {
        self.check_target(canonical)?;
        fsx::atomic_write(&self.local_path(canonical), bytes, Visibility::Shared)
    }

    pub fn remove_local(&self, canonical: &str) -> Result<()> {
        self.check_target(canonical)?;
        let path = self.local_path(canonical);
        fsx::remove_optional(&path)?;
        // Tidy up now-empty folders, but never the working-copy root itself.
        let stop = self.specs_dir();
        let mut dir = path.parent().map(Path::to_path_buf);
        while let Some(d) = dir {
            if d == stop || fs::remove_dir(&d).is_err() {
                break;
            }
            dir = d.parent().map(Path::to_path_buf);
        }
        Ok(())
    }

    pub fn read_base(&self, canonical: &str) -> Result<Option<Vec<u8>>> {
        fsx::read_optional(&self.base_path(canonical))
    }
    pub fn write_base(&self, canonical: &str, bytes: &[u8]) -> Result<()> {
        fsx::atomic_write(&self.base_path(canonical), bytes, Visibility::Shared)
    }
    pub fn remove_base(&self, canonical: &str) -> Result<()> {
        fsx::remove_optional(&self.base_path(canonical))
    }

    /// Hash every canonical Markdown file under `local_dir`. Symlinks are reported, never followed.
    pub fn scan(&self) -> Result<LocalScan> {
        let mut scan = LocalScan::default();
        let specs = self.specs_dir();
        match fs::symlink_metadata(&specs) {
            Ok(meta) if meta.file_type().is_symlink() => {
                scan.unsafe_paths
                    .push(self.config.spec_source.local_dir.clone());
                return Ok(scan);
            }
            Ok(_) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(scan),
            Err(e) => {
                return Err(Error::Io {
                    context: format!("inspect {}", specs.display()),
                    source: e,
                });
            }
        }
        walk(&specs, "", &mut scan)?;
        Ok(scan)
    }
}

fn walk(dir: &Path, prefix: &str, scan: &mut LocalScan) -> Result<()> {
    let entries = fs::read_dir(dir).ctx(format!("read {}", dir.display()))?;
    let mut entries: Vec<_> = entries
        .collect::<std::io::Result<_>>()
        .ctx(format!("read {}", dir.display()))?;
    entries.sort_by_key(|e| e.file_name());
    for entry in entries {
        let name = entry.file_name();
        let Some(name) = name.to_str() else {
            scan.skipped.push((
                format!("{prefix}{}", name.to_string_lossy()),
                "file name is not valid UTF-8",
            ));
            continue;
        };
        let rel = format!("{prefix}{name}");
        let kind = entry.file_type().ctx(format!("inspect {rel}"))?;
        if kind.is_symlink() {
            scan.unsafe_paths.push(rel);
        } else if kind.is_dir() {
            if name.starts_with('.') {
                scan.skipped.push((rel, "hidden directory"));
            } else {
                walk(&entry.path(), &format!("{rel}/"), scan)?;
            }
        } else if kind.is_file() {
            if is_canonical_markdown(&rel) {
                let bytes = fs::read(entry.path()).ctx(format!("read {rel}"))?;
                scan.files.insert(rel, git_blob_sha(&bytes));
            } else if !name.starts_with('.') {
                scan.skipped
                    .push((rel, "not a canonical Markdown document path"));
            }
        } else {
            scan.unsafe_paths.push(rel);
        }
    }
    Ok(())
}

/// Lines `init` keeps out of Git: the working copy and per-machine sync state.
pub fn gitignore_entries(local_dir: &str) -> [String; 4] {
    [
        format!("{local_dir}/"),
        ".speq/lock.json".into(),
        ".speq/manifest.json".into(),
        ".speq/base/".into(),
    ]
}

/// Append any missing entries to `.gitignore`, leaving existing lines untouched.
pub fn ensure_gitignore(root: &Path, local_dir: &str) -> Result<()> {
    let path = root.join(".gitignore");
    let existing = fsx::read_optional(&path)?
        .map(|b| String::from_utf8_lossy(&b).into_owned())
        .unwrap_or_default();
    let present: Vec<&str> = existing.lines().map(str::trim).collect();
    let missing: Vec<String> = gitignore_entries(local_dir)
        .into_iter()
        .filter(|e| !present.contains(&e.as_str()))
        .collect();
    if missing.is_empty() {
        return Ok(());
    }
    let mut updated = existing.clone();
    if !updated.is_empty() && !updated.ends_with('\n') {
        updated.push('\n');
    }
    if !updated.is_empty() {
        updated.push('\n');
    }
    updated.push_str("# Speq\n");
    for entry in missing {
        updated.push_str(&entry);
        updated.push('\n');
    }
    fsx::atomic_write(&path, updated.as_bytes(), Visibility::Shared)
}

pub fn ensure_valid_epic_feature(epic: &str, feature: &str) -> Result<()> {
    if paths::is_folder_name(epic) && paths::is_folder_name(feature) {
        Ok(())
    } else {
        Err(Error::invalid(
            "the epic and the feature must be lowercase folder names such as `epic-payment/create-payment`",
        ))
    }
}
