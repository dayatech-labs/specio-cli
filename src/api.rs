//! Typed client for the Specio API. It never logs tokens or document content.
use crate::error::{ApiError, Error, Result};
use reqwest::{Method, StatusCode};
use secrecy::{ExposeSecret, SecretString};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use std::sync::Once;
use std::time::Duration;
use url::Url;

pub const DEFAULT_API_URL: &str = "https://specio-api.dayatech.workers.dev";
const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);
const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
/// Reads are retried on 429/503 only, waiting at most this long per attempt.
const MAX_RETRY_WAIT: u64 = 30;
const READ_ATTEMPTS: u32 = 3;

/// Accept `https` URLs, and plain `http` only for loopback (a local API during development).
pub fn parse_api_url(raw: &str) -> Result<Url> {
    let url =
        Url::parse(raw).map_err(|e| Error::invalid(format!("invalid API URL {raw:?}: {e}")))?;
    let loopback = matches!(url.host(), Some(url::Host::Domain("localhost")))
        || matches!(url.host(), Some(url::Host::Ipv4(ip)) if ip.is_loopback())
        || matches!(url.host(), Some(url::Host::Ipv6(ip)) if ip.is_loopback());
    match url.scheme() {
        "https" => {}
        "http" if loopback => {}
        _ => {
            return Err(Error::invalid(
                "the API URL must use https (http is allowed only for localhost)",
            ));
        }
    }
    if url.host_str().is_none() || !url.username().is_empty() || url.password().is_some() {
        return Err(Error::invalid(
            "the API URL must be an origin without credentials",
        ));
    }
    if (url.path() != "/" && !url.path().is_empty())
        || url.query().is_some()
        || url.fragment().is_some()
    {
        return Err(Error::invalid(
            "the API URL must be an origin without path, query, or fragment",
        ));
    }
    Ok(url)
}

pub fn install_crypto_provider() {
    static ONCE: Once = Once::new();
    ONCE.call_once(|| {
        // Another provider may already be installed by the host process; that is fine.
        let _ = rustls::crypto::ring::default_provider().install_default();
    });
}

// ---------------------------------------------------------------- payloads

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RuleSet {
    pub allow: Vec<String>,
    pub deny: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Capabilities {
    pub create: RuleSet,
    pub update: RuleSet,
    pub can_delete: bool,
    pub can_restore: bool,
    pub can_manage: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ProjectSummary {
    pub project_id: String,
    pub display_name: String,
    pub repository_full_name: Option<String>,
    pub branch: String,
    pub roles: Vec<String>,
    pub capabilities: Capabilities,
}

#[derive(Debug, Clone, Deserialize)]
pub struct SnapshotBody {
    pub user_id: String,
    pub authorization_version: i64,
    pub issued_at: String,
    pub expires_at: String,
    pub projects: Vec<ProjectSummary>,
}

pub enum ProjectsResponse {
    Fresh {
        body: SnapshotBody,
        etag: Option<String>,
    },
    /// `304`: the window headers carry the new validity period.
    NotModified {
        issued_at: Option<String>,
        expires_at: Option<String>,
    },
}

#[derive(Debug, Clone, Deserialize)]
pub struct CliRequest {
    pub request_id: String,
    pub verification_url: String,
    pub expires_at: String,
    pub poll_after_seconds: u64,
}

#[derive(Deserialize)]
pub struct Tokens {
    pub access_token: String,
    #[allow(dead_code)]
    pub access_token_expires_at: String,
    pub refresh_credential: String,
}

impl std::fmt::Debug for Tokens {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Tokens([redacted])")
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct Me {
    pub user_id: String,
    pub email: String,
    pub name: Option<String>,
}

#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
pub struct SyncFile {
    pub path: String,
    pub sha: String,
    pub size: u64,
}

#[derive(Debug, Clone, Deserialize)]
pub struct SyncManifest {
    pub head_sha: String,
    pub files: Vec<SyncFile>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Document {
    pub path: String,
    pub content: String,
    pub document_sha: String,
    pub head_sha: String,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(tag = "action", rename_all = "snake_case")]
pub enum ChangeOperation {
    Create {
        path: String,
        content: String,
    },
    Update {
        path: String,
        content: String,
        document_sha: String,
    },
    Delete {
        path: String,
        document_sha: String,
    },
}

impl ChangeOperation {
    pub fn path(&self) -> &str {
        match self {
            ChangeOperation::Create { path, .. }
            | ChangeOperation::Update { path, .. }
            | ChangeOperation::Delete { path, .. } => path,
        }
    }

    pub fn action(&self) -> &'static str {
        match self {
            ChangeOperation::Create { .. } => "create",
            ChangeOperation::Update { .. } => "update",
            ChangeOperation::Delete { .. } => "delete",
        }
    }
}

#[derive(Debug, Serialize)]
pub struct ChangesRequest<'a> {
    pub base_commit_sha: &'a str,
    pub operations: &'a [ChangeOperation],
}

#[derive(Debug, Clone, Deserialize)]
pub struct ChangedPath {
    pub action: String,
    pub path: String,
    pub document_sha: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ChangesResult {
    pub commit_sha: String,
    pub head_sha: String,
    pub replayed: bool,
    pub operations: Vec<ChangedPath>,
}

// ------------------------------------------------------------------ client

#[derive(Clone)]
pub struct Client {
    http: reqwest::Client,
    base: Url,
}

struct Call<'a> {
    method: Method,
    url: Url,
    token: Option<&'a SecretString>,
    json: Option<serde_json::Value>,
    headers: Vec<(&'static str, String)>,
    retry: bool,
}

impl Client {
    pub fn new(base: Url) -> Result<Client> {
        install_crypto_provider();
        let http = reqwest::Client::builder()
            .user_agent(format!("specio-cli/{}", env!("CARGO_PKG_VERSION")))
            .timeout(REQUEST_TIMEOUT)
            .connect_timeout(CONNECT_TIMEOUT)
            // The API never redirects; following one could forward the Bearer token elsewhere.
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .map_err(|e| Error::Other(format!("cannot build the HTTP client: {e}")))?;
        Ok(Client { http, base })
    }

    pub fn base(&self) -> &Url {
        &self.base
    }

    fn url(&self, path: &str, query: &[(&str, &str)]) -> Url {
        let mut url = self.base.clone();
        url.set_path(path);
        if !query.is_empty() {
            let mut pairs = url.query_pairs_mut();
            for (k, v) in query {
                pairs.append_pair(k, v);
            }
        }
        url
    }

    async fn send(&self, call: Call<'_>) -> Result<reqwest::Response> {
        let mut attempt = 0;
        loop {
            attempt += 1;
            let mut request = self.http.request(call.method.clone(), call.url.clone());
            if let Some(token) = call.token {
                request = request.bearer_auth(token.expose_secret());
            }
            for (name, value) in &call.headers {
                request = request.header(*name, value);
            }
            if let Some(body) = &call.json {
                request = request.json(body);
            }
            let response = request
                .send()
                .await
                .map_err(|e| Error::Network(describe_transport(&e)))?;
            let status = response.status();
            let request_id = response
                .headers()
                .get("x-request-id")
                .and_then(|v| v.to_str().ok())
                .map(str::to_owned);
            tracing::debug!(method = %call.method, path = call.url.path(), status = status.as_u16(), request_id = request_id.as_deref(), "api response");

            if status.is_success() || status == StatusCode::NOT_MODIFIED {
                return Ok(response);
            }
            let error = read_error(response, status, request_id).await;
            let retryable =
                matches!(status.as_u16(), 429 | 503) && call.retry && attempt < READ_ATTEMPTS;
            let wait = error.retry_after.filter(|s| *s <= MAX_RETRY_WAIT);
            match (retryable, wait) {
                (true, Some(seconds)) => tokio::time::sleep(Duration::from_secs(seconds)).await,
                _ => return Err(Error::Api(Box::new(error))),
            }
        }
    }

    async fn json<T: DeserializeOwned>(response: reqwest::Response) -> Result<T> {
        response
            .json::<T>()
            .await
            .map_err(|e| Error::Other(format!("unexpected API response: {e}")))
    }

    fn get_call<'a>(
        &self,
        path: &str,
        query: &[(&str, &str)],
        token: &'a SecretString,
    ) -> Call<'a> {
        Call {
            method: Method::GET,
            url: self.url(path, query),
            token: Some(token),
            json: None,
            headers: vec![],
            retry: true,
        }
    }

    fn post_call<'a>(
        &self,
        path: &str,
        token: Option<&'a SecretString>,
        json: serde_json::Value,
    ) -> Call<'a> {
        Call {
            method: Method::POST,
            url: self.url(path, &[]),
            token,
            json: Some(json),
            headers: vec![],
            retry: false,
        }
    }

    // ---- authentication

    pub async fn cli_request(&self, code_challenge: &str, device_name: &str) -> Result<CliRequest> {
        let body = serde_json::json!({ "code_challenge": code_challenge, "code_challenge_method": "S256", "device_name": device_name });
        Self::json(
            self.send(self.post_call("/v1/auth/cli/requests", None, body))
                .await?,
        )
        .await
    }

    /// `authorization_pending`, `slow_down`, `invalid_grant` and `410` arrive as [`Error::Api`].
    pub async fn cli_complete(&self, request_id: &str, code_verifier: &str) -> Result<Tokens> {
        let body = serde_json::json!({ "code_verifier": code_verifier });
        let path = format!("/v1/auth/cli/requests/{request_id}/complete");
        Self::json(self.send(self.post_call(&path, None, body)).await?).await
    }

    pub async fn refresh(&self, refresh_credential: &str) -> Result<Tokens> {
        let body = serde_json::json!({ "refresh_credential": refresh_credential });
        Self::json(
            self.send(self.post_call("/v1/auth/cli/refresh", None, body))
                .await?,
        )
        .await
    }

    /// Revokes the device session identified by the refresh credential.
    pub async fn logout(&self, refresh_credential: &str) -> Result<()> {
        let body = serde_json::json!({ "refresh_credential": refresh_credential });
        self.send(self.post_call("/v1/auth/logout", None, body))
            .await
            .map(|_| ())
    }

    pub async fn me(&self, token: &SecretString) -> Result<Me> {
        Self::json(self.send(self.get_call("/v1/me", &[], token)).await?).await
    }

    // ---- capability snapshot

    pub async fn projects(
        &self,
        token: &SecretString,
        etag: Option<&str>,
    ) -> Result<ProjectsResponse> {
        let mut call = self.get_call("/v1/projects", &[], token);
        if let Some(etag) = etag {
            call.headers.push(("If-None-Match", etag.to_string()));
        }
        let response = self.send(call).await?;
        let header = |name: &str| {
            response
                .headers()
                .get(name)
                .and_then(|v| v.to_str().ok())
                .map(str::to_owned)
        };
        if response.status() == StatusCode::NOT_MODIFIED {
            return Ok(ProjectsResponse::NotModified {
                issued_at: header("x-specio-snapshot-issued-at"),
                expires_at: header("x-specio-snapshot-expires-at"),
            });
        }
        let etag = header("etag");
        Ok(ProjectsResponse::Fresh {
            body: Self::json(response).await?,
            etag,
        })
    }

    // ---- documents

    pub async fn sync(&self, token: &SecretString, project_id: &str) -> Result<SyncManifest> {
        let path = format!("/v1/projects/{project_id}/sync");
        Self::json(self.send(self.get_call(&path, &[], token)).await?).await
    }

    pub async fn document(
        &self,
        token: &SecretString,
        project_id: &str,
        doc_path: &str,
        head_sha: Option<&str>,
    ) -> Result<Document> {
        let path = format!("/v1/projects/{project_id}/docs");
        let mut query = vec![("path", doc_path)];
        if let Some(head) = head_sha {
            query.push(("head_sha", head));
        }
        Self::json(self.send(self.get_call(&path, &query, token)).await?).await
    }

    /// One batch commit. Never retried automatically: the caller decides, reusing the same key.
    pub async fn changes(
        &self,
        token: &SecretString,
        project_id: &str,
        idempotency_key: &str,
        request: &ChangesRequest<'_>,
    ) -> Result<ChangesResult> {
        let path = format!("/v1/projects/{project_id}/changes");
        let body = serde_json::to_value(request).map_err(|e| Error::Other(e.to_string()))?;
        let mut call = self.post_call(&path, Some(token), body);
        call.headers
            .push(("Idempotency-Key", idempotency_key.to_string()));
        Self::json(self.send(call).await?).await
    }
}

fn describe_transport(e: &reqwest::Error) -> String {
    // The URL may contain a path or query but never a token; still, report only the failure kind.
    if e.is_timeout() {
        "the request timed out".into()
    } else if e.is_connect() {
        "cannot connect to the Specio API".into()
    } else {
        "the request to the Specio API failed".into()
    }
}

async fn read_error(
    response: reqwest::Response,
    status: StatusCode,
    request_id: Option<String>,
) -> ApiError {
    let retry_after = response
        .headers()
        .get("retry-after")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.trim().parse::<u64>().ok());
    #[derive(Deserialize)]
    struct Envelope {
        error: Inner,
    }
    #[derive(Deserialize)]
    struct Inner {
        code: String,
        message: String,
        request_id: Option<String>,
        details: Option<serde_json::Value>,
    }
    let bytes = response.bytes().await.unwrap_or_default();
    match serde_json::from_slice::<Envelope>(&bytes) {
        Ok(Envelope { error }) => ApiError {
            status: status.as_u16(),
            code: error.code,
            message: error.message,
            request_id: error.request_id.or(request_id),
            details: error.details,
            retry_after,
        },
        Err(_) => ApiError {
            status: status.as_u16(),
            code: "http_error".into(),
            message: status
                .canonical_reason()
                .unwrap_or("unexpected response")
                .to_string(),
            request_id,
            details: None,
            retry_after,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn api_url_rules() {
        assert!(parse_api_url("https://specio-api.dayatech.workers.dev").is_ok());
        assert!(parse_api_url("http://localhost:8787").is_ok());
        assert!(parse_api_url("http://127.0.0.1:8787").is_ok());
        assert!(parse_api_url("http://example.com").is_err());
        assert!(parse_api_url("https://user:pw@example.com").is_err());
        assert!(parse_api_url("https://example.com/v1").is_err());
        assert!(parse_api_url("ftp://example.com").is_err());
    }
}
