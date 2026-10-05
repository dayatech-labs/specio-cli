mod support;

use speq::credentials::CredentialStore;
use speq::error::Error;
use speq::session::Session;
use speq::snapshot;
use speq::uninstall::uninstall;
use speq::upgrade::Install;
use std::path::PathBuf;
use std::sync::atomic::Ordering;
use support::{Device, FakeApi, Fixture};

/// A stand-in for the installed binary, in its own directory.
fn fake_binary() -> (tempfile::TempDir, PathBuf) {
    let dir = tempfile::tempdir().expect("tempdir");
    let exe = dir.path().join("speq");
    std::fs::write(&exe, b"binary").expect("write");
    (dir, exe)
}

const BREW: Install = Install::Managed {
    manager: "Homebrew",
    command: "brew upgrade speq",
    uninstall: "brew uninstall speq",
};

#[tokio::test]
async fn official_install_loses_its_state_and_its_binary_without_calling_the_api() {
    let api = FakeApi::start();
    let device = Device::new(&api);
    device.login().await;
    let (_dir, exe) = fake_binary();
    let logouts = api.count("POST /v1/auth/logout");

    let report = uninstall(&device.env, &exe, &Install::Official).unwrap();

    assert!(device.creds.load(&device.env.account()).unwrap().is_none());
    assert!(
        snapshot::load(&device.env.snapshot_path())
            .unwrap()
            .is_none()
    );
    assert_eq!(device.scheduler.uninstalls.load(Ordering::SeqCst), 1);
    assert!(
        !device.data.path().exists() || std::fs::read_dir(device.data.path()).unwrap().count() == 0,
        "the data directory is gone"
    );
    assert!(report.credential_removed);
    assert!(!exe.exists(), "the binary is removed");
    assert_eq!(report.removed_binary, Some(exe.display().to_string()));
    assert!(
        report.manual_steps.iter().any(|s| s.contains("PATH")),
        "the PATH edit made by the installer is pointed out: {:?}",
        report.manual_steps
    );
    assert_eq!(
        api.count("POST /v1/auth/logout"),
        logouts,
        "uninstall never ends the server-side session"
    );
    assert!(matches!(
        Session::new(&device.env).authenticate().await,
        Err(Error::NotLoggedIn)
    ));
}

#[tokio::test]
async fn a_package_managed_binary_is_kept_and_its_own_command_is_shown() {
    let api = FakeApi::start();
    let device = Device::new(&api);
    device.login().await;
    let (_dir, exe) = fake_binary();

    let report = uninstall(&device.env, &exe, &BREW).unwrap();

    assert!(exe.exists(), "the package manager owns the binary");
    assert_eq!(report.removed_binary, None);
    assert_eq!(report.manual_steps.len(), 1);
    assert!(report.manual_steps[0].contains("`brew uninstall speq`"));
    assert!(device.creds.load(&device.env.account()).unwrap().is_none());
    assert_eq!(device.scheduler.uninstalls.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn a_binary_the_installer_did_not_place_is_never_deleted() {
    let api = FakeApi::start();
    let device = Device::new(&api);
    device.login().await;
    let (_dir, exe) = fake_binary();

    let report = uninstall(&device.env, &exe, &Install::Unmanaged).unwrap();

    assert!(exe.exists());
    assert_eq!(report.removed_binary, None);
    assert!(report.manual_steps[0].contains(&exe.display().to_string()));
    assert!(device.creds.load(&device.env.account()).unwrap().is_none());
}

#[tokio::test]
async fn workspaces_are_left_untouched() {
    let fx = Fixture::new().await;
    fx.write("epics/a/prd.md", "my edit");
    let (_dir, exe) = fake_binary();

    uninstall(&fx.device.env, &exe, &Install::Official).unwrap();

    assert_eq!(fx.read("epics/a/prd.md").as_deref(), Some("my edit"));
    assert!(fx.workspace_dir.path().join(".speq/config.toml").exists());
}

#[tokio::test]
async fn uninstalling_when_not_logged_in_succeeds_and_can_be_repeated() {
    let api = FakeApi::start();
    let device = Device::new(&api);
    let (_dir, exe) = fake_binary();

    uninstall(&device.env, &exe, &BREW).unwrap();
    let again = uninstall(&device.env, &exe, &BREW).unwrap();

    assert_eq!(again.removed_binary, None);
    assert!(exe.exists());
}
