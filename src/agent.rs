//! Background refresh agent: a per-user job that runs `speq agent run` every two minutes.
//! macOS uses a launchd LaunchAgent, Linux a systemd user timer, Windows a per-user scheduled task.
//! Each tick is a no-op until the capability snapshot is close to expiry.
use crate::error::{Error, IoContext, Result};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

pub const INTERVAL_SECONDS: u32 = 120;
const LABEL: &str = "com.dayatech.speq.agent";
const SYSTEMD_UNIT: &str = "speq-agent";
const WINDOWS_TASK: &str = "Speq Agent";

pub trait Scheduler: Send + Sync {
    fn install(&self) -> Result<()>;
    fn uninstall(&self) -> Result<()>;
}

/// Installs nothing; used where no scheduler exists and in tests.
pub struct NoScheduler;

impl Scheduler for NoScheduler {
    fn install(&self) -> Result<()> {
        Ok(())
    }
    fn uninstall(&self) -> Result<()> {
        Ok(())
    }
}

/// The OS scheduler of the current platform.
pub struct SystemScheduler {
    api_url: String,
}

impl SystemScheduler {
    pub fn new(api_url: &url::Url) -> Self {
        SystemScheduler {
            api_url: api_url.as_str().trim_end_matches('/').to_string(),
        }
    }
}

fn run(program: &str, args: &[&str]) -> Result<()> {
    let status = Command::new(program)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map_err(|e| Error::Other(format!("cannot run {program}: {e}")))?;
    if status.success() {
        Ok(())
    } else {
        Err(Error::Other(format!(
            "{program} {} failed ({status})",
            args.first().copied().unwrap_or("")
        )))
    }
}

fn home() -> Result<PathBuf> {
    directories::BaseDirs::new()
        .map(|d| d.home_dir().to_path_buf())
        .ok_or_else(|| Error::Other("cannot determine the home directory".into()))
}

fn current_exe() -> Result<PathBuf> {
    std::env::current_exe().ctx("locate the speq executable")
}

// ------------------------------------------------------------- unit files

fn xml_escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

pub fn launchd_plist(exe: &Path, api_url: &str) -> String {
    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>Label</key><string>{LABEL}</string>
  <key>ProgramArguments</key>
  <array>
    <string>{exe}</string>
    <string>--api-url</string>
    <string>{api}</string>
    <string>agent</string>
    <string>run</string>
  </array>
  <key>StartInterval</key><integer>{INTERVAL_SECONDS}</integer>
  <key>RunAtLoad</key><false/>
  <key>ProcessType</key><string>Background</string>
</dict>
</plist>
"#,
        exe = xml_escape(&exe.to_string_lossy()),
        api = xml_escape(api_url),
    )
}

/// Quote one argument for a systemd `ExecStart=` line.
fn systemd_quote(arg: &str) -> String {
    let escaped: String = arg
        .chars()
        .flat_map(|c| match c {
            '"' => vec!['\\', '"'],
            '\\' => vec!['\\', '\\'],
            '%' => vec!['%', '%'],
            '$' => vec!['$', '$'],
            c => vec![c],
        })
        .collect();
    format!("\"{escaped}\"")
}

pub fn systemd_service(exe: &Path, api_url: &str) -> String {
    format!(
        "[Unit]\nDescription=Speq capability snapshot refresh\n\n[Service]\nType=oneshot\nExecStart={} --api-url {} agent run\n",
        systemd_quote(&exe.to_string_lossy()),
        systemd_quote(api_url),
    )
}

pub fn systemd_timer() -> String {
    format!(
        "[Unit]\nDescription=Speq capability snapshot refresh timer\n\n[Timer]\nOnBootSec=60\nOnUnitActiveSec={INTERVAL_SECONDS}\nRandomizedDelaySec=10\nAccuracySec=1s\n\n[Install]\nWantedBy=timers.target\n"
    )
}

pub fn schtasks_create_args(exe: &Path, api_url: &str) -> Vec<String> {
    let minutes = (INTERVAL_SECONDS / 60).max(1).to_string();
    let command = format!("\"{}\" --api-url \"{}\" agent run", exe.display(), api_url);
    [
        "/Create",
        "/TN",
        WINDOWS_TASK,
        "/SC",
        "MINUTE",
        "/MO",
        &minutes,
        "/TR",
        &command,
        "/RL",
        "LIMITED",
        "/F",
    ]
    .iter()
    .map(|s| s.to_string())
    .collect()
}

// ----------------------------------------------------------- per platform

impl Scheduler for SystemScheduler {
    fn install(&self) -> Result<()> {
        let exe = current_exe()?;
        if cfg!(target_os = "macos") {
            let dir = home()?.join("Library/LaunchAgents");
            let plist = dir.join(format!("{LABEL}.plist"));
            crate::fsx::atomic_write(
                &plist,
                launchd_plist(&exe, &self.api_url).as_bytes(),
                crate::fsx::Visibility::Shared,
            )?;
            let uid = Command::new("id").arg("-u").output().ctx("run id -u")?;
            let domain = format!("gui/{}", String::from_utf8_lossy(&uid.stdout).trim());
            let _ = run("launchctl", &["bootout", &format!("{domain}/{LABEL}")]);
            run(
                "launchctl",
                &["bootstrap", &domain, &plist.to_string_lossy()],
            )
        } else if cfg!(windows) {
            let args = schtasks_create_args(&exe, &self.api_url);
            let refs: Vec<&str> = args.iter().map(String::as_str).collect();
            run("schtasks", &refs)
        } else {
            let dir = directories::BaseDirs::new()
                .map(|d| d.config_dir().join("systemd/user"))
                .ok_or_else(|| {
                    Error::Other("cannot determine the systemd user directory".into())
                })?;
            crate::fsx::atomic_write(
                &dir.join(format!("{SYSTEMD_UNIT}.service")),
                systemd_service(&exe, &self.api_url).as_bytes(),
                crate::fsx::Visibility::Shared,
            )?;
            crate::fsx::atomic_write(
                &dir.join(format!("{SYSTEMD_UNIT}.timer")),
                systemd_timer().as_bytes(),
                crate::fsx::Visibility::Shared,
            )?;
            run("systemctl", &["--user", "daemon-reload"])?;
            run(
                "systemctl",
                &[
                    "--user",
                    "enable",
                    "--now",
                    &format!("{SYSTEMD_UNIT}.timer"),
                ],
            )
        }
    }

    fn uninstall(&self) -> Result<()> {
        if cfg!(target_os = "macos") {
            let plist = home()?.join(format!("Library/LaunchAgents/{LABEL}.plist"));
            if !plist.exists() {
                return Ok(());
            }
            if let Ok(uid) = Command::new("id").arg("-u").output() {
                let domain = format!("gui/{}", String::from_utf8_lossy(&uid.stdout).trim());
                let _ = run("launchctl", &["bootout", &format!("{domain}/{LABEL}")]);
            }
            crate::fsx::remove_optional(&plist)
        } else if cfg!(windows) {
            // Deleting a task that does not exist is not an error worth reporting.
            let _ = run("schtasks", &["/Delete", "/TN", WINDOWS_TASK, "/F"]);
            Ok(())
        } else {
            let dir = directories::BaseDirs::new().map(|d| d.config_dir().join("systemd/user"));
            let Some(dir) = dir else { return Ok(()) };
            let timer = dir.join(format!("{SYSTEMD_UNIT}.timer"));
            let service = dir.join(format!("{SYSTEMD_UNIT}.service"));
            if !timer.exists() && !service.exists() {
                return Ok(());
            }
            let _ = run(
                "systemctl",
                &[
                    "--user",
                    "disable",
                    "--now",
                    &format!("{SYSTEMD_UNIT}.timer"),
                ],
            );
            crate::fsx::remove_optional(&timer)?;
            crate::fsx::remove_optional(&service)?;
            let _ = run("systemctl", &["--user", "daemon-reload"]);
            Ok(())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plist_escapes_and_lists_arguments() {
        let plist = launchd_plist(Path::new("/Users/a&b/bin/speq"), "https://api.example");
        assert!(plist.contains("<string>/Users/a&amp;b/bin/speq</string>"));
        assert!(plist.contains("<string>agent</string>") && plist.contains("<string>run</string>"));
        assert!(plist.contains(&format!("<integer>{INTERVAL_SECONDS}</integer>")));
    }

    #[test]
    fn systemd_quoting_prevents_expansion() {
        let unit = systemd_service(Path::new("/home/a b/speq"), "https://api.example");
        assert!(
            unit.contains(
                "ExecStart=\"/home/a b/speq\" --api-url \"https://api.example\" agent run"
            )
        );
        assert_eq!(systemd_quote("100%$x\"y"), "\"100%%$$x\\\"y\"");
        assert!(systemd_timer().contains("RandomizedDelaySec=10"));
    }

    #[test]
    fn schtasks_runs_every_two_minutes_for_the_current_user() {
        let args = schtasks_create_args(Path::new("C:\\Users\\a\\speq.exe"), "https://api.example");
        assert!(args.windows(2).any(|w| w == ["/MO", "2"]));
        assert!(args.iter().any(|a| a.contains("agent run")));
        assert!(!args.contains(&"/RU".to_string()));
    }
}
