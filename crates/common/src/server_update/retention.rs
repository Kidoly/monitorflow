//! How long VictoriaMetrics and VictoriaLogs keep data on the compose
//! deployment (ADR-0069). It is a start flag of those containers, so the
//! updater applies it: the server asks with [`RetentionRequest`], and the
//! updater reports what `.env` says in `status.json`.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use super::backup::check_request;

/// The request in the inbox.
pub const RETENTION_REQUEST_FILE: &str = "retention.json";
/// What the compose file keeps without any setting.
pub const DEFAULT_METRICS: &str = "90d";
pub const DEFAULT_LOGS: &str = "30d";
/// Most days the web interface accepts: ten years.
pub const MAX_DAYS: u32 = 3650;

/// `retention.json`: the new values in days, `None` leaves one alone.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RetentionRequest {
    pub id: String,
    pub metrics_days: Option<u32>,
    pub logs_days: Option<u32>,
    pub requested_by: String,
    pub requested_at: DateTime<Utc>,
}

impl RetentionRequest {
    pub fn parse(bytes: &[u8]) -> Result<Self, String> {
        let request: Self = serde_json::from_slice(bytes).map_err(|e| e.to_string())?;
        check_request(&request.id, &request.requested_by)?;
        for days in [request.metrics_days, request.logs_days]
            .into_iter()
            .flatten()
        {
            if !valid_days(days) {
                return Err(format!("{days} days is out of range"));
            }
        }
        Ok(request)
    }
}

pub fn valid_days(days: u32) -> bool {
    (1..=MAX_DAYS).contains(&days)
}

/// One store's retention.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Kept {
    /// The value of its `-retentionPeriod` flag, like `90d`.
    pub value: String,
    /// `.env` sets it by hand, which wins over the web interface.
    pub locked: bool,
}

/// The retention part of `status.json`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Retention {
    pub metrics: Kept,
    pub logs: Kept,
    /// The updater is applying a change now.
    #[serde(default)]
    pub applying: bool,
    /// Why the last change failed; the old values still apply.
    #[serde(default)]
    pub error: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn request(id: &str, days: Option<u32>) -> Vec<u8> {
        serde_json::to_vec(&RetentionRequest {
            id: id.into(),
            metrics_days: days,
            logs_days: None,
            requested_by: "alice".into(),
            requested_at: Utc::now(),
        })
        .unwrap()
    }

    #[test]
    fn requests_are_checked() {
        let parsed = RetentionRequest::parse(&request("r-1", Some(60))).unwrap();
        assert_eq!(parsed.metrics_days, Some(60));
        assert!(RetentionRequest::parse(&request("r-1", None)).is_ok());
        assert!(RetentionRequest::parse(&request("r 1", None)).is_err());
        assert!(RetentionRequest::parse(&request("r-1", Some(0))).is_err());
        assert!(RetentionRequest::parse(&request("r-1", Some(MAX_DAYS + 1))).is_err());
        assert!(RetentionRequest::parse(b"{}").is_err());
    }
}
