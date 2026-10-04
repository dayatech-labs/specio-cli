//! A stateful in-process fake of the Specio API (the contract in `specio-api/contract/`).
#![allow(dead_code)]
use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use serde_json::{Value, json};
use specio::agent::Scheduler;
use specio::credentials::MemoryStore;
use specio::dirs::AppDirs;
use specio::env::Env;
use specio::hashing::{git_blob_sha, sha256_bytes, sha256_hex};
use std::collections::{BTreeMap, HashMap};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
pub mod http;

pub const PROJECT: &str = "proj_payment";
pub const REPO: &str = "acme/payment-specs";

#[derive(Default)]
pub struct Session {
    user: String,
    access: Option<String>,
    revoked: bool,
}

pub struct Project {
    pub repo: Option<String>,
    pub files: BTreeMap<String, String>,
    pub head: String,
    pub commits: usize,
}

pub struct State {
    pub projects: BTreeMap<String, Project>,
    pub members: HashMap<String, Vec<String>>, // user -> project ids
    pub auth_version: HashMap<String, i64>,
    cli_requests: HashMap<String, (String, bool)>, // id -> (challenge, consumed)
    pub next_login_user: String,
    sessions: HashMap<String, Session>,
    access_index: HashMap<String, String>,
    counter: usize,
    pub snapshot_ttl: i64,
    pub send_304_headers: bool,
    pub forbid_writes: bool,
    /// Lifetime of the access token the next login receives (the real API issues 30 days).
    pub token_ttl: i64,
    pub log: Vec<String>,
    /// Move the head (an external push) right after serving the next `sync`.
    pub move_head_after_sync: bool,
    /// Fail document reads for this path with a 500.
    pub fail_path: Option<String>,
    /// Report this extra path in the manifest (for hostile-path tests).
    pub inject_manifest_path: Option<(String, String)>,
    /// Commit a push, then answer 502 once (the response is "lost").
    pub fail_after_commit_once: bool,
    idempotent: HashMap<String, (String, Value)>,
    pub rate_limit_docs_once: bool,
}

pub struct FakeApi {
    pub url: String,
    pub state: Arc<Mutex<State>>,
    _server: http::HttpServer,
}

fn head_of(n: usize) -> String {
    sha256_hex(format!("head-{n}").as_bytes())[..40].to_string()
}

impl State {
    fn new() -> State {
        let mut files = BTreeMap::new();
        files.insert("llms.txt".into(), "# llms\n".into());
        files.insert(
            "epics/epic-payment/prd.md".into(),
            "# PRD\n\nPayments.\n".into(),
        );
        files.insert(
            "epics/epic-payment/product-brief.md".into(),
            "# Brief\n".into(),
        );
        files.insert(
            "epics/epic-payment/architecture.md".into(),
            "# Architecture\n".into(),
        );
        files.insert(
            "epics/epic-payment/features/create-payment/README.md".into(),
            "# Create payment\n".into(),
        );
        files.insert(
            "epics/epic-payment/features/create-payment/backend.md".into(),
            "# Backend\n".into(),
        );
        let mut projects = BTreeMap::new();
        projects.insert(
            PROJECT.to_string(),
            Project {
                repo: Some(REPO.into()),
                files,
                head: head_of(0),
                commits: 0,
            },
        );
        projects.insert(
            "proj_other".to_string(),
            Project {
                repo: Some("other/payment-specs".into()),
                files: BTreeMap::new(),
                head: head_of(100),
                commits: 0,
            },
        );
        projects.insert(
            "proj_cart".to_string(),
            Project {
                repo: Some("acme/cart-specs".into()),
                files: BTreeMap::new(),
                head: head_of(200),
                commits: 0,
            },
        );
        let mut members = HashMap::new();
        for user in ["user_a", "user_b"] {
            members.insert(
                user.to_string(),
                vec![
                    PROJECT.to_string(),
                    "proj_other".to_string(),
                    "proj_cart".to_string(),
                ],
            );
        }
        State {
            projects,
            members,
            auth_version: HashMap::new(),
            cli_requests: HashMap::new(),
            next_login_user: "user_a".into(),
            sessions: HashMap::new(),
            access_index: HashMap::new(),
            counter: 0,
            snapshot_ttl: 600,
            send_304_headers: true,
            forbid_writes: false,
            token_ttl: 30 * 24 * 3600,
            log: vec![],
            move_head_after_sync: false,
            fail_path: None,
            inject_manifest_path: None,
            fail_after_commit_once: false,
            idempotent: HashMap::new(),
            rate_limit_docs_once: false,
        }
    }

    fn next(&mut self, prefix: &str) -> String {
        self.counter += 1;
        format!("{prefix}_{}", self.counter)
    }

    fn bump_head(&mut self, project: &str) -> String {
        self.counter += 1;
        let n = self.counter;
        let p = self.projects.get_mut(project).expect("project");
        p.commits += 1;
        p.head = head_of(n);
        p.head.clone()
    }

    pub fn external_change(&mut self, project: &str, path: &str, content: Option<&str>) {
        let p = self.projects.get_mut(project).expect("project");
        match content {
            Some(c) => p.files.insert(path.into(), c.into()),
            None => p.files.remove(path),
        };
        self.bump_head(project);
    }

    fn user_for(&self, token: &str) -> Option<String> {
        let id = self.access_index.get(token)?;
        let s = self.sessions.get(id)?;
        (!s.revoked && s.access.as_deref() == Some(token)).then(|| s.user.clone())
    }

    fn snapshot_etag(&self, user: &str) -> String {
        let version = self.auth_version.get(user).copied().unwrap_or(1);
        let ids = self.members.get(user).cloned().unwrap_or_default();
        format!(
            "\"{}\"",
            &sha256_hex(format!("{user}:{version}:{ids:?}").as_bytes())[..32]
        )
    }
}

type Resp = (u16, Value, Vec<(String, String)>);

fn error(status: u16, code: &str, message: &str, details: Option<Value>) -> Resp {
    (
        status,
        json!({ "error": { "code": code, "message": message, "request_id": "req-test", "details": details } }),
        vec![],
    )
}

fn iso(offset_seconds: i64) -> String {
    let t = time::OffsetDateTime::now_utc() + time::Duration::seconds(offset_seconds);
    t.format(&time::format_description::well_known::Rfc3339)
        .expect("format")
}

fn query_param(url: &str, name: &str) -> Option<String> {
    let parsed = url::Url::parse(&format!("http://x{url}")).ok()?;
    parsed
        .query_pairs()
        .find(|(k, _)| k == name)
        .map(|(_, v)| v.into_owned())
}

fn handle(
    state: &Mutex<State>,
    base: &str,
    method: &str,
    url: &str,
    headers: &HashMap<String, String>,
    body: &str,
) -> Resp {
    let mut st = state.lock().expect("state");
    let path = url.split('?').next().unwrap_or(url).to_string();
    st.log.push(format!("{method} {path}"));
    let body: Value = serde_json::from_str(body).unwrap_or(Value::Null);
    let bearer = headers
        .get("authorization")
        .and_then(|v| v.strip_prefix("Bearer "))
        .map(str::to_owned);

    match (method, path.as_str()) {
        ("POST", "/v1/auth/cli/requests") => {
            let id = st.next("req");
            let challenge = body["code_challenge"]
                .as_str()
                .unwrap_or_default()
                .to_string();
            st.cli_requests.insert(id.clone(), (challenge, false));
            return (
                201,
                json!({ "request_id": id, "verification_url": format!("{base}/v1/auth/cli/verify?request_id={id}"), "expires_at": iso(600), "poll_after_seconds": 0 }),
                vec![],
            );
        }
        ("POST", p) if p.starts_with("/v1/auth/cli/requests/") && p.ends_with("/complete") => {
            let id = p
                .trim_start_matches("/v1/auth/cli/requests/")
                .trim_end_matches("/complete")
                .to_string();
            let Some((challenge, consumed)) = st.cli_requests.get(&id).cloned() else {
                return error(404, "not_found", "Not found", None);
            };
            let verifier = body["code_verifier"].as_str().unwrap_or_default();
            if URL_SAFE_NO_PAD.encode(sha256_bytes(verifier.as_bytes())) != challenge {
                return error(400, "invalid_grant", "PKCE verifier does not match", None);
            }
            if consumed {
                return error(410, "request_consumed", "Request was already used", None);
            }
            st.cli_requests.insert(id, (challenge, true));
            let user = st.next_login_user.clone();
            let session_id = st.next("sess");
            let access = st.next("at");
            st.access_index.insert(access.clone(), session_id.clone());
            st.sessions.insert(
                session_id,
                Session {
                    user,
                    access: Some(access.clone()),
                    revoked: false,
                },
            );
            return (
                200,
                json!({ "access_token": access, "access_token_expires_at": iso(st.token_ttl) }),
                vec![],
            );
        }
        ("POST", "/v1/auth/logout") => {
            // Stateless tokens: only `all` revokes anything, and it ends every session of the user.
            let Some(user) = bearer.as_deref().and_then(|t| st.user_for(t)) else {
                return error(
                    401,
                    "unauthorized",
                    "Missing, invalid, expired, or revoked credentials",
                    None,
                );
            };
            if body["all"].as_bool() == Some(true) {
                st.sessions
                    .values_mut()
                    .filter(|s| s.user == user)
                    .for_each(|s| s.revoked = true);
            }
            return (204, Value::Null, vec![]);
        }
        _ => {}
    }

    // Everything below needs a Bearer access token.
    let Some(user) = bearer.as_deref().and_then(|t| st.user_for(t)) else {
        return error(
            401,
            "unauthorized",
            "Missing, invalid, expired, or revoked credentials",
            None,
        );
    };

    if method == "GET" && path == "/v1/me" {
        return (
            200,
            json!({ "user_id": user, "email": format!("{user}@example.com"), "name": user, "authorization_version": 1, "session": { "kind": "cli", "expires_at": iso(900) } }),
            vec![],
        );
    }
    if method == "GET" && path == "/v1/projects" {
        let etag = st.snapshot_etag(&user);
        if headers.get("if-none-match") == Some(&etag) {
            let mut h = vec![("ETag".to_string(), etag)];
            if st.send_304_headers {
                h.push(("X-Specio-Snapshot-Issued-At".into(), iso(0)));
                h.push(("X-Specio-Snapshot-Expires-At".into(), iso(st.snapshot_ttl)));
            }
            return (304, Value::Null, h);
        }
        let projects: Vec<Value> = st
            .members
            .get(&user)
            .cloned()
            .unwrap_or_default()
            .iter()
            .filter_map(|id| st.projects.get(id).map(|p| (id, p)))
            .map(|(id, p)| {
                json!({ "project_id": id, "display_name": id, "repository_full_name": p.repo, "branch": "main", "roles": ["engineer"],
                    "capabilities": { "create": { "allow": ["epics/*/features/*/*.md"], "deny": [] }, "update": { "allow": ["epics/*/features/*/*.md"], "deny": [] }, "can_delete": false, "can_restore": false, "can_manage": false } })
            })
            .collect();
        let body = json!({ "user_id": user, "authorization_version": st.auth_version.get(&user).copied().unwrap_or(1), "issued_at": iso(0), "expires_at": iso(st.snapshot_ttl), "projects": projects });
        return (200, body, vec![("ETag".into(), etag)]);
    }

    let Some(rest) = path.strip_prefix("/v1/projects/") else {
        return error(404, "not_found", "Not found", None);
    };
    let (project_id, tail) = rest.split_once('/').unwrap_or((rest, ""));
    let member = st
        .members
        .get(&user)
        .is_some_and(|ids| ids.iter().any(|i| i == project_id));
    if !member || !st.projects.contains_key(project_id) {
        return error(404, "not_found", "Not found", None);
    }
    let project_id = project_id.to_string();

    match (method, tail) {
        ("GET", "sync") => {
            let p = &st.projects[&project_id];
            let mut files: Vec<Value> = p.files.iter().map(|(path, c)| json!({ "path": path, "sha": git_blob_sha(c.as_bytes()), "size": c.len() })).collect();
            if let Some((path, content)) = &st.inject_manifest_path {
                files.push(json!({ "path": path, "sha": git_blob_sha(content.as_bytes()), "size": content.len() }));
            }
            let body = json!({ "head_sha": p.head, "files": files });
            if st.move_head_after_sync {
                st.move_head_after_sync = false;
                st.external_change(&project_id, "llms.txt", Some("# llms (moved)\n"));
            }
            (200, body, vec![])
        }
        ("GET", "docs") => {
            if st.rate_limit_docs_once {
                st.rate_limit_docs_once = false;
                return (
                    429,
                    json!({ "error": { "code": "rate_limited", "message": "Slow down", "request_id": "r" } }),
                    vec![("Retry-After".into(), "0".into())],
                );
            }
            let wanted = query_param(url, "path").unwrap_or_default();
            if st.fail_path.as_deref() == Some(wanted.as_str()) {
                return error(500, "internal", "boom", None);
            }
            let p = &st.projects[&project_id];
            if let Some(pinned) = query_param(url, "head_sha")
                && pinned != p.head
            {
                return error(
                    409,
                    "head_changed",
                    "The repository head changed",
                    Some(json!({ "head_sha": p.head })),
                );
            }
            if let Some((path, content)) = &st.inject_manifest_path
                && *path == wanted
            {
                return (
                    200,
                    json!({ "path": wanted, "content": content, "document_sha": git_blob_sha(content.as_bytes()), "head_sha": p.head, "capability": {} }),
                    vec![],
                );
            }
            match p.files.get(&wanted) {
                Some(c) => (
                    200,
                    json!({ "path": wanted, "content": c, "document_sha": git_blob_sha(c.as_bytes()), "head_sha": p.head, "capability": { "can_read": true } }),
                    vec![],
                ),
                None => error(404, "not_found", "Not found", None),
            }
        }
        ("POST", "changes") => {
            if st.forbid_writes {
                return error(
                    403,
                    "policy_denied",
                    "Not allowed to apply every change in this batch",
                    Some(json!([])),
                );
            }
            let key = headers.get("idempotency-key").cloned().unwrap_or_default();
            let payload_hash = sha256_hex(body.to_string().as_bytes());
            if let Some((hash, response)) = st.idempotent.get(&key) {
                if *hash != payload_hash {
                    return error(
                        422,
                        "idempotency_key_reuse",
                        "Key used with a different payload",
                        None,
                    );
                }
                let mut response = response.clone();
                response["replayed"] = json!(true);
                return (200, response, vec![]);
            }
            let p = &st.projects[&project_id];
            if body["base_commit_sha"].as_str() != Some(p.head.as_str()) {
                return error(
                    409,
                    "head_changed",
                    "The repository changed since it was read",
                    Some(json!({ "head_sha": p.head })),
                );
            }
            let operations = body["operations"].as_array().cloned().unwrap_or_default();
            let mut files = p.files.clone();
            let mut done = vec![];
            for op in &operations {
                let path = op["path"].as_str().unwrap_or_default().to_string();
                let current_sha = files.get(&path).map(|c| git_blob_sha(c.as_bytes()));
                match op["action"].as_str().unwrap_or_default() {
                    "create" => {
                        if files.contains_key(&path) {
                            return error(409, "path_exists", "Path exists", None);
                        }
                        files.insert(
                            path.clone(),
                            op["content"].as_str().unwrap_or_default().into(),
                        );
                        done.push(json!({ "action": "create", "path": path, "document_sha": git_blob_sha(op["content"].as_str().unwrap_or_default().as_bytes()) }));
                    }
                    "update" => {
                        if current_sha.as_deref() != op["document_sha"].as_str() {
                            return error(409, "document_changed", "Document changed", None);
                        }
                        files.insert(
                            path.clone(),
                            op["content"].as_str().unwrap_or_default().into(),
                        );
                        done.push(json!({ "action": "update", "path": path, "document_sha": git_blob_sha(op["content"].as_str().unwrap_or_default().as_bytes()) }));
                    }
                    "delete" => {
                        if current_sha.as_deref() != op["document_sha"].as_str() {
                            return error(409, "document_changed", "Document changed", None);
                        }
                        files.remove(&path);
                        done.push(json!({ "action": "delete", "path": path, "document_sha": Value::Null }));
                    }
                    _ => return error(422, "path_invalid", "bad action", None),
                }
            }
            st.projects.get_mut(&project_id).expect("project").files = files;
            let head = st.bump_head(&project_id);
            let response = json!({ "commit_sha": head, "head_sha": head, "replayed": false, "operations": done });
            st.idempotent.insert(key, (payload_hash, response.clone()));
            if st.fail_after_commit_once {
                st.fail_after_commit_once = false;
                return error(502, "github_unavailable", "GitHub unavailable", None);
            }
            (200, response, vec![])
        }
        _ => error(404, "not_found", "Not found", None),
    }
}

impl FakeApi {
    pub fn start() -> FakeApi {
        let state = Arc::new(Mutex::new(State::new()));
        // The base URL is only known once the listener is bound, so the handler reads it lazily.
        let base = Arc::new(Mutex::new(String::new()));
        let (handler_state, handler_base) = (state.clone(), base.clone());
        let server = http::serve(move |request| {
            let base = handler_base.lock().expect("base").clone();
            let (status, value, extra) = handle(
                &handler_state,
                &base,
                &request.method,
                &request.url,
                &request.headers,
                &request.body,
            );
            let mut headers = vec![("Content-Type".to_string(), "application/json".to_string())];
            headers.extend(extra);
            http::Reply {
                status,
                headers,
                body: if value.is_null() {
                    vec![]
                } else {
                    value.to_string().into_bytes()
                },
            }
        });
        let url = format!("http://{}", server.addr);
        *base.lock().expect("base") = url.clone();
        FakeApi {
            url,
            state,
            _server: server,
        }
    }

    pub fn with<R>(&self, f: impl FnOnce(&mut State) -> R) -> R {
        f(&mut self.state.lock().expect("state"))
    }

    pub fn requests(&self) -> usize {
        self.with(|s| s.log.len())
    }

    pub fn count(&self, needle: &str) -> usize {
        self.with(|s| s.log.iter().filter(|l| l.contains(needle)).count())
    }

    pub fn file(&self, path: &str) -> Option<String> {
        self.with(|s| s.projects[PROJECT].files.get(path).cloned())
    }

    pub fn head(&self) -> String {
        self.with(|s| s.projects[PROJECT].head.clone())
    }

    pub fn commits(&self) -> usize {
        self.with(|s| s.projects[PROJECT].commits)
    }

    pub fn external_change(&self, path: &str, content: Option<&str>) {
        self.with(|s| s.external_change(PROJECT, path, content));
    }

    pub fn revoke_all_sessions(&self) {
        self.with(|s| s.sessions.values_mut().for_each(|x| x.revoked = true));
    }
}

#[derive(Default)]
pub struct RecordingScheduler {
    pub installs: AtomicUsize,
    pub uninstalls: AtomicUsize,
}

impl Scheduler for RecordingScheduler {
    fn install(&self) -> specio::error::Result<()> {
        self.installs.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }
    fn uninstall(&self) -> specio::error::Result<()> {
        self.uninstalls.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }
}

/// One "device": its own data directory, credential store and scheduler, sharing the fake API.
pub struct Device {
    pub env: Env,
    pub creds: Arc<MemoryStore>,
    pub scheduler: Arc<RecordingScheduler>,
    pub data: tempfile::TempDir,
}

impl Device {
    pub fn new(api: &FakeApi) -> Device {
        let data = tempfile::tempdir().expect("tempdir");
        let creds = Arc::new(MemoryStore::default());
        let scheduler = Arc::new(RecordingScheduler::default());
        let api_url = specio::api::parse_api_url(&api.url).expect("api url");
        let env = Env {
            client: specio::api::Client::new(api_url.clone()).expect("client"),
            dirs: AppDirs::at(data.path()),
            creds: creds.clone(),
            scheduler: scheduler.clone(),
            api_url,
            open_browser: false,
        };
        Device {
            env,
            creds,
            scheduler,
            data,
        }
    }

    /// A second "process" on the same device: same data directory and credential store, own HTTP client.
    pub fn another_process(&self) -> Env {
        Env {
            client: specio::api::Client::new(self.env.api_url.clone()).expect("client"),
            dirs: AppDirs::at(self.data.path()),
            creds: self.creds.clone(),
            scheduler: self.scheduler.clone(),
            api_url: self.env.api_url.clone(),
            open_browser: false,
        }
    }

    pub async fn login(&self) -> specio::session::LoginReport {
        specio::session::Session::new(&self.env)
            .login()
            .await
            .expect("login")
    }
}

/// A logged-in device with an initialised, empty workspace.
pub struct Fixture {
    pub api: FakeApi,
    pub device: Device,
    pub workspace_dir: tempfile::TempDir,
}

impl Fixture {
    pub async fn new() -> Fixture {
        let api = FakeApi::start();
        let device = Device::new(&api);
        device.login().await;
        let workspace_dir = tempfile::tempdir().expect("workspace");
        let fixture = Fixture {
            api,
            device,
            workspace_dir,
        };
        fixture.init().await;
        fixture
    }

    pub async fn init(&self) {
        let options = specio::init::InitOptions {
            repo: REPO,
            repository_type: "frontend",
            local_dir: "specs",
            reconfigure: false,
        };
        specio::init::init(
            &self.device.env,
            &std::fs::canonicalize(self.workspace_dir.path()).expect("canonical"),
            options,
        )
        .await
        .expect("init");
    }

    pub fn ws(&self) -> specio::workspace::Workspace {
        specio::workspace::Workspace::discover(self.workspace_dir.path()).expect("workspace")
    }

    pub fn path(&self, rel: &str) -> std::path::PathBuf {
        self.ws().local_path(rel)
    }

    pub fn read(&self, rel: &str) -> Option<String> {
        std::fs::read_to_string(self.path(rel)).ok()
    }

    pub fn write(&self, rel: &str, content: &str) {
        let p = self.path(rel);
        std::fs::create_dir_all(p.parent().expect("parent")).expect("mkdir");
        std::fs::write(p, content).expect("write");
    }

    pub async fn pull(&self) -> specio::error::Result<specio::sync::pull::PullReport> {
        specio::sync::pull::pull(&self.device.env, &self.ws()).await
    }
}
