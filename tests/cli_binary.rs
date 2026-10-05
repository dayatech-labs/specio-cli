//! The real binary: help, version, usage errors. None of these touch credentials or the network.
use std::process::Command;

fn speq(args: &[&str]) -> (i32, String, String) {
    let out = Command::new(env!("CARGO_BIN_EXE_speq"))
        .args(args)
        .output()
        .expect("run speq");
    (
        out.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

const COMMANDS: [&str; 12] = [
    "login",
    "logout",
    "list",
    "init",
    "pull",
    "status",
    "diff",
    "update",
    "push",
    "context",
    "upgrade",
    "uninstall",
];

#[test]
fn version_reports_semver_commit_and_target() {
    let (code, out, _) = speq(&["--version"]);
    assert_eq!(code, 0);
    assert!(out.starts_with(&format!("speq {}", env!("CARGO_PKG_VERSION"))));
    assert!(out.contains("commit: ") && out.contains("target: "));
}

#[test]
fn help_lists_every_command_with_a_summary_and_exit_codes() {
    let (code, out, _) = speq(&["help"]);
    assert_eq!(code, 0);
    for c in COMMANDS {
        assert!(out.contains(&format!("  {c} ")), "missing {c} in help");
    }
    assert!(
        !out.contains("\n  agent "),
        "the internal agent command is hidden"
    );
    assert!(out.contains("Exit codes:"));
}

#[test]
fn every_command_help_has_usage_arguments_flags_examples_and_exit_codes() {
    for c in COMMANDS {
        let (code, out, _) = speq(&[c, "--help"]);
        assert_eq!(code, 0, "{c}");
        assert!(out.contains("Usage: speq"), "{c}");
        assert!(out.contains("Examples:"), "{c}");
        assert!(out.contains("Exit codes:"), "{c}");
        assert!(out.contains("--json"), "{c}");
        // `speq help <command>` is equivalent.
        assert_eq!(speq(&["help", c]).1, out, "{c}");
    }
    let (_, init, _) = speq(&["init", "--help"]);
    assert!(
        init.contains("--type") && init.contains("--local-dir") && init.contains("--reconfigure")
    );
    let (_, upgrade, _) = speq(&["upgrade", "--help"]);
    assert!(
        upgrade.contains("--check") && upgrade.contains("--yes") && upgrade.contains("--version")
    );
    let (_, diff, _) = speq(&["diff", "--help"]);
    assert!(diff.contains("--base"));
}

#[test]
fn usage_errors_exit_2_and_force_flags_do_not_exist() {
    assert_eq!(speq(&["pull", "--feature", "x"]).0, 2);
    assert_eq!(speq(&["update", "--force", "a.md"]).0, 2);
    assert_eq!(speq(&["nope"]).0, 2);
    assert_eq!(speq(&["diff"]).0, 2);
}

#[test]
fn commands_outside_a_workspace_exit_7_without_touching_the_network() {
    let dir = tempfile::tempdir().expect("tempdir");
    let out = Command::new(env!("CARGO_BIN_EXE_speq"))
        .args(["status"])
        .current_dir(dir.path())
        .output()
        .expect("run");
    assert_eq!(out.status.code(), Some(7));
    assert!(String::from_utf8_lossy(&out.stderr).contains("speq init"));
}

#[test]
fn context_outside_a_workspace_reports_json_errors_when_asked() {
    let dir = tempfile::tempdir().expect("tempdir");
    let out = Command::new(env!("CARGO_BIN_EXE_speq"))
        .args(["--json", "context", "a/b"])
        .current_dir(dir.path())
        .output()
        .expect("run");
    assert_eq!(out.status.code(), Some(7));
    let body: serde_json::Value = serde_json::from_slice(&out.stderr).expect("json error");
    assert_eq!(body["error"]["code"], "not_initialized");
}

#[test]
fn upgrade_check_fails_non_zero_when_the_release_server_is_unreachable() {
    let out = Command::new(env!("CARGO_BIN_EXE_speq"))
        .args(["upgrade", "--check"])
        .env("SPEQ_RELEASE_URL", "http://127.0.0.1:1/releases")
        .output()
        .expect("run");
    assert_eq!(out.status.code(), Some(4));
}

#[test]
fn uninstall_without_a_terminal_asks_for_yes_and_changes_nothing() {
    // Refuses before touching anything: with no terminal there is nobody to confirm.
    let (code, _, err) = speq(&["uninstall"]);
    assert_eq!(code, 7);
    assert!(err.contains("--yes"), "{err}");
    assert!(std::path::Path::new(env!("CARGO_BIN_EXE_speq")).exists());
}
