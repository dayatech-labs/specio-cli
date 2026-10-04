mod support;

use specio::context::context;
use specio::error::{Error, exit};
use specio::sync::{push, status};
use support::{Fixture, PROJECT};

const PRD: &str = "epic-payment/prd.md";
const README: &str = "epic-payment/features/create-payment/README.md";
const NEW_DOC: &str = "epic-payment/features/create-payment/frontend.md";

#[tokio::test]
async fn first_pull_downloads_everything_and_advances_the_lock() {
    let f = Fixture::new().await;
    let report = f.pull().await.unwrap();
    assert_eq!(report.added.len(), 6);
    assert!(report.lock_advanced && report.conflicts.is_empty());
    assert_eq!(f.read(PRD).as_deref(), Some("# PRD\n\nPayments.\n"));
    let lock = f.ws().load_lock().unwrap();
    assert_eq!(lock.head_sha.as_deref(), Some(f.api.head().as_str()));
    // The baseline copy and manifest entry exist for every applied path.
    assert_eq!(f.ws().load_manifest().unwrap().files.len(), 6);
    assert_eq!(
        f.ws().read_base(PRD).unwrap().unwrap(),
        b"# PRD\n\nPayments.\n"
    );

    // A second pull is a no-op.
    let again = f.pull().await.unwrap();
    assert!(again.added.is_empty() && again.updated.is_empty() && again.deleted.is_empty());
}

#[tokio::test]
async fn pull_applies_remote_changes_but_keeps_local_only_edits() {
    let f = Fixture::new().await;
    f.pull().await.unwrap();
    f.write(README, "# mine\n"); // local-only edit
    f.api.external_change(PRD, Some("# PRD v2\n"));
    f.api.external_change(
        "epic-payment/features/create-payment/cli.md",
        Some("# New\n"),
    );
    f.api.external_change("epic-payment/architecture.md", None);

    let report = f.pull().await.unwrap();
    assert_eq!(report.updated, vec![PRD.to_string()]);
    assert_eq!(
        report.added,
        vec!["epic-payment/features/create-payment/cli.md".to_string()]
    );
    assert_eq!(
        report.deleted,
        vec!["epic-payment/architecture.md".to_string()]
    );
    assert_eq!(f.read(README).as_deref(), Some("# mine\n"));
    assert_eq!(f.read(PRD).as_deref(), Some("# PRD v2\n"));
    assert!(f.read("epic-payment/architecture.md").is_none());
    assert!(report.lock_advanced);
}

#[tokio::test]
async fn conflicts_are_never_overwritten_and_block_push() {
    let f = Fixture::new().await;
    f.pull().await.unwrap();
    let before_lock = f.ws().load_lock().unwrap();

    f.write(PRD, "# local PRD\n"); // both changed
    f.api.external_change(PRD, Some("# remote PRD\n"));
    f.write("epic-payment/product-brief.md", "# local brief\n"); // remote delete over local edit
    f.api.external_change("epic-payment/product-brief.md", None);
    f.write(NEW_DOC, "# my frontend doc\n"); // remote add over untracked local file
    f.api
        .external_change(NEW_DOC, Some("# remote frontend doc\n"));

    let report = f.pull().await.unwrap();
    let mut conflicted: Vec<&str> = report.conflicts.iter().map(|c| c.path.as_str()).collect();
    conflicted.sort();
    assert_eq!(
        conflicted,
        vec![
            "epic-payment/features/create-payment/frontend.md",
            PRD,
            "epic-payment/product-brief.md"
        ]
    );
    assert!(!report.lock_advanced);
    assert_eq!(f.read(PRD).as_deref(), Some("# local PRD\n"));
    assert_eq!(
        f.read("epic-payment/product-brief.md").as_deref(),
        Some("# local brief\n")
    );
    assert_eq!(f.read(NEW_DOC).as_deref(), Some("# my frontend doc\n"));
    let lock = f.ws().load_lock().unwrap();
    assert_eq!(lock.head_sha, before_lock.head_sha);
    assert_eq!(lock.pending.len(), 3);

    // Push is refused while conflicts are pending, and no request is sent.
    let commits = f.api.commits();
    let err = push::push(&f.device.env, &f.ws()).await.unwrap_err();
    assert_eq!(err.exit_code(), exit::CONFLICT);
    assert_eq!(f.api.commits(), commits);
    // So is context, which needs a clean lock.
    assert!(context(&f.ws(), "epic-payment/create-payment").is_err());
}

#[tokio::test]
async fn an_identical_untracked_local_file_is_adopted_not_flagged() {
    let f = Fixture::new().await;
    f.write(PRD, "# PRD\n\nPayments.\n");
    let report = f.pull().await.unwrap();
    assert!(report.conflicts.is_empty());
    assert_eq!(report.adopted, vec![PRD.to_string()]);
    assert!(report.lock_advanced);
}

#[tokio::test]
async fn a_head_that_moves_during_pull_is_retried_from_a_fresh_manifest() {
    let f = Fixture::new().await;
    f.api.with(|s| s.move_head_after_sync = true);
    let report = f.pull().await.unwrap();
    // The second attempt used the moved head, so no files from two heads were mixed.
    assert_eq!(report.head_sha, f.api.head());
    assert_eq!(f.read("llms.txt").as_deref(), Some("# llms (moved)\n"));
    assert_eq!(f.api.count("GET /v1/projects/proj_payment/sync"), 2);
}

#[tokio::test]
async fn an_interrupted_pull_leaves_no_partial_state() {
    let f = Fixture::new().await;
    f.api.with(|s| s.fail_path = Some(PRD.into()));
    let err = f.pull().await.unwrap_err();
    assert_eq!(err.exit_code(), exit::UNAVAILABLE);
    assert!(
        !f.path("llms.txt").exists(),
        "nothing is applied until every blob arrived"
    );
    assert!(f.ws().load_lock().unwrap().head_sha.is_none());
    assert!(f.ws().load_manifest().unwrap().files.is_empty());

    f.api.with(|s| s.fail_path = None);
    assert!(f.pull().await.unwrap().lock_advanced);
}

#[tokio::test]
async fn rate_limited_reads_are_retried() {
    let f = Fixture::new().await;
    f.api.with(|s| s.rate_limit_docs_once = true);
    assert!(f.pull().await.unwrap().lock_advanced);
}

#[tokio::test]
async fn hostile_remote_paths_never_touch_the_disk() {
    let f = Fixture::new().await;
    f.api
        .with(|s| s.inject_manifest_path = Some(("../evil.md".into(), "pwned".into())));
    let err = f.pull().await.unwrap_err();
    assert!(err.to_string().contains("non-canonical"));
    assert!(!f.workspace_dir.path().join("evil.md").exists());
    assert!(!f.path("llms.txt").exists());
}

#[cfg(unix)]
#[tokio::test]
async fn symlinks_are_never_followed_or_sent() {
    let f = Fixture::new().await;
    f.pull().await.unwrap();
    let outside = tempfile::tempdir().unwrap();
    std::fs::write(outside.path().join("secret.md"), "secret").unwrap();
    std::os::unix::fs::symlink(
        outside.path().join("secret.md"),
        f.path("epic-payment/features/create-payment/cli.md"),
    )
    .unwrap();

    assert!(push::push(&f.device.env, &f.ws()).await.is_err());
    assert!(f.pull().await.is_err());
    assert_eq!(f.api.count("POST /v1/projects/proj_payment/changes"), 0);
    // A symlinked parent directory is refused as a write target too.
    std::fs::remove_file(f.path("epic-payment/features/create-payment/cli.md")).unwrap();
    std::fs::remove_dir_all(f.path("epic-payment")).unwrap();
    std::os::unix::fs::symlink(outside.path(), f.path("epic-payment")).unwrap();
    f.api.external_change(PRD, Some("# changed\n"));
    assert!(f.pull().await.is_err());
    assert!(!outside.path().join("prd.md").exists());
}

#[tokio::test]
async fn status_reports_each_category_and_diff_works_with_and_without_network() {
    let f = Fixture::new().await;
    f.pull().await.unwrap();
    f.write(README, "# mine\n"); // local-only
    std::fs::remove_file(f.path("llms.txt")).unwrap(); // deleted
    f.api
        .external_change("epic-payment/architecture.md", Some("# arch v2\n")); // remote-only
    f.write(PRD, "# local\n");
    f.api.external_change(PRD, Some("# remote\n")); // conflict

    let report = status::status(&f.device.env, &f.ws()).await.unwrap();
    let state = |p: &str| report.files.iter().find(|x| x.path == p).unwrap().state;
    assert_eq!(state(README), "local-only");
    assert_eq!(state("llms.txt"), "deleted");
    assert_eq!(state("epic-payment/architecture.md"), "remote-only");
    assert_eq!(state(PRD), "conflict");
    assert_eq!(state("epic-payment/product-brief.md"), "unchanged");
    assert!(!report.up_to_date);
    assert_eq!(report.exit_code_for_test(), exit::CONFLICT);

    let remote = status::diff_remote(&f.device.env, &f.ws(), PRD)
        .await
        .unwrap();
    assert!(
        !remote.identical && remote.diff.contains("-# remote") && remote.diff.contains("+# local")
    );

    let before = f.api.requests();
    let base = status::diff_base(&f.ws(), PRD).unwrap();
    assert_eq!(
        f.api.requests(),
        before,
        "diff --base must not use the network"
    );
    assert!(base.diff.contains("-# PRD") && base.diff.contains("+# local"));
}

#[tokio::test]
async fn update_applies_one_file_only_when_local_is_unchanged() {
    let f = Fixture::new().await;
    f.pull().await.unwrap();
    f.api.external_change(PRD, Some("# PRD v2\n"));
    f.api.external_change(README, Some("# readme v2\n"));

    let done = specio::sync::pull::update(&f.device.env, &f.ws(), PRD)
        .await
        .unwrap();
    assert_eq!(done.result, "updated");
    assert_eq!(f.read(PRD).as_deref(), Some("# PRD v2\n"));
    assert_eq!(
        f.read(README).as_deref(),
        Some("# Create payment\n"),
        "other files are untouched"
    );
    assert_eq!(
        specio::sync::pull::update(&f.device.env, &f.ws(), PRD)
            .await
            .unwrap()
            .result,
        "up-to-date"
    );

    f.write(README, "# my edit\n");
    let err = specio::sync::pull::update(&f.device.env, &f.ws(), README)
        .await
        .unwrap_err();
    assert!(matches!(err, Error::Conflict(_)));
    assert_eq!(f.read(README).as_deref(), Some("# my edit\n"));
    assert!(
        specio::sync::pull::update(&f.device.env, &f.ws(), "../x.md")
            .await
            .is_err()
    );
}

#[tokio::test]
async fn push_sends_one_batch_and_moves_baseline_and_lock() {
    let f = Fixture::new().await;
    f.pull().await.unwrap();
    f.write(README, "# readme edited\n"); // update
    f.write(NEW_DOC, "# frontend\n"); // create
    std::fs::remove_file(f.path("epic-payment/features/create-payment/backend.md")).unwrap(); // delete

    let before_commits = f.api.commits();
    let report = push::push(&f.device.env, &f.ws()).await.unwrap();
    assert_eq!(report.changes.len(), 3);
    assert_eq!(f.api.commits() - before_commits, 1, "one batch, one commit");
    assert_eq!(f.api.file(README).as_deref(), Some("# readme edited\n"));
    assert_eq!(f.api.file(NEW_DOC).as_deref(), Some("# frontend\n"));
    assert!(
        f.api
            .file("epic-payment/features/create-payment/backend.md")
            .is_none()
    );

    assert_eq!(
        f.ws().load_lock().unwrap().head_sha.as_deref(),
        Some(f.api.head().as_str())
    );
    let status = status::status(&f.device.env, &f.ws()).await.unwrap();
    assert!(
        status.files.iter().all(|x| x.state == "unchanged"),
        "{:?}",
        status.files
    );
    assert!(status.up_to_date);
    assert!(
        push::push(&f.device.env, &f.ws())
            .await
            .unwrap()
            .nothing_to_push
    );
}

#[tokio::test]
async fn a_rejected_push_keeps_lock_and_files_and_is_not_retried() {
    let f = Fixture::new().await;
    f.pull().await.unwrap();
    let lock = f.ws().load_lock().unwrap();
    f.write(README, "# mine\n");
    f.api.external_change(PRD, Some("# someone else\n")); // the head moves

    let err = push::push(&f.device.env, &f.ws()).await.unwrap_err();
    assert!(matches!(err, Error::Conflict(_)));
    assert!(err.to_string().contains("specio pull"));
    assert_eq!(
        f.api.count("POST /v1/projects/proj_payment/changes"),
        1,
        "no automatic retry"
    );
    assert_eq!(f.ws().load_lock().unwrap(), lock);
    assert_eq!(f.read(README).as_deref(), Some("# mine\n"));

    // After pulling, the same edit pushes cleanly.
    f.pull().await.unwrap();
    push::push(&f.device.env, &f.ws()).await.unwrap();
}

#[tokio::test]
async fn repeating_an_interrupted_push_finalises_it_without_a_second_commit() {
    let f = Fixture::new().await;
    f.pull().await.unwrap();
    f.write(README, "# mine\n");
    f.api.with(|s| s.fail_after_commit_once = true);
    let before = f.api.commits();

    let err = push::push(&f.device.env, &f.ws()).await.unwrap_err();
    assert_eq!(err.exit_code(), exit::UNAVAILABLE);
    // The lock did not move, so the retry carries the same base and the same Idempotency-Key.
    let report = push::push(&f.device.env, &f.ws()).await.unwrap();
    assert!(report.replayed);
    assert_eq!(f.api.commits() - before, 1);
    assert_eq!(
        f.ws().load_lock().unwrap().head_sha.as_deref(),
        Some(f.api.head().as_str())
    );
}

#[tokio::test]
async fn push_rejects_unsendable_content_before_any_request() {
    let f = Fixture::new().await;
    f.pull().await.unwrap();
    std::fs::write(f.path(README), [0xff, 0xfe, 0x00]).unwrap();
    assert_eq!(
        push::push(&f.device.env, &f.ws())
            .await
            .unwrap_err()
            .exit_code(),
        exit::INVALID
    );
    f.write(README, &"x".repeat(1024 * 1024 + 1));
    assert!(push::push(&f.device.env, &f.ws()).await.is_err());
    f.write(README, "# ok\n");
    f.write("epic-payment/notes.md", "ignored: not a canonical path\n");
    let report = push::push(&f.device.env, &f.ws()).await.unwrap();
    assert_eq!(
        report.changes.len(),
        1,
        "non-canonical files are never sent"
    );
    assert_eq!(f.api.count("POST /v1/projects/proj_payment/changes"), 1);
}

#[tokio::test]
async fn context_is_deterministic_offline_and_ordered() {
    let f = Fixture::new().await;
    f.api.external_change(
        "epic-payment/features/create-payment/DESIGN.md",
        Some("# Design\n"),
    );
    f.api.external_change(
        "epic-payment/features/create-payment/acceptance-criteria.md",
        Some("# AC\n"),
    );
    f.pull().await.unwrap();

    let before = f.api.requests();
    let report = context(&f.ws(), "epic-payment/create-payment").unwrap();
    assert_eq!(f.api.requests(), before, "context never calls the API");
    assert_eq!(
        report.paths,
        vec![
            "specs/llms.txt",
            "specs/epic-payment/product-brief.md",
            "specs/epic-payment/prd.md",
            "specs/epic-payment/architecture.md",
            "specs/epic-payment/features/create-payment/README.md",
            "specs/epic-payment/features/create-payment/acceptance-criteria.md",
            "specs/epic-payment/features/create-payment/DESIGN.md",
            "specs/epic-payment/features/create-payment/backend.md",
        ]
    );
    assert_eq!(report.head_sha, f.api.head());
    assert_eq!(report.project_id, PROJECT);
    assert_eq!(
        context(&f.ws(), "epic-payment/create-payment")
            .unwrap()
            .paths,
        report.paths
    );

    for bad in [
        "epic-payment",
        "../x/y",
        "epic-payment/create-payment/extra",
        "Epic/Feature",
        "epic-payment/missing",
    ] {
        assert!(context(&f.ws(), bad).is_err(), "{bad}");
    }
}

#[tokio::test]
async fn a_revoked_role_is_caught_by_the_api_even_while_the_snapshot_is_valid() {
    let f = Fixture::new().await;
    // The snapshot still lists the project, but the API no longer knows this membership.
    f.api.with(|s| {
        s.members
            .get_mut("user_a")
            .unwrap()
            .retain(|p| p != PROJECT)
    });
    assert_eq!(
        specio::init::list(&f.device.env)
            .await
            .unwrap()
            .projects
            .len(),
        3
    );
    let err = f.pull().await.unwrap_err();
    assert_eq!(err.exit_code(), exit::INVALID); // generic 404 for a project the user may not read
    assert_eq!(
        f.api.count("POST"),
        f.api.count("POST /v1/auth"),
        "no mutation was attempted"
    );
}

#[tokio::test]
async fn state_is_separated_per_workspace() {
    let f = Fixture::new().await;
    f.pull().await.unwrap();
    let other = tempfile::tempdir().unwrap();
    let options = specio::init::InitOptions {
        repo: "acme/cart-specs",
        repository_type: "backend",
        local_dir: "docs/specs",
        reconfigure: false,
    };
    specio::init::init(
        &f.device.env,
        &std::fs::canonicalize(other.path()).unwrap(),
        options,
    )
    .await
    .unwrap();
    let ws = specio::workspace::Workspace::discover(other.path()).unwrap();
    assert!(ws.load_lock().unwrap().head_sha.is_none());
    assert!(ws.load_manifest().unwrap().files.is_empty());
    assert_eq!(ws.config.spec_source.local_dir, "docs/specs");
    assert_ne!(
        f.device.env.dirs.workspace_lock(&f.ws().root),
        f.device.env.dirs.workspace_lock(&ws.root)
    );
}

#[tokio::test]
async fn a_second_command_in_the_same_workspace_fails_fast() {
    let f = Fixture::new().await;
    let _held = f.ws().guard(&f.device.env, false).unwrap();
    let err = f.pull().await.unwrap_err();
    assert!(matches!(err, Error::Conflict(_)));
}

trait ExitForTest {
    fn exit_code_for_test(&self) -> u8;
}

impl ExitForTest for status::StatusReport {
    fn exit_code_for_test(&self) -> u8 {
        use specio::output::Human;
        self.exit_code()
    }
}
