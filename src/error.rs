//! One error type for the whole CLI. Each variant maps to a documented exit code.
use std::fmt;

pub type Result<T> = std::result::Result<T, Error>;

/// Exit codes shared by every command (documented in `--help`).
pub mod exit {
    pub const OK: u8 = 0;
    pub const FAILURE: u8 = 1;
    /// 2 is reserved for usage errors reported by Clap.
    pub const AUTH: u8 = 3;
    pub const UNAVAILABLE: u8 = 4;
    pub const CONFLICT: u8 = 5;
    pub const DENIED: u8 = 6;
    pub const INVALID: u8 = 7;
}

/// Error body returned by the API: `{ error: { code, message, request_id, details? } }`.
#[derive(Debug, Clone)]
pub struct ApiError {
    pub status: u16,
    pub code: String,
    pub message: String,
    pub request_id: Option<String>,
    pub details: Option<serde_json::Value>,
    pub retry_after: Option<u64>,
}

impl fmt::Display for ApiError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} ({}, HTTP {})", self.message, self.code, self.status)?;
        if let Some(id) = &self.request_id {
            write!(f, " [request {id}]")?;
        }
        Ok(())
    }
}

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("not logged in; run `speq login`")]
    NotLoggedIn,
    #[error("the session is no longer valid; run `speq login` again")]
    SessionExpired,
    #[error("network error: {0}")]
    Network(String),
    #[error("{0}")]
    Api(Box<ApiError>),
    #[error("{0}")]
    Conflict(String),
    #[error("{0}")]
    Invalid(String),
    #[error("not a Speq workspace (no .speq/config.toml found); run `speq init <repo>`")]
    NotInitialized,
    #[error("{context}: {source}")]
    Io {
        context: String,
        #[source]
        source: std::io::Error,
    },
    #[error("credential store: {0}")]
    CredentialStore(String),
    #[error("{0}")]
    Other(String),
}

impl Error {
    pub fn exit_code(&self) -> u8 {
        match self {
            Error::NotLoggedIn | Error::SessionExpired => exit::AUTH,
            Error::Network(_) => exit::UNAVAILABLE,
            Error::Api(e) => match e.status {
                401 => exit::AUTH,
                403 => exit::DENIED,
                404 | 400 | 422 => exit::INVALID,
                409 => exit::CONFLICT,
                429 | 500..=599 => exit::UNAVAILABLE,
                _ => exit::FAILURE,
            },
            Error::Conflict(_) => exit::CONFLICT,
            Error::Invalid(_) | Error::NotInitialized => exit::INVALID,
            Error::Io { .. } | Error::CredentialStore(_) | Error::Other(_) => exit::FAILURE,
        }
    }

    pub fn code(&self) -> &str {
        match self {
            Error::NotLoggedIn => "not_logged_in",
            Error::SessionExpired => "session_expired",
            Error::Network(_) => "network",
            Error::Api(e) => &e.code,
            Error::Conflict(_) => "conflict",
            Error::Invalid(_) => "invalid",
            Error::NotInitialized => "not_initialized",
            Error::Io { .. } => "io",
            Error::CredentialStore(_) => "credential_store",
            Error::Other(_) => "error",
        }
    }

    pub fn request_id(&self) -> Option<&str> {
        match self {
            Error::Api(e) => e.request_id.as_deref(),
            _ => None,
        }
    }

    pub fn invalid(message: impl Into<String>) -> Self {
        Error::Invalid(message.into())
    }

    pub fn conflict(message: impl Into<String>) -> Self {
        Error::Conflict(message.into())
    }

    pub fn api_status(&self) -> Option<u16> {
        match self {
            Error::Api(e) => Some(e.status),
            _ => None,
        }
    }
}

/// Attach context to `std::io` errors: `fs::read(p).ctx("read config")`.
pub trait IoContext<T> {
    fn ctx(self, context: impl Into<String>) -> Result<T>;
}

impl<T> IoContext<T> for std::io::Result<T> {
    fn ctx(self, context: impl Into<String>) -> Result<T> {
        self.map_err(|source| Error::Io {
            context: context.into(),
            source,
        })
    }
}
