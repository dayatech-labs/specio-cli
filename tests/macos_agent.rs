//! Real launchd + Keychain check for the background agent (07.07). Installs a LaunchAgent for the
//! current user, so it is ignored by default: `cargo test --test macos_agent -- --ignored --nocapture`.
#![cfg(target_os = "macos")]
mod support;

use std::path::PathBuf;
use std::process::{Command, Output};
use std::time::{Duration, Instant};
use support::FakeApi;

const LABEL: &str = "com.dayatech.speq.agent";

fn speq(api: &str, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_speq"))
        .arg("--api-url")
        .arg(api)
        .args(args)
        .output()
        .expect("run speq")
}

fn uid() -> String {
    String::from_utf8(Command::new("id").arg("-u").output().unwrap().stdout)
        .unwrap()
        .trim()
        .to_string()
}

fn launchctl_print() -> Option<String> {
    let out = Command::new("launchctl")
        .args(["print", &format!("gui/{}/{LABEL}", uid())])
        .output()
        .unwrap();
    out.status
        .success()
        .then(|| String::from_utf8_lossy(&out.stdout).into_owned())
}

fn plist() -> PathBuf {
    PathBuf::from(std::env::var("HOME").unwrap())
        .join(format!("Library/LaunchAgents/{LABEL}.plist"))
}

fn wait_for(what: &str, limit: Duration, mut done: impl FnMut() -> bool) {
    let start = Instant::now();
    while !done() {
        assert!(start.elapsed() < limit, "timed out waiting for {what}");
        std::thread::sleep(Duration::from_millis(250));
    }
}

/// Always remove the LaunchAgent and the Keychain entry, even when an assertion fails.
struct Cleanup<'a>(&'a str);
impl Drop for Cleanup<'_> {
    fn drop(&mut self) {
        let _ = speq(self.0, &["logout"]);
    }
}

#[test]
#[ignore = "installs a real LaunchAgent and uses the real Keychain"]
fn launchd_agent_installs_refreshes_and_is_removed_on_logout() {
    // Never touch an agent that belongs to a real login.
    assert!(
        !plist().exists(),
        "{} already exists; log out of Speq first",
        plist().display()
    );

    let api = FakeApi::start();
    api.with(|s| s.snapshot_ttl = 60); // inside the agent's refresh-ahead window, so the first tick must refresh
    let _cleanup = Cleanup(&api.url);

    let login = speq(&api.url, &["login", "--no-browser"]);
    assert!(
        login.status.success(),
        "login failed: {}",
        String::from_utf8_lossy(&login.stderr)
    );

    // Installed: plist on disk pointing at this binary and API, and loaded into launchd.
    let text = std::fs::read_to_string(plist()).expect("plist written");
    assert!(
        text.contains(env!("CARGO_BIN_EXE_speq"))
            && text.contains(&api.url)
            && text.contains("<string>agent</string>")
    );
    let printed = launchctl_print().expect("LaunchAgent is loaded in launchd");
    assert!(printed.contains("run interval = 120 seconds"), "{printed}");

    // The real Keychain holds the credential: a later process can use it.
    let list = speq(&api.url, &["list", "--json"]);
    assert!(
        list.status.success(),
        "{}",
        String::from_utf8_lossy(&list.stderr)
    );
    let calls_before = api.count("GET /v1/projects");

    // Run the job the way launchd would. The snapshot is near expiry, so the agent refreshes it.
    let kick = Command::new("launchctl")
        .args(["kickstart", "-k", &format!("gui/{}/{LABEL}", uid())])
        .output()
        .unwrap();
    assert!(
        kick.status.success(),
        "{}",
        String::from_utf8_lossy(&kick.stderr)
    );
    wait_for("the agent to refresh", Duration::from_secs(40), || {
        api.count("GET /v1/projects") > calls_before
    });
    wait_for("the agent to exit cleanly", Duration::from_secs(15), || {
        launchctl_print().is_some_and(|p| p.contains("last exit code = 0"))
    });

    // Logout removes the credential, the cache, and the LaunchAgent.
    let logout = speq(&api.url, &["logout"]);
    assert!(
        logout.status.success(),
        "{}",
        String::from_utf8_lossy(&logout.stderr)
    );
    assert!(!plist().exists());
    wait_for(
        "launchd to unload the agent",
        Duration::from_secs(10),
        || launchctl_print().is_none(),
    );
    assert_eq!(speq(&api.url, &["list"]).status.code(), Some(3));
}
