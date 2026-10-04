mod support;

use specio::credentials::CredentialStore;
use specio::error::Error;
use specio::session::{AgentOutcome, Session};
use specio::snapshot;
use std::sync::atomic::Ordering;
use support::{Device, FakeApi, REPO};

#[tokio::test]
async fn login_stores_refresh_credential_snapshot_and_agent_but_no_access_token() {
    let api = FakeApi::start();
    let device = Device::new(&api);
    let report = device.login().await;
    assert_eq!(report.user_id, "user_a");
    assert_eq!(report.projects, 3);
    assert!(report.agent_installed);
    assert_eq!(device.scheduler.installs.load(Ordering::SeqCst), 1);

    let credential = device
        .creds
        .load(&device.env.account())
        .unwrap()
        .expect("credential in the store");
    assert_eq!(credential.user_id.as_deref(), Some("user_a"));
    // The snapshot is on disk; no access token and no refresh credential appear anywhere on disk.
    let snapshot = snapshot::load(&device.env.snapshot_path())
        .unwrap()
        .expect("snapshot");
    assert!(snapshot.etag.is_some());
    let mut on_disk = String::new();
    for entry in walk(device.data.path()) {
        on_disk.push_str(&std::fs::read_to_string(&entry).unwrap_or_default());
    }
    assert!(!on_disk.contains(&credential.refresh_credential));
    assert!(
        !on_disk.contains("at_"),
        "access tokens must stay in memory"
    );
}

fn walk(dir: &std::path::Path) -> Vec<std::path::PathBuf> {
    let mut out = vec![];
    for entry in std::fs::read_dir(dir).unwrap().flatten() {
        let path = entry.path();
        if path.is_dir() {
            out.extend(walk(&path))
        } else {
            out.push(path)
        }
    }
    out
}

#[tokio::test]
async fn two_devices_have_independent_sessions_and_logout_revokes_only_one() {
    let api = FakeApi::start();
    let (laptop, desktop) = (Device::new(&api), Device::new(&api));
    laptop.login().await;
    desktop.login().await;

    let report = Session::new(&laptop.env).logout().await.unwrap();
    assert!(report.was_logged_in);
    assert!(laptop.creds.load(&laptop.env.account()).unwrap().is_none());
    assert!(
        snapshot::load(&laptop.env.snapshot_path())
            .unwrap()
            .is_none()
    );
    assert_eq!(laptop.scheduler.uninstalls.load(Ordering::SeqCst), 1);
    assert!(matches!(
        Session::new(&laptop.env).authenticate().await,
        Err(Error::NotLoggedIn)
    ));

    // The other device still works and rotates its credential normally.
    Session::new(&desktop.env)
        .authenticate()
        .await
        .expect("desktop session unaffected");
}

#[tokio::test]
async fn list_uses_the_valid_snapshot_without_calling_the_api() {
    let api = FakeApi::start();
    let device = Device::new(&api);
    device.login().await;
    let before = api.requests();
    let list = specio::init::list(&device.env).await.unwrap();
    assert_eq!(list.projects.len(), 3);
    assert_eq!(
        api.requests(),
        before,
        "a valid snapshot must not touch the network"
    );
}

#[tokio::test]
async fn an_expired_snapshot_is_never_used_and_refreshes_with_if_none_match() {
    let api = FakeApi::start();
    api.with(|s| s.snapshot_ttl = -5); // every window the API hands out is already over
    let device = Device::new(&api);
    device.login().await;
    let first_etag = snapshot::load(&device.env.snapshot_path())
        .unwrap()
        .unwrap()
        .etag;

    // The cache is expired, so `list` must refresh in the foreground instead of trusting it.
    api.with(|s| s.snapshot_ttl = 600);
    let before = api.requests();
    let list = specio::init::list(&device.env).await.unwrap();
    assert!(api.requests() > before);
    // The ETag still matched, so the new window came from a 304 and the content is unchanged.
    let after = snapshot::load(&device.env.snapshot_path())
        .unwrap()
        .unwrap();
    assert_eq!(after.etag, first_etag);
    assert!(after.is_valid(snapshot::now()));
    assert_eq!(list.projects.len(), 3);
}

#[tokio::test]
async fn a_304_without_valid_window_headers_triggers_a_full_fetch() {
    let api = FakeApi::start();
    api.with(|s| s.snapshot_ttl = -5);
    let device = Device::new(&api);
    device.login().await;
    api.with(|s| {
        s.snapshot_ttl = 600;
        s.send_304_headers = false;
    });
    let before = api.count("GET /v1/projects");
    specio::init::list(&device.env).await.unwrap();
    assert_eq!(
        api.count("GET /v1/projects") - before,
        2,
        "304 without headers must be followed by a full GET"
    );
    assert!(
        snapshot::load(&device.env.snapshot_path())
            .unwrap()
            .unwrap()
            .is_valid(snapshot::now())
    );
}

#[tokio::test]
async fn a_new_window_longer_than_fifteen_minutes_is_clamped() {
    let api = FakeApi::start();
    api.with(|s| s.snapshot_ttl = 24 * 3600);
    let device = Device::new(&api);
    device.login().await;
    let snapshot = snapshot::load(&device.env.snapshot_path())
        .unwrap()
        .unwrap();
    assert!(snapshot.expires_at - snapshot::now() <= snapshot::MAX_WINDOW);
}

#[tokio::test]
async fn role_change_refreshes_the_snapshot_after_a_403() {
    let f = support::Fixture::new().await;
    f.api.with(|s| s.forbid_writes = true);
    f.write("epic-payment/features/create-payment/cli.md", "# CLI\n");
    f.pull().await.unwrap();
    f.write("epic-payment/features/create-payment/cli.md", "# CLI\n");
    let before = f.api.count("GET /v1/projects");
    let err = specio::sync::push::push(&f.device.env, &f.ws())
        .await
        .unwrap_err();
    assert_eq!(err.exit_code(), specio::error::exit::DENIED);
    // The snapshot was dropped and refreshed once; the push itself was sent exactly once.
    assert_eq!(f.api.count("GET /v1/projects") - before, 1);
    assert_eq!(f.api.count("POST /v1/projects/proj_payment/changes"), 1);
}

#[tokio::test]
async fn concurrent_processes_never_present_the_same_refresh_credential() {
    let api = FakeApi::start();
    let device = Device::new(&api);
    device.login().await;
    let handles: Vec<_> = (0..6)
        .map(|_| {
            let env = device.another_process();
            std::thread::spawn(move || {
                let rt = tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                    .unwrap();
                rt.block_on(async { Session::new(&env).authenticate().await.map(|_| ()) })
            })
        })
        .collect();
    for h in handles {
        h.join().unwrap().expect("authenticate under contention");
    }
    assert_eq!(
        api.with(|s| s.reuse_detected),
        0,
        "rotation must be serialised across processes"
    );
    assert_eq!(api.with(|s| s.refresh_calls), 6);
}

#[tokio::test]
async fn reusing_a_rotated_credential_revokes_the_device_and_clears_local_state() {
    let api = FakeApi::start();
    let device = Device::new(&api);
    device.login().await;
    let stale = device.creds.load(&device.env.account()).unwrap().unwrap();
    Session::new(&device.env).authenticate().await.unwrap(); // rotates
    device.creds.save(&device.env.account(), &stale).unwrap(); // an old copy comes back

    let err = Session::new(&device.env).authenticate().await.unwrap_err();
    assert!(matches!(err, Error::SessionExpired));
    assert_eq!(err.exit_code(), specio::error::exit::AUTH);
    assert!(device.creds.load(&device.env.account()).unwrap().is_none());
    assert!(
        snapshot::load(&device.env.snapshot_path())
            .unwrap()
            .is_none()
    );
    assert!(device.scheduler.uninstalls.load(Ordering::SeqCst) >= 1);
}

#[tokio::test]
async fn a_401_clears_authentication_state_and_stops_the_agent() {
    let api = FakeApi::start();
    let device = Device::new(&api);
    device.login().await;
    api.revoke_all_sessions();
    let err = Session::new(&device.env).authenticate().await.unwrap_err();
    assert!(matches!(err, Error::SessionExpired));
    assert!(device.creds.load(&device.env.account()).unwrap().is_none());
    assert!(
        snapshot::load(&device.env.snapshot_path())
            .unwrap()
            .is_none()
    );
    assert!(device.scheduler.uninstalls.load(Ordering::SeqCst) >= 1);
    assert!(matches!(
        Session::new(&device.env).agent_tick(false).await,
        Ok(AgentOutcome::LoggedOut)
    ));
}

#[tokio::test]
async fn switching_accounts_revokes_the_old_session_and_drops_its_snapshot() {
    let api = FakeApi::start();
    let device = Device::new(&api);
    device.login().await;
    let old = device.creds.load(&device.env.account()).unwrap().unwrap();

    api.with(|s| s.next_login_user = "user_b".into());
    let report = device.login().await;
    assert_eq!(report.user_id, "user_b");
    let snapshot = snapshot::load(&device.env.snapshot_path())
        .unwrap()
        .unwrap();
    assert_eq!(snapshot.user_id, "user_b");
    assert_eq!(
        device
            .creds
            .load(&device.env.account())
            .unwrap()
            .unwrap()
            .user_id
            .as_deref(),
        Some("user_b")
    );

    // The previous device session no longer works on the server.
    let client = &device.env.client;
    assert_eq!(
        client
            .refresh(&old.refresh_credential)
            .await
            .unwrap_err()
            .api_status(),
        Some(401)
    );
}

#[tokio::test]
async fn the_agent_is_a_no_op_until_expiry_and_then_refreshes_with_etag() {
    let api = FakeApi::start();
    let device = Device::new(&api);
    device.login().await;
    let session = Session::new(&device.env);
    let calls = api.with(|s| s.refresh_calls);
    assert_eq!(
        session.agent_tick(false).await.unwrap(),
        AgentOutcome::StillFresh
    );
    assert_eq!(api.with(|s| s.refresh_calls), calls);

    // Close to expiry: the agent refreshes without any user command.
    let mut near = snapshot::load(&device.env.snapshot_path())
        .unwrap()
        .unwrap();
    near.expires_at = snapshot::now() + time::Duration::seconds(30);
    snapshot::save(&device.env.snapshot_path(), &near).unwrap();
    assert_eq!(
        session.agent_tick(false).await.unwrap(),
        AgentOutcome::Refreshed
    );
    let renewed = snapshot::load(&device.env.snapshot_path())
        .unwrap()
        .unwrap();
    assert!(renewed.expires_at - snapshot::now() > time::Duration::minutes(5));
}

#[tokio::test]
async fn init_lists_only_repositories_from_the_api_and_requires_unique_short_names() {
    let f = support::Fixture::new().await;
    let dir = tempfile::tempdir().unwrap();
    let root = std::fs::canonicalize(dir.path()).unwrap();
    let init = |repo: &'static str, reconfigure: bool| {
        let root = root.clone();
        let env = &f.device.env;
        async move {
            specio::init::init(
                env,
                &root,
                specio::init::InitOptions {
                    repo,
                    repository_type: "frontend",
                    local_dir: "specs",
                    reconfigure,
                },
            )
            .await
        }
    };
    assert!(
        init("payment-specs", false)
            .await
            .unwrap_err()
            .to_string()
            .contains("ambiguous")
    );
    assert!(init("nope/none", false).await.is_err());
    let report = init("cart-specs", false).await.unwrap();
    assert_eq!(report.repository, "acme/cart-specs");

    let gitignore = std::fs::read_to_string(root.join(".gitignore")).unwrap();
    for line in [
        "specs/",
        ".specio/lock.json",
        ".specio/manifest.json",
        ".specio/base/",
    ] {
        assert!(gitignore.lines().any(|l| l == line), "{line}");
    }
    assert!(root.join(".specio/config.toml").is_file());
    assert!(root.join("specs").is_dir());

    // Re-init needs an explicit reconfigure.
    assert!(
        init(REPO, false)
            .await
            .unwrap_err()
            .to_string()
            .contains("--reconfigure")
    );
    let again = init(REPO, true).await.unwrap();
    assert!(again.reconfigured && again.sync_state_reset);
}
