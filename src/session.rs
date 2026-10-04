//! Device session: login (PKCE), rotating refresh credential, in-memory access token,
//! capability snapshot refresh, logout, and the background-agent tick.
use crate::api::{ProjectsResponse, Tokens};
use crate::credentials::StoredCredential;
use crate::env::Env;
use crate::error::{Error, Result};
use crate::fsx::FileLock;
use crate::snapshot::{self, StoredSnapshot, now};
use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use secrecy::SecretString;
use serde::Serialize;
use std::process::{Command, Stdio};
use std::time::Duration;
use time::Duration as TimeDuration;
use url::Url;

const LOGIN_DEADLINE: Duration = Duration::from_secs(600);
const MAX_POLL_FAILURES: u32 = 3;
/// The agent refreshes this long before expiry so `list` never meets an expired cache.
const AGENT_REFRESH_AHEAD: TimeDuration = TimeDuration::seconds(150);
const AGENT_JITTER_MAX_SECONDS: u64 = 10;

/// An access token valid for ~15 minutes. It exists only in memory.
pub struct Authed {
    pub token: SecretString,
}

impl std::fmt::Debug for Authed {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Authed([redacted])")
    }
}

#[derive(Debug, Serialize)]
pub struct LoginReport {
    pub user_id: String,
    pub email: Option<String>,
    pub name: Option<String>,
    pub projects: usize,
    pub agent_installed: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub agent_warning: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct LogoutReport {
    pub was_logged_in: bool,
}

#[derive(Debug, PartialEq, Eq)]
pub enum AgentOutcome {
    LoggedOut,
    StillFresh,
    Refreshed,
}

pub struct Session<'a> {
    env: &'a Env,
}

fn random_bytes<const N: usize>() -> Result<[u8; N]> {
    let mut buf = [0u8; N];
    getrandom::fill(&mut buf).map_err(|e| Error::Other(format!("no secure random source: {e}")))?;
    Ok(buf)
}

/// PKCE S256: a 43-character verifier from 32 random bytes and its challenge.
pub fn pkce_pair() -> Result<(String, String)> {
    let verifier = URL_SAFE_NO_PAD.encode(random_bytes::<32>()?);
    let challenge = URL_SAFE_NO_PAD.encode(crate::hashing::sha256_bytes(verifier.as_bytes()));
    Ok((verifier, challenge))
}

fn device_name() -> String {
    let raw = std::env::var("HOSTNAME")
        .or_else(|_| std::env::var("COMPUTERNAME"))
        .ok()
        .or_else(|| {
            Command::new("hostname")
                .output()
                .ok()
                .filter(|o| o.status.success())
                .map(|o| String::from_utf8_lossy(&o.stdout).into_owned())
        })
        .unwrap_or_default();
    let cleaned: String = raw.chars().filter(|c| !c.is_control()).collect();
    let cleaned = cleaned.trim();
    let name = if cleaned.is_empty() {
        format!("specio on {}", std::env::consts::OS)
    } else {
        cleaned.to_string()
    };
    name.chars().take(64).collect()
}

/// Only open a verification URL on the API's own origin; never one the API cannot vouch for.
fn check_verification_url(api_url: &Url, verification_url: &str) -> Result<Url> {
    let url = Url::parse(verification_url)
        .map_err(|_| Error::Other("the API returned an invalid verification URL".into()))?;
    if url.origin() != api_url.origin() {
        return Err(Error::Other(
            "the verification URL is not on the API origin; refusing to open it".into(),
        ));
    }
    Ok(url)
}

fn open_in_browser(url: &Url) -> bool {
    let mut command = if cfg!(target_os = "macos") {
        let mut c = Command::new("open");
        c.arg(url.as_str());
        c
    } else if cfg!(windows) {
        let mut c = Command::new("rundll32");
        c.args(["url.dll,FileProtocolHandler", url.as_str()]);
        c
    } else {
        let mut c = Command::new("xdg-open");
        c.arg(url.as_str());
        c
    };
    command
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .is_ok()
}

impl<'a> Session<'a> {
    pub fn new(env: &'a Env) -> Self {
        Session { env }
    }

    pub fn credential(&self) -> Result<StoredCredential> {
        self.env
            .creds
            .load(&self.env.account())?
            .ok_or(Error::NotLoggedIn)
    }

    /// Remove every trace of the device session from this machine: credential, snapshot, agent.
    pub fn clear_local(&self) -> Result<()> {
        let credential = self.env.creds.delete(&self.env.account());
        let snapshot = snapshot::delete(&self.env.snapshot_path());
        // A missing scheduler is not a failure to sign out.
        let _ = self.env.scheduler.uninstall();
        credential.and(snapshot)
    }

    // ---------------------------------------------------------------- login

    pub async fn login(&self) -> Result<LoginReport> {
        let env = self.env;
        let (verifier, challenge) = pkce_pair()?;
        let request = env.client.cli_request(&challenge, &device_name()).await?;
        let url = check_verification_url(&env.api_url, &request.verification_url)?;

        eprintln!("To sign in, approve this device in your browser:\n  {url}");
        if env.open_browser && !open_in_browser(&url) {
            eprintln!("(could not open a browser automatically; open the URL above)");
        }
        eprintln!("Waiting for approval (the request expires in 10 minutes)...");

        let tokens = self
            .poll_for_tokens(
                &request.request_id,
                &verifier,
                Duration::from_secs(request.poll_after_seconds),
            )
            .await?;
        self.finish_login(tokens).await
    }

    async fn poll_for_tokens(
        &self,
        request_id: &str,
        verifier: &str,
        mut interval: Duration,
    ) -> Result<Tokens> {
        let deadline = tokio::time::Instant::now() + LOGIN_DEADLINE;
        let mut failures = 0;
        loop {
            tokio::time::sleep(interval).await;
            if tokio::time::Instant::now() >= deadline {
                return Err(Error::invalid(
                    "the login request expired; run `specio login` again",
                ));
            }
            match self.env.client.cli_complete(request_id, verifier).await {
                Ok(tokens) => return Ok(tokens),
                Err(Error::Api(e)) if e.code == "authorization_pending" => failures = 0,
                Err(Error::Api(e)) if e.status == 429 => {
                    interval = interval.max(Duration::from_secs(e.retry_after.unwrap_or(5)));
                    failures = 0;
                }
                Err(Error::Api(e))
                    if matches!(e.status, 404 | 410) || e.code == "invalid_grant" =>
                {
                    return Err(Error::invalid(
                        "the login request is no longer valid; run `specio login` again",
                    ));
                }
                Err(Error::Network(message)) => {
                    failures += 1;
                    if failures >= MAX_POLL_FAILURES {
                        return Err(Error::Network(message));
                    }
                }
                Err(e) => return Err(e),
            }
        }
    }

    async fn finish_login(&self, tokens: Tokens) -> Result<LoginReport> {
        let env = self.env;
        let _lock = FileLock::exclusive(&env.auth_lock_path())?;

        // Replace any earlier session on this device: revoke it (best effort) and drop its cache,
        // so an account switch cannot leave the previous user's snapshot behind.
        let previous = env.creds.load(&env.account()).ok().flatten();
        if let Some(previous) = previous {
            let _ = env.client.logout(&previous.refresh_credential).await;
        }
        snapshot::delete(&env.snapshot_path())?;

        // Store the credential before anything else can fail so the new device session is never orphaned.
        env.creds.save(
            &env.account(),
            &StoredCredential {
                user_id: None,
                refresh_credential: tokens.refresh_credential,
            },
        )?;
        let token = SecretString::from(tokens.access_token);

        let snapshot = self
            .refresh_snapshot(&token, true, TimeDuration::ZERO)
            .await?;
        self.remember_user(&snapshot.user_id)?;
        let me = env.client.me(&token).await.ok();

        let (agent_installed, agent_warning) = match env.scheduler.install() {
            Ok(()) => (true, None),
            Err(e) => (
                false,
                Some(format!(
                    "background refresh is unavailable ({e}); commands will refresh in the foreground"
                )),
            ),
        };
        Ok(LoginReport {
            user_id: snapshot.user_id,
            email: me.as_ref().map(|m| m.email.clone()),
            name: me.and_then(|m| m.name),
            projects: snapshot.projects.len(),
            agent_installed,
            agent_warning,
        })
    }

    fn remember_user(&self, user_id: &str) -> Result<()> {
        let mut credential = self.credential()?;
        if credential.user_id.as_deref() != Some(user_id) {
            credential.user_id = Some(user_id.to_string());
            self.env.creds.save(&self.env.account(), &credential)?;
        }
        Ok(())
    }

    // --------------------------------------------------------------- logout

    /// Revokes this device on the server, then clears local state either way.
    pub async fn logout(&self) -> Result<LogoutReport> {
        let _lock = FileLock::exclusive(&self.env.auth_lock_path())?;
        let credential = self.env.creds.load(&self.env.account()).ok().flatten();
        let revoke = match &credential {
            Some(c) => self.env.client.logout(&c.refresh_credential).await,
            None => Ok(()),
        };
        self.clear_local()?;
        match revoke {
            Ok(()) => Ok(LogoutReport {
                was_logged_in: credential.is_some(),
            }),
            // The credential was already invalid server-side: nothing is left to revoke.
            Err(Error::Api(e)) if e.status == 401 => Ok(LogoutReport {
                was_logged_in: credential.is_some(),
            }),
            Err(e) => Err(Error::Network(format!(
                "signed out on this machine, but the device could not be revoked on the server ({e}); revoke it from an admin account if needed"
            ))),
        }
    }

    // ----------------------------------------------------------- credentials

    /// Rotate the refresh credential into a fresh access token. Cross-process safe: the rotation
    /// and the write of the new credential happen under one lock, so two processes never present
    /// the same credential. A `401` removes all local authentication state.
    pub async fn authenticate(&self) -> Result<Authed> {
        self.authenticate_with(TimeDuration::ZERO).await
    }

    async fn authenticate_with(&self, snapshot_margin: TimeDuration) -> Result<Authed> {
        let env = self.env;
        let _lock = FileLock::exclusive(&env.auth_lock_path())?;
        let credential = self.credential()?;

        let tokens = match env.client.refresh(&credential.refresh_credential).await {
            Ok(tokens) => tokens,
            Err(Error::Api(e)) if e.status == 401 => {
                self.clear_local()?;
                return Err(Error::SessionExpired);
            }
            Err(e) => return Err(e),
        };
        let rotated = StoredCredential {
            user_id: credential.user_id.clone(),
            refresh_credential: tokens.refresh_credential,
        };
        if let Err(first) = env.creds.save(&env.account(), &rotated) {
            // The server already rotated: retry once, because losing the new credential forces a new login.
            env.creds
                .save(&env.account(), &rotated)
                .map_err(|_| first)?;
        }
        let token = SecretString::from(tokens.access_token);

        let snapshot = self
            .refresh_snapshot(&token, false, snapshot_margin)
            .await?;
        if credential
            .user_id
            .as_deref()
            .is_some_and(|u| u != snapshot.user_id)
        {
            // The cache does not belong to this credential: do not trust either.
            self.clear_local()?;
            return Err(Error::SessionExpired);
        }
        if credential.user_id.is_none() {
            self.remember_user(&snapshot.user_id)?;
        }
        Ok(Authed { token })
    }

    // -------------------------------------------------------------- snapshot

    /// Refresh the cache unless it is still valid for `margin`. Sends `If-None-Match` when it
    /// has an `ETag`; a `304` only extends the window when its headers are valid.
    async fn refresh_snapshot(
        &self,
        token: &SecretString,
        force: bool,
        margin: TimeDuration,
    ) -> Result<StoredSnapshot> {
        let path = self.env.snapshot_path();
        let current = snapshot::load(&path)?;
        if !force
            && let Some(s) = &current
            && s.is_valid_for(now(), margin)
        {
            return Ok(s.clone());
        }
        let mut etag = if force {
            None
        } else {
            current.as_ref().and_then(|s| s.etag.clone())
        };
        loop {
            match self.env.client.projects(token, etag.as_deref()).await? {
                ProjectsResponse::Fresh { body, etag } => {
                    let fresh = StoredSnapshot::from_body(body, etag, now())?;
                    snapshot::save(&path, &fresh)?;
                    return Ok(fresh);
                }
                ProjectsResponse::NotModified {
                    issued_at,
                    expires_at,
                } => {
                    let window = issued_at
                        .as_deref()
                        .and_then(snapshot::parse_time)
                        .zip(expires_at.as_deref().and_then(snapshot::parse_time));
                    if let (Some(cached), Some((issued, expires))) = (&current, window)
                        && expires > issued
                        && expires - issued <= snapshot::MAX_WINDOW
                        && expires > now()
                    {
                        let mut renewed = cached.clone();
                        renewed.issued_at = issued;
                        renewed.expires_at = expires;
                        snapshot::save(&path, &renewed)?;
                        return Ok(renewed);
                    }
                    // Missing or implausible headers: do not guess a new expiry, fetch in full.
                    etag = None;
                }
            }
        }
    }

    /// A snapshot valid right now, refreshing in the foreground when it expired or is missing.
    /// An expired cache is never returned.
    pub async fn snapshot(&self) -> Result<StoredSnapshot> {
        let credential = self.credential()?;
        let path = self.env.snapshot_path();
        if let Some(cached) = snapshot::load(&path)? {
            let mine = credential
                .user_id
                .as_deref()
                .is_none_or(|u| u == cached.user_id);
            if mine && cached.is_valid(now()) {
                return Ok(cached);
            }
            if !mine {
                snapshot::delete(&path)?;
            }
        }
        self.authenticate().await?;
        snapshot::load(&path)?
            .filter(|s| s.is_valid(now()))
            .ok_or_else(|| Error::Other("could not refresh the capability snapshot".into()))
    }

    /// After a `403` the role may have changed: drop the snapshot and refresh once. The failed
    /// request is never retried; this only keeps `list`/`init` truthful.
    pub async fn after_forbidden(&self, auth: &Authed) {
        let _ = snapshot::delete(&self.env.snapshot_path());
        if let Ok(_lock) = FileLock::exclusive(&self.env.auth_lock_path()) {
            let _ = self
                .refresh_snapshot(&auth.token, true, TimeDuration::ZERO)
                .await;
        }
    }

    // ----------------------------------------------------------------- agent

    /// One scheduled tick. Cheap and local until the snapshot is near expiry.
    pub async fn agent_tick(&self, jitter: bool) -> Result<AgentOutcome> {
        if self.env.creds.load(&self.env.account())?.is_none() {
            let _ = self.env.scheduler.uninstall();
            return Ok(AgentOutcome::LoggedOut);
        }
        if let Some(s) = snapshot::load(&self.env.snapshot_path())?
            && s.is_valid_for(now(), AGENT_REFRESH_AHEAD)
        {
            return Ok(AgentOutcome::StillFresh);
        }
        if jitter {
            let [byte] = random_bytes::<1>()?;
            tokio::time::sleep(Duration::from_secs(
                u64::from(byte) % (AGENT_JITTER_MAX_SECONDS + 1),
            ))
            .await;
        }
        // Refreshing before the margin lapses is the point; another process may have done it meanwhile.
        match self.authenticate_with(AGENT_REFRESH_AHEAD).await {
            Ok(_) => Ok(AgentOutcome::Refreshed),
            Err(Error::SessionExpired | Error::NotLoggedIn) => Ok(AgentOutcome::LoggedOut),
            Err(e) => Err(e),
        }
    }
}
