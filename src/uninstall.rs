//! `speq uninstall`: remove this machine's Speq state and, when the official installer placed it, the binary.
//! Workspaces are never touched, and the server-side session is not ended (that is `speq logout --all`).
use crate::env::Env;
use crate::error::{IoContext, Result};
use crate::fsx::FileLock;
use crate::session::Session;
use crate::upgrade::Install;
use serde::Serialize;
use std::path::Path;

#[derive(Debug, Serialize, PartialEq, Eq)]
pub struct UninstallReport {
    /// False when the credential store could not be cleaned (see `manual_steps`).
    pub credential_removed: bool,
    /// The per-user data directory that was removed (cache, locks).
    pub data_dir: String,
    /// The binary that was deleted; only an official install is ours to delete.
    pub removed_binary: Option<String>,
    /// What is left for the user to do by hand.
    pub manual_steps: Vec<String>,
}

/// Everything is attempted, and what could not be done is reported instead of aborting: an uninstall that stops at
/// an unreachable credential store (headless Linux, a locked keychain) could never be completed.
pub fn uninstall(env: &Env, exe: &Path, install: &Install) -> Result<UninstallReport> {
    let mut manual_steps = Vec::new();
    let mut credential_removed = true;
    {
        let _lock = FileLock::exclusive(&env.auth_lock_path())?;
        // Tries the credential, the snapshot, and the background agent before it reports a failure.
        if let Err(e) = Session::new(env).clear_local() {
            credential_removed = false;
            manual_steps.push(format!(
                "the stored credential could not be fully removed ({e}); delete the speq entry from your OS credential store by hand (the token expires on its own)"
            ));
        }
    }
    match std::fs::remove_dir_all(env.dirs.root()) {
        Err(e) if e.kind() != std::io::ErrorKind::NotFound => {
            return Err(e).ctx(format!("remove {}", env.dirs.root().display()));
        }
        _ => {}
    }

    let removed_binary = match install {
        Install::Official => {
            remove_binary(exe)?;
            manual_steps.extend(official_leftovers(exe));
            Some(exe.display().to_string())
        }
        Install::Managed {
            manager, uninstall, ..
        } => {
            manual_steps.push(format!(
                "speq is managed by {manager}; run `{uninstall}` to remove the program"
            ));
            None
        }
        Install::Unmanaged => {
            manual_steps.push(format!(
                "{} was not installed by the official installer, so it was left in place; delete it yourself",
                exe.display()
            ));
            None
        }
    };
    Ok(UninstallReport {
        credential_removed,
        data_dir: env.dirs.root().display().to_string(),
        removed_binary,
        manual_steps,
    })
}

#[cfg(not(windows))]
fn remove_binary(exe: &Path) -> Result<()> {
    // A running executable can be unlinked on Unix.
    std::fs::remove_file(exe).ctx(format!("remove {}", exe.display()))
}

/// A running `.exe` cannot be deleted on Windows, but it can be renamed away (as `upgrade` does).
#[cfg(windows)]
fn remove_binary(exe: &Path) -> Result<()> {
    let old = exe.with_extension("exe.old");
    let _ = std::fs::remove_file(&old);
    std::fs::rename(exe, &old).ctx(format!("move {} aside", exe.display()))
}

/// The installer edits the shell profile (Unix) or the user PATH (Windows); undoing that is the user's call.
fn official_leftovers(exe: &Path) -> Vec<String> {
    let dir = exe.parent().map(|p| p.display().to_string());
    if cfg!(windows) {
        let mut steps = vec![format!(
            "delete {} once this command has exited (a running program cannot delete itself on Windows)",
            exe.with_extension("exe.old").display()
        )];
        steps.extend(dir.map(|d| {
            format!("{d} may still be on your user PATH; remove it in Environment Variables if nothing else lives there")
        }));
        steps
    } else {
        vec![
            "the installer may have added ~/.local/bin to your PATH with a `# added by the speq installer` line in your shell profile (~/.zshrc, ~/.bashrc, ~/.bash_profile, or ~/.profile); remove it if nothing else lives there"
                .into(),
        ]
    }
}
