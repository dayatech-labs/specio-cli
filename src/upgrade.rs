//! `speq upgrade`: verify and atomically replace the installed binary. It never runs on its own.
use crate::api::install_crypto_provider;
use crate::error::{Error, IoContext, Result};
use crate::hashing::sha256_hex;
use semver::Version;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::Duration;
use tempfile::NamedTempFile;
use url::Url;

pub const DEFAULT_RELEASE_BASE: &str = "https://github.com/dayatech-labs/speq-cli/releases";
pub const CURRENT_TARGET: &str = env!("SPEQ_TARGET");
const MAX_MANIFEST_BYTES: usize = 1024 * 1024;
const MAX_BINARY_BYTES: u64 = 200 * 1024 * 1024;

#[derive(Debug, Clone, Deserialize)]
pub struct Asset {
    pub url: String,
    pub sha256: String,
    pub size: u64,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Artifact {
    /// The bare executable, which is what installers and `upgrade` place on disk.
    pub binary: Asset,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ReleaseManifest {
    pub version: Version,
    pub commit: Option<String>,
    pub artifacts: BTreeMap<String, Artifact>,
}

#[derive(Debug, Serialize, PartialEq, Eq)]
pub struct CheckReport {
    pub current: String,
    pub latest: String,
    pub status: &'static str,
    pub target: &'static str,
}

#[derive(Debug, Serialize)]
pub struct UpgradeReport {
    pub from: String,
    pub to: String,
    pub installed: bool,
    pub path: Option<String>,
}

fn allowed_url(url: &Url) -> bool {
    let loopback = matches!(url.host_str(), Some("localhost" | "127.0.0.1" | "[::1]"));
    url.scheme() == "https" || (url.scheme() == "http" && loopback)
}

pub fn http_client() -> Result<reqwest::Client> {
    install_crypto_provider();
    reqwest::Client::builder()
        .user_agent(format!("speq-cli/{}", env!("CARGO_PKG_VERSION")))
        .timeout(Duration::from_secs(120))
        .connect_timeout(Duration::from_secs(10))
        // Release assets redirect to a CDN; follow only to https (or loopback in tests).
        .redirect(reqwest::redirect::Policy::custom(|attempt| {
            if allowed_url(attempt.url()) && attempt.previous().len() < 5 {
                attempt.follow()
            } else {
                attempt.stop()
            }
        }))
        .build()
        .map_err(|e| Error::Other(format!("cannot build the HTTP client: {e}")))
}

pub fn manifest_url(base: &str, version: Option<&Version>) -> Result<Url> {
    let base = base.trim_end_matches('/');
    let raw = match version {
        None => format!("{base}/latest/download/manifest.json"),
        Some(v) => format!("{base}/download/v{v}/manifest.json"),
    };
    let url = Url::parse(&raw).map_err(|e| Error::invalid(format!("invalid release URL: {e}")))?;
    if !allowed_url(&url) {
        return Err(Error::invalid("the release URL must use https"));
    }
    Ok(url)
}

async fn get_limited(http: &reqwest::Client, url: &Url, limit: u64) -> Result<Vec<u8>> {
    let response = http.get(url.clone()).send().await.map_err(|e| {
        Error::Network(format!(
            "cannot download {}: {e}",
            url.host_str().unwrap_or("release")
        ))
    })?;
    if !response.status().is_success() {
        return Err(Error::Network(format!(
            "{} answered HTTP {}",
            url.host_str().unwrap_or("the release server"),
            response.status().as_u16()
        )));
    }
    if response.content_length().is_some_and(|n| n > limit) {
        return Err(Error::Other("the download is larger than expected".into()));
    }
    let bytes = response
        .bytes()
        .await
        .map_err(|e| Error::Network(format!("download interrupted: {e}")))?;
    if bytes.len() as u64 > limit {
        return Err(Error::Other("the download is larger than expected".into()));
    }
    Ok(bytes.to_vec())
}

/// A network failure or an invalid manifest is an error, never "already up to date".
pub async fn fetch_manifest(http: &reqwest::Client, url: &Url) -> Result<ReleaseManifest> {
    let bytes = get_limited(http, url, MAX_MANIFEST_BYTES as u64).await?;
    let manifest: ReleaseManifest = serde_json::from_slice(&bytes)
        .map_err(|e| Error::Other(format!("the release manifest is invalid: {e}")))?;
    for (target, artifact) in &manifest.artifacts {
        let url = Url::parse(&artifact.binary.url).map_err(|_| {
            Error::Other(format!(
                "the release manifest has an invalid URL for {target}"
            ))
        })?;
        let hash_ok = artifact.binary.sha256.len() == 64
            && artifact
                .binary
                .sha256
                .chars()
                .all(|c| c.is_ascii_hexdigit());
        if !allowed_url(&url)
            || !hash_ok
            || artifact.binary.size == 0
            || artifact.binary.size > MAX_BINARY_BYTES
        {
            return Err(Error::Other(format!(
                "the release manifest entry for {target} is invalid"
            )));
        }
    }
    Ok(manifest)
}

pub fn current_version() -> Version {
    Version::parse(env!("CARGO_PKG_VERSION")).expect("package version is semver")
}

pub fn check(current: &Version, manifest: &ReleaseManifest) -> CheckReport {
    CheckReport {
        current: current.to_string(),
        latest: manifest.version.to_string(),
        status: if manifest.version > *current {
            "update-available"
        } else {
            "up-to-date"
        },
        target: CURRENT_TARGET,
    }
}

// ------------------------------------------------------------ installation

#[derive(Debug, PartialEq, Eq)]
pub enum Install {
    /// Placed by the official installer in the per-user directory: safe to replace.
    Official,
    /// Owned by a package manager: leave it alone and print its command.
    Managed {
        manager: &'static str,
        command: &'static str,
    },
    /// Anywhere else (a build tree, `/usr/local/bin`, ...): not ours to overwrite.
    Unmanaged,
}

pub fn official_dir(home: Option<&Path>, local_app_data: Option<&Path>) -> Option<PathBuf> {
    if cfg!(windows) {
        local_app_data.map(|p| p.join("Speq").join("bin"))
    } else {
        home.map(|h| h.join(".local").join("bin"))
    }
}

pub fn detect_install(exe: &Path, official: Option<&Path>) -> Install {
    let path = exe.to_string_lossy().replace('\\', "/").to_lowercase();
    let managed = [
        ("/cellar/", "Homebrew", "brew upgrade speq"),
        ("/homebrew/", "Homebrew", "brew upgrade speq"),
        ("/linuxbrew/", "Homebrew", "brew upgrade speq"),
        ("/scoop/", "Scoop", "scoop update speq"),
        ("/winget/", "WinGet", "winget upgrade Dayatech.Speq"),
        (
            "/microsoft/windowsapps/",
            "WinGet",
            "winget upgrade Dayatech.Speq",
        ),
        ("/nix/store/", "Nix", "nix profile upgrade speq"),
    ];
    if let Some((_, manager, command)) = managed.iter().find(|(marker, _, _)| path.contains(marker))
    {
        return Install::Managed { manager, command };
    }
    match (exe.parent(), official) {
        (Some(parent), Some(dir)) if parent == dir => Install::Official,
        _ => Install::Unmanaged,
    }
}

/// Download, verify (size and SHA-256), smoke-test, then atomically replace `exe`.
/// The previous binary stays usable if any step fails.
pub async fn install_binary(
    http: &reqwest::Client,
    asset: &Asset,
    exe: &Path,
    expected: &Version,
) -> Result<()> {
    let url = Url::parse(&asset.url).map_err(|_| Error::Other("invalid download URL".into()))?;
    let bytes = get_limited(http, &url, asset.size).await?;
    if bytes.len() as u64 != asset.size || !sha256_hex(&bytes).eq_ignore_ascii_case(&asset.sha256) {
        return Err(Error::Other(
            "the downloaded binary failed checksum verification; nothing was changed".into(),
        ));
    }
    let dir = exe
        .parent()
        .ok_or_else(|| Error::Other("the executable has no parent directory".into()))?;
    let mut tmp = tempfile::Builder::new()
        .prefix(".speq-upgrade-")
        .tempfile_in(dir)
        .ctx(format!("create a temporary file in {}", dir.display()))?;
    tmp.write_all(&bytes).ctx("write the new binary")?;
    tmp.as_file().sync_all().ctx("sync the new binary")?;
    make_executable(&tmp)?;
    // Close the write handle before running the file: Linux refuses to execute a file that is still open
    // for writing ("Text file busy"). The path stays reserved, and is removed again unless we persist it.
    let staged = tmp.into_temp_path();
    smoke_test(&staged, expected)?;
    replace(staged, exe)
}

#[cfg(unix)]
fn make_executable(tmp: &NamedTempFile) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;
    tmp.as_file()
        .set_permissions(std::fs::Permissions::from_mode(0o755))
        .ctx("make the new binary executable")
}

#[cfg(not(unix))]
fn make_executable(_: &NamedTempFile) -> Result<()> {
    Ok(())
}

fn smoke_test(path: &Path, expected: &Version) -> Result<()> {
    // The file is closed, but another thread that forked while it was open can still make the first
    // exec report `ExecutableFileBusy` for a moment: retry briefly instead of failing a valid upgrade.
    let mut attempts = 0;
    let output = loop {
        match std::process::Command::new(path).arg("--version").output() {
            Err(e) if e.kind() == std::io::ErrorKind::ExecutableFileBusy && attempts < 10 => {
                attempts += 1;
                std::thread::sleep(Duration::from_millis(50));
            }
            Err(e) => return Err(Error::Other(format!("the new binary does not run: {e}"))),
            Ok(output) => break output,
        }
    };
    let text = String::from_utf8_lossy(&output.stdout);
    if !output.status.success() || !text.contains(&expected.to_string()) {
        return Err(Error::Other(
            "the new binary did not report the expected version; nothing was changed".into(),
        ));
    }
    Ok(())
}

#[cfg(not(windows))]
fn replace(staged: tempfile::TempPath, exe: &Path) -> Result<()> {
    staged.persist(exe).map_err(|e| Error::Io {
        context: format!("replace {}", exe.display()),
        source: e.error,
    })
}

/// A running `.exe` cannot be overwritten on Windows, but it can be renamed away.
#[cfg(windows)]
fn replace(staged: tempfile::TempPath, exe: &Path) -> Result<()> {
    let old = exe.with_extension("exe.old");
    let _ = std::fs::remove_file(&old);
    std::fs::rename(exe, &old).ctx(format!("move the current binary aside ({})", exe.display()))?;
    match staged.persist(exe) {
        Ok(()) => Ok(()),
        Err(e) => {
            let _ = std::fs::rename(&old, exe);
            Err(Error::Io {
                context: format!("install {}", exe.display()),
                source: e.error,
            })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_package_managers_and_official_installs() {
        let official = Path::new("/home/a/.local/bin");
        assert_eq!(
            detect_install(Path::new("/home/a/.local/bin/speq"), Some(official)),
            Install::Official
        );
        assert!(matches!(
            detect_install(
                Path::new("/opt/homebrew/Cellar/speq/1.0.0/bin/speq"),
                Some(official)
            ),
            Install::Managed {
                manager: "Homebrew",
                ..
            }
        ));
        assert!(matches!(
            detect_install(
                Path::new("C:\\Users\\a\\scoop\\apps\\speq\\current\\speq.exe"),
                None
            ),
            Install::Managed {
                manager: "Scoop",
                ..
            }
        ));
        assert_eq!(
            detect_install(
                Path::new("/work/speq-cli/target/debug/speq"),
                Some(official)
            ),
            Install::Unmanaged
        );
    }

    #[test]
    fn release_urls() {
        let v = Version::parse("1.4.0").unwrap();
        assert_eq!(
            manifest_url("https://h/releases/", None).unwrap().as_str(),
            "https://h/releases/latest/download/manifest.json"
        );
        assert_eq!(
            manifest_url("https://h/releases", Some(&v))
                .unwrap()
                .as_str(),
            "https://h/releases/download/v1.4.0/manifest.json"
        );
        assert!(manifest_url("http://example.com/releases", None).is_err());
    }
}
