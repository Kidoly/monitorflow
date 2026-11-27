//! The `major_upgrade` part of package reports (ADR-0182): the latest one
//! per server in `server_major_upgrades`, bounded, as the API returns it.

use chrono::{DateTime, TimeZone, Utc};
use monitorflow_common::major_upgrade::Phase;
use monitorflow_common::proto::v1::{MajorUpgradeProgress, MajorUpgradeReport, UpgradeCheck};
use serde::{Deserialize, Serialize};
use sqlx::PgExecutor;
use uuid::Uuid;

/// Longest text kept from an agent.
const MAX_TEXT: usize = 512;
/// Longest leapp report kept, as the agent bounds it.
const MAX_REPORT: usize = 64 << 10;
const MAX_CHECKS: usize = 16;
const MAX_ITEMS: usize = 50;

fn bounded(text: &str) -> String {
    text.chars().take(MAX_TEXT).collect()
}

/// One check before an upgrade.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
#[schema(as = MajorUpgradeCheck)]
pub struct Check {
    /// `allowed`, `agent_version` and `mirror` from the central server;
    /// `up_to_date`, `held_packages`, `broken_packages`, `free_space`,
    /// `reboot_pending`, `sources`, `release_offered` (Ubuntu), `leapp`
    /// (RHEL, Rocky Linux, AlmaLinux) and `third_party_sources` from the
    /// agent.
    pub name: String,
    /// `ok`, `warning` (never blocks) or `failed`.
    pub state: String,
    /// What was found, in English.
    pub detail: String,
    /// Held packages, third-party entries and the like.
    #[serde(default)]
    pub items: Vec<String>,
}

impl Check {
    pub fn new(name: &str, state: &str, detail: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            state: state.into(),
            detail: detail.into(),
            items: Vec::new(),
        }
    }

    fn from_proto(check: &UpgradeCheck) -> Self {
        Self {
            name: bounded(&check.name),
            state: bounded(&check.state),
            detail: bounded(&check.detail),
            items: check
                .items
                .iter()
                .take(MAX_ITEMS)
                .map(|i| bounded(i))
                .collect(),
        }
    }
}

/// The last major upgrade of a server, as its agent tells it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
#[schema(as = MajorUpgradeProgress)]
pub struct Progress {
    pub command_id: String,
    pub from: String,
    pub to: String,
    /// `starting`, `preupgrade` (leapp), `sources`, `refreshing`,
    /// `upgrading`, `full_upgrade`, `rebooting`, `done` or `failed`.
    pub phase: String,
    pub error: Option<String>,
    /// A failure before the package step put the sources back.
    pub sources_restored: bool,
    /// Third-party entries switched off and left off.
    pub disabled_sources: Vec<String>,
    /// leapp's report as it wrote it, inhibitors included; `null` outside
    /// the RHEL family.
    #[serde(default)]
    pub report: Option<String>,
    /// `leapp preupgrade` alone (`leapp_preupgrade`), not an upgrade:
    /// `done` means leapp found no inhibitor; the release did not change.
    #[serde(default)]
    pub preupgrade_only: bool,
    pub started_at: Option<DateTime<Utc>>,
    pub updated_at: Option<DateTime<Utc>>,
}

impl Progress {
    fn from_proto(p: &MajorUpgradeProgress) -> Self {
        let at = |ms: i64| {
            (ms > 0)
                .then(|| Utc.timestamp_millis_opt(ms).single())
                .flatten()
        };
        Self {
            command_id: bounded(&p.command_id),
            from: bounded(&p.from),
            to: bounded(&p.to),
            phase: Phase::parse(&p.phase)
                .map_or("failed", Phase::as_str)
                .to_owned(),
            error: (!p.error.is_empty()).then(|| bounded(&p.error)),
            sources_restored: p.sources_restored,
            disabled_sources: p
                .disabled_sources
                .iter()
                .take(MAX_ITEMS)
                .map(|s| bounded(s))
                .collect(),
            report: (!p.report.is_empty()).then(|| p.report.chars().take(MAX_REPORT).collect()),
            preupgrade_only: p.preupgrade_only,
            started_at: at(p.started_at_unix_ms),
            updated_at: at(p.updated_at_unix_ms),
        }
    }

    pub fn phase(&self) -> Option<Phase> {
        Phase::parse(&self.phase)
    }
}

/// What `server_major_upgrades.report` holds.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Stored {
    pub release: String,
    pub codename: String,
    pub target: String,
    pub target_codename: String,
    pub checks: Vec<Check>,
    pub progress: Option<Progress>,
}

impl Stored {
    pub fn from_report(report: &MajorUpgradeReport) -> Self {
        Self {
            release: bounded(&report.release),
            codename: bounded(&report.codename),
            target: bounded(&report.target),
            target_codename: bounded(&report.target_codename),
            checks: report
                .checks
                .iter()
                .take(MAX_CHECKS)
                .map(Check::from_proto)
                .collect(),
            progress: report.progress.as_ref().map(Progress::from_proto),
        }
    }
}

/// Stores `report` for `server`, or forgets the previous one when the
/// agent sent none (another family, a release without a next one, or an
/// agent before ADR-0182).
pub async fn store(
    db: impl PgExecutor<'_>,
    server: Uuid,
    report: Option<&MajorUpgradeReport>,
) -> Result<(), sqlx::Error> {
    match report {
        Some(report) => {
            sqlx::query(
                "INSERT INTO server_major_upgrades (server_id, report) VALUES ($1, $2)
                 ON CONFLICT (server_id) DO UPDATE SET report = $2, reported_at = now()",
            )
            .bind(server)
            .bind(sqlx::types::Json(Stored::from_report(report)))
            .execute(db)
            .await?;
        }
        None => {
            sqlx::query("DELETE FROM server_major_upgrades WHERE server_id = $1")
                .bind(server)
                .execute(db)
                .await?;
        }
    }
    Ok(())
}

/// The open incidents of a server, as `metric instance` keys: the checks
/// failing on it, which a major upgrade must leave as they were.
pub async fn failing_checks(
    db: impl PgExecutor<'_>,
    server: Uuid,
) -> Result<Vec<String>, sqlx::Error> {
    sqlx::query_scalar(
        "SELECT metric || CASE WHEN instance = '' THEN '' ELSE ' (' || instance || ')' END
         FROM incidents WHERE server_id = $1 AND state = 'open' ORDER BY 1",
    )
    .bind(server)
    .fetch_all(db)
    .await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reports_are_bounded_and_phases_checked() {
        let report = MajorUpgradeReport {
            release: "debian 12".into(),
            target: "debian 13".into(),
            checks: (0..40)
                .map(|i| UpgradeCheck {
                    name: format!("c{i}"),
                    state: "ok".into(),
                    detail: "x".repeat(2000),
                    items: vec!["i".into(); 100],
                })
                .collect(),
            progress: Some(MajorUpgradeProgress {
                command_id: "c".into(),
                phase: "weird".into(),
                started_at_unix_ms: 1_700_000_000_000,
                report: "r".repeat(100_000),
                ..MajorUpgradeProgress::default()
            }),
            ..MajorUpgradeReport::default()
        };
        let stored = Stored::from_report(&report);
        assert_eq!(stored.checks.len(), MAX_CHECKS);
        assert_eq!(stored.checks[0].detail.len(), MAX_TEXT);
        assert_eq!(stored.checks[0].items.len(), MAX_ITEMS);
        let progress = stored.progress.unwrap();
        assert_eq!(progress.phase, "failed", "an unknown phase is a failure");
        assert!(progress.started_at.is_some() && progress.updated_at.is_none());
        assert_eq!(progress.error, None);
        assert_eq!(progress.report.unwrap().len(), MAX_REPORT);
    }
}
