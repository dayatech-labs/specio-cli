use semver::Version;
use specio::hashing::sha256_hex;
use specio::upgrade::{self, Asset, Install};
use std::collections::HashMap;

#[path = "support/http.rs"]
mod http;

/// Serves fixed bodies by path, like a release page.
struct Releases {
    base: String,
    _server: http::HttpServer,
}

impl Releases {
    fn start(routes: HashMap<String, Vec<u8>>) -> Releases {
        let server = http::serve(move |request| match routes.get(&request.url) {
            Some(body) => http::Reply {
                status: 200,
                headers: vec![],
                body: body.clone(),
            },
            None => http::Reply {
                status: 404,
                headers: vec![],
                body: b"missing".to_vec(),
            },
        });
        Releases {
            base: format!("http://{}", server.addr),
            _server: server,
        }
    }
}

fn manifest(base: &str, version: &str, binary: &[u8]) -> Vec<u8> {
    serde_json::json!({
        "version": version,
        "commit": "abc",
        "artifacts": { upgrade::CURRENT_TARGET: { "binary": {
            "url": format!("{base}/bin/specio-{version}"), "sha256": sha256_hex(binary), "size": binary.len() } } }
    })
    .to_string()
    .into_bytes()
}

#[tokio::test]
async fn check_reports_update_available_and_up_to_date() {
    let releases = Releases::start(HashMap::from([(
        "/latest/download/manifest.json".to_string(),
        manifest("http://127.0.0.1:1", "99.0.0", b"x"),
    )]));
    let http = upgrade::http_client().unwrap();
    let url = upgrade::manifest_url(&releases.base, None).unwrap();
    let found = upgrade::fetch_manifest(&http, &url).await.unwrap();
    let report = upgrade::check(&upgrade::current_version(), &found);
    assert_eq!(report.status, "update-available");
    assert_eq!(report.latest, "99.0.0");

    let same = upgrade::check(&Version::parse("99.0.0").unwrap(), &found);
    assert_eq!(same.status, "up-to-date");
}

#[tokio::test]
async fn network_failure_or_invalid_manifest_is_an_error_never_up_to_date() {
    let http = upgrade::http_client().unwrap();
    let down = upgrade::manifest_url("http://127.0.0.1:1", None).unwrap();
    assert_eq!(
        upgrade::fetch_manifest(&http, &down)
            .await
            .unwrap_err()
            .exit_code(),
        specio::error::exit::UNAVAILABLE
    );

    let releases = Releases::start(HashMap::from([
        ("/latest/download/manifest.json".to_string(), b"{ not json".to_vec()),
        ("/download/v1.0.0/manifest.json".to_string(), serde_json::json!({ "version": "1.0.0", "artifacts": { "t": { "binary": { "url": "http://evil.example/x", "sha256": "00", "size": 1 } } } }).to_string().into_bytes()),
    ]));
    let invalid = upgrade::manifest_url(&releases.base, None).unwrap();
    assert!(upgrade::fetch_manifest(&http, &invalid).await.is_err());
    let pinned =
        upgrade::manifest_url(&releases.base, Some(&Version::parse("1.0.0").unwrap())).unwrap();
    assert!(
        upgrade::fetch_manifest(&http, &pinned).await.is_err(),
        "non-https asset URLs are rejected"
    );
}

#[cfg(unix)]
#[tokio::test]
async fn install_verifies_then_replaces_atomically_and_keeps_the_old_binary_on_failure() {
    use std::os::unix::fs::PermissionsExt;
    let new_binary = b"#!/bin/sh\necho \"specio 9.9.9\"\n".to_vec();
    let releases = Releases::start(HashMap::from([
        ("/bin/good".to_string(), new_binary.clone()),
        (
            "/bin/tampered".to_string(),
            b"#!/bin/sh\necho evil\n".to_vec(),
        ),
        (
            "/bin/liar".to_string(),
            b"#!/bin/sh\necho \"specio 1.0.0\"\n".to_vec(),
        ),
    ]));
    let dir = tempfile::tempdir().unwrap();
    let exe = dir.path().join("specio");
    std::fs::write(&exe, "old").unwrap();
    let http = upgrade::http_client().unwrap();
    let version = Version::parse("9.9.9").unwrap();
    let asset = |path: &str, body: &[u8]| Asset {
        url: format!("{}{path}", releases.base),
        sha256: sha256_hex(body),
        size: body.len() as u64,
    };

    // Checksum mismatch: the bytes served differ from what the manifest promised.
    let mut bad = asset("/bin/tampered", &new_binary);
    bad.size = b"#!/bin/sh\necho evil\n".len() as u64;
    assert!(
        upgrade::install_binary(&http, &bad, &exe, &version)
            .await
            .is_err()
    );
    assert_eq!(std::fs::read_to_string(&exe).unwrap(), "old");

    // Right checksum but the binary does not report the expected version.
    let lying = asset("/bin/liar", b"#!/bin/sh\necho \"specio 1.0.0\"\n");
    assert!(
        upgrade::install_binary(&http, &lying, &exe, &version)
            .await
            .is_err()
    );
    assert_eq!(std::fs::read_to_string(&exe).unwrap(), "old");

    // Unreachable download.
    let gone = Asset {
        url: "http://127.0.0.1:1/x".into(),
        sha256: "0".repeat(64),
        size: 5,
    };
    assert!(
        upgrade::install_binary(&http, &gone, &exe, &version)
            .await
            .is_err()
    );
    assert_eq!(std::fs::read_to_string(&exe).unwrap(), "old");

    upgrade::install_binary(&http, &asset("/bin/good", &new_binary), &exe, &version)
        .await
        .unwrap();
    assert_eq!(std::fs::read(&exe).unwrap(), new_binary);
    assert_eq!(
        std::fs::metadata(&exe).unwrap().permissions().mode() & 0o111,
        0o111
    );
    // No temporary files are left behind.
    assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 1);
}

#[test]
fn package_manager_binaries_are_never_replaced() {
    let official = std::path::Path::new("/home/a/.local/bin");
    for path in [
        "/opt/homebrew/Cellar/specio/1.0.0/bin/specio",
        "/home/linuxbrew/.linuxbrew/bin/specio",
        "C:\\Users\\a\\scoop\\shims\\specio.exe",
    ] {
        assert!(
            matches!(
                upgrade::detect_install(std::path::Path::new(path), Some(official)),
                Install::Managed { .. }
            ),
            "{path}"
        );
    }
    assert_eq!(
        upgrade::detect_install(
            std::path::Path::new("/usr/local/bin/specio"),
            Some(official)
        ),
        Install::Unmanaged
    );
}
