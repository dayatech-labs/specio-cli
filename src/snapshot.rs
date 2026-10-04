//! Capability snapshot cache: identity and project/role/capability data with `ETag` and expiry.
//! It holds no credentials and no document content, and is never used past `expires_at`.
use crate::api::{ProjectSummary, SnapshotBody};
use crate::error::{Error, Result};
use crate::fsx::{self, Visibility};
use serde::{Deserialize, Serialize};
use std::path::Path;
use time::format_description::well_known::Rfc3339;
use time::{Duration, OffsetDateTime};

/// The API issues windows of 5–15 minutes; anything longer is treated as invalid.
pub const MAX_WINDOW: Duration = Duration::minutes(15);

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StoredSnapshot {
    pub user_id: String,
    pub authorization_version: i64,
    pub etag: Option<String>,
    #[serde(with = "time::serde::rfc3339")]
    pub issued_at: OffsetDateTime,
    #[serde(with = "time::serde::rfc3339")]
    pub expires_at: OffsetDateTime,
    pub projects: Vec<ProjectSummary>,
}

pub fn now() -> OffsetDateTime {
    OffsetDateTime::now_utc()
}

pub fn parse_time(raw: &str) -> Option<OffsetDateTime> {
    OffsetDateTime::parse(raw, &Rfc3339).ok()
}

impl StoredSnapshot {
    pub fn from_body(
        body: SnapshotBody,
        etag: Option<String>,
        now: OffsetDateTime,
    ) -> Result<StoredSnapshot> {
        let issued_at = parse_time(&body.issued_at)
            .ok_or_else(|| Error::Other("the API returned an invalid snapshot time".into()))?;
        let declared = parse_time(&body.expires_at)
            .ok_or_else(|| Error::Other("the API returned an invalid snapshot time".into()))?;
        Ok(StoredSnapshot {
            user_id: body.user_id,
            authorization_version: body.authorization_version,
            etag,
            issued_at,
            // Never trust a longer window than the spec allows, whatever the server claims.
            expires_at: declared.min(now + MAX_WINDOW),
            projects: body.projects,
        })
    }

    /// Valid only while strictly before `expires_at`, with at least `margin` left.
    pub fn is_valid_for(&self, now: OffsetDateTime, margin: Duration) -> bool {
        self.expires_at - now > margin
    }

    pub fn is_valid(&self, now: OffsetDateTime) -> bool {
        self.is_valid_for(now, Duration::ZERO)
    }
}

/// A corrupt or unreadable cache is dropped rather than trusted.
pub fn load(path: &Path) -> Result<Option<StoredSnapshot>> {
    let Some(bytes) = fsx::read_optional(path)? else {
        return Ok(None);
    };
    match serde_json::from_slice(&bytes) {
        Ok(snapshot) => Ok(Some(snapshot)),
        Err(_) => {
            fsx::remove_optional(path)?;
            Ok(None)
        }
    }
}

pub fn save(path: &Path, snapshot: &StoredSnapshot) -> Result<()> {
    let bytes = serde_json::to_vec_pretty(snapshot).map_err(|e| Error::Other(e.to_string()))?;
    fsx::atomic_write(path, &bytes, Visibility::Private)
}

pub fn delete(path: &Path) -> Result<()> {
    fsx::remove_optional(path)
}
