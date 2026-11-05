//! Thresholds, incidents and alert states in PostgreSQL (ADR-0017).

use chrono::{DateTime, Utc};
use serde_json::json;
use sqlx::{PgPool, Postgres, Transaction};
use uuid::Uuid;

use super::rules::{resolve, Row, Rule, Rules};
use super::state::{SeriesState, Transition};
use super::Severity;
use crate::ingest::validate::METRIC_PREFIX;
use crate::sensor_config::{Layer, SensorConfigStore, Spec};

/// One series: server row, full metric name, instance (`''` for none).
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct SeriesKey {
    pub server_id: Uuid,
    pub metric: String,
    pub instance: String,
}

/// The effective thresholds of one server, and how often its checks run.
#[derive(Debug, Clone, PartialEq)]
pub struct ServerRules {
    pub server_id: Uuid,
    /// Threshold and sensor revisions together: either moving reloads.
    pub revision: u64,
    pub rules: Rules,
    pub intervals: CheckIntervals,
}

/// The collection interval of each check of a server, by sensor name; the
/// longest of its instances.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CheckIntervals(std::collections::BTreeMap<String, u64>);

impl CheckIntervals {
    pub fn new(sensors: &[Spec]) -> Self {
        let mut intervals = std::collections::BTreeMap::new();
        for spec in sensors {
            let interval = intervals.entry(spec.name.clone()).or_insert(0);
            *interval = spec.interval_ms.max(*interval);
        }
        Self(intervals)
    }

    /// The interval of the check that writes `metric`, in milliseconds:
    /// the sensor with the longest name that prefixes it
    /// (`monitorflow_disk_data_used_ratio` comes from `disk_data`, not
    /// `disk`), as the web interface matches them.
    pub fn of(&self, metric: &str) -> Option<u64> {
        let short = metric.strip_prefix(METRIC_PREFIX)?;
        self.0
            .iter()
            .filter(|(name, _)| {
                short
                    .strip_prefix(name.as_str())
                    .is_some_and(|rest| rest.starts_with('_'))
            })
            .max_by_key(|(name, _)| name.len())
            .map(|(_, interval)| *interval)
    }
}

/// A series that was pending or firing when the engine stopped.
#[derive(Debug, Clone, PartialEq)]
pub struct Stored {
    pub key: SeriesKey,
    pub state: SeriesState,
    /// The open incident and its peak value.
    pub incident: Option<(Uuid, f64)>,
}

/// What one evaluated sample changes in the database.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum IncidentStep<'a> {
    None,
    /// Opens an incident, or adopts the open one of the series.
    Open {
        severity: Severity,
        rule: &'a Rule,
    },
    /// Records the value, and the severity when it changed.
    Update {
        id: Uuid,
        severity: Severity,
        peak: f64,
    },
    Resolve {
        id: Uuid,
    },
}

#[derive(Debug, Clone, Copy)]
pub struct Step<'a> {
    pub key: &'a SeriesKey,
    /// Sample time, Unix milliseconds, or the server clock without a
    /// sample.
    pub at: i64,
    /// `None` when no sample caused the step; the incident keeps its last
    /// value.
    pub value: Option<f64>,
    pub incident: IncidentStep<'a>,
    /// Recorded in `incident_events` when set.
    pub transition: Option<Transition>,
    /// Why, in the event's details, when not a sample crossing a threshold.
    pub reason: Option<&'static str>,
    /// Written to `alert_states` when set; an OK state deletes the row.
    pub state: Option<&'a SeriesState>,
}

fn time(ms: i64) -> DateTime<Utc> {
    DateTime::from_timestamp_millis(ms).unwrap_or_default()
}

fn non_negative(value: i64) -> u64 {
    u64::try_from(value).unwrap_or(0)
}

#[derive(Debug, Clone)]
pub struct AlertStore {
    pool: PgPool,
}

impl AlertStore {
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }

    /// The thresholds of the agent `peer_id` and the intervals of its
    /// checks, or `None` without a `servers` row or for a removed server
    /// (ADR-0062). The thresholds come with the revision in one statement;
    /// the checks are read after it, so a change in between is newer than
    /// the revision and only causes one more reload.
    pub async fn load_rules(&self, peer_id: &str) -> Result<Option<ServerRules>, sqlx::Error> {
        type Line = (
            Uuid,
            i64,
            Option<bool>,
            Option<String>,
            Option<String>,
            Option<String>,
            Option<f64>,
            Option<f64>,
            Option<i32>,
            Option<f64>,
            Option<bool>,
        );
        let lines: Vec<Line> = sqlx::query_as(
            "SELECT s.id, si.threshold_revision + s.threshold_revision
                          + si.sensor_revision + s.sensor_revision,
                    t.server_id IS NOT NULL, t.metric, t.instance, t.comparison,
                    t.warning, t.critical, t.duration_s, t.hysteresis, t.enabled
             FROM servers s
             JOIN sites si ON si.id = s.site_id
             LEFT JOIN thresholds t ON t.site_id = s.site_id OR t.server_id = s.id
             WHERE s.peer_id = $1 AND s.revoked_at IS NULL",
        )
        .bind(peer_id)
        .fetch_all(&self.pool)
        .await?;
        let Some(&(server_id, revision, ..)) = lines.first() else {
            return Ok(None);
        };
        let rows = lines
            .into_iter()
            .filter_map(|line| {
                let (
                    _,
                    _,
                    server,
                    metric,
                    instance,
                    comparison,
                    warning,
                    critical,
                    duration,
                    hysteresis,
                    enabled,
                ) = line;
                Some(Row {
                    layer: if server? { Layer::Server } else { Layer::Site },
                    rule: Rule {
                        metric: metric?,
                        instance,
                        comparison: comparison?.parse().ok()?,
                        warning,
                        critical,
                        duration_s: u32::try_from(duration?).unwrap_or(0),
                        hysteresis: hysteresis?,
                    },
                    enabled: enabled?,
                })
            })
            .collect();
        let sensors = SensorConfigStore::new(self.pool.clone())
            .load(peer_id)
            .await?
            .map(|resolved| resolved.sensors)
            .unwrap_or_default();
        Ok(Some(ServerRules {
            server_id,
            revision: non_negative(revision),
            rules: resolve(rows),
            intervals: CheckIntervals::new(&sensors),
        }))
    }

    /// Current threshold and sensor revisions of `server_ids`, removed
    /// ones left out, so the engine forgets them.
    pub async fn revisions(&self, server_ids: &[Uuid]) -> Result<Vec<(Uuid, u64)>, sqlx::Error> {
        let rows: Vec<(Uuid, i64)> = sqlx::query_as(
            "SELECT s.id, si.threshold_revision + s.threshold_revision
                          + si.sensor_revision + s.sensor_revision
             FROM servers s JOIN sites si ON si.id = s.site_id
             WHERE s.id = ANY($1) AND s.revoked_at IS NULL",
        )
        .bind(server_ids)
        .fetch_all(&self.pool)
        .await?;
        Ok(rows
            .into_iter()
            .map(|(id, rev)| (id, non_negative(rev)))
            .collect())
    }

    /// Every series that is pending or has an open incident. The severity
    /// comes from the incident, the timers from `alert_states`.
    pub async fn load_states(&self) -> Result<Vec<Stored>, sqlx::Error> {
        self.states(None).await
    }

    /// [`Self::load_states`] of one server, as the engine reads them when
    /// its agent's stream comes to this instance (ADR-0089).
    pub async fn load_server_states(&self, server_id: Uuid) -> Result<Vec<Stored>, sqlx::Error> {
        self.states(Some(server_id)).await
    }

    async fn states(&self, server_id: Option<Uuid>) -> Result<Vec<Stored>, sqlx::Error> {
        type Line = (
            Uuid,
            String,
            String,
            Option<DateTime<Utc>>,
            Option<DateTime<Utc>>,
            Option<DateTime<Utc>>,
            Option<Uuid>,
            Option<String>,
            Option<f64>,
            Option<DateTime<Utc>>,
        );
        let lines: Vec<Line> = sqlx::query_as(
            "SELECT server_id, metric, instance, a.warning_since, a.critical_since,
                    a.last_sample_at, i.id, i.severity, i.peak_value, i.last_value_at
             FROM alert_states a
             -- Offline incidents belong to their own task (ADR-0072).
             FULL JOIN (SELECT * FROM incidents WHERE state = 'open' AND metric <> 'offline') i
                 USING (server_id, metric, instance)
             WHERE $1::uuid IS NULL OR server_id = $1",
        )
        .bind(server_id)
        .fetch_all(&self.pool)
        .await?;
        Ok(lines
            .into_iter()
            .map(|line| {
                let (
                    server_id,
                    metric,
                    instance,
                    warning,
                    critical,
                    sampled,
                    id,
                    severity,
                    peak,
                    valued,
                ) = line;
                let last_at = sampled.max(valued).map(|t| t.timestamp_millis());
                Stored {
                    key: SeriesKey {
                        server_id,
                        metric,
                        instance,
                    },
                    state: SeriesState {
                        firing: severity.and_then(|s| s.parse().ok()),
                        warning_since: warning.map(|t| t.timestamp_millis()),
                        critical_since: critical.map(|t| t.timestamp_millis()),
                        last_at,
                    },
                    incident: id.zip(peak),
                }
            })
            .collect())
    }

    /// Applies `step` in one transaction. Returns the open incident's ID
    /// and peak after an [`IncidentStep::Open`].
    pub async fn apply(&self, step: &Step<'_>) -> Result<Option<(Uuid, f64)>, sqlx::Error> {
        let mut tx = self.pool.begin().await?;
        let (at, value) = (time(step.at), step.value);
        let mut opened = None;
        let incident = match step.incident {
            IncidentStep::None => None,
            IncidentStep::Open { severity, rule } => {
                let (id, peak, inserted) = open(&mut tx, step, severity, rule).await?;
                opened = Some((id, peak));
                // An adopted incident already has its opening event.
                (inserted || !matches!(step.transition, Some(Transition::Opened(_)))).then_some(id)
            }
            IncidentStep::Update { id, severity, peak } => {
                let updated = sqlx::query(
                    "UPDATE incidents SET severity = $2, last_value = $3, last_value_at = $4,
                                          peak_value = $5
                     WHERE id = $1 AND state = 'open'",
                )
                .bind(id)
                .bind(severity.as_str())
                .bind(value)
                .bind(at)
                .bind(peak)
                .execute(&mut *tx)
                .await?;
                // Resolved meanwhile, as by another server instance
                // (ADR-0089): no event for what did not happen.
                (updated.rows_affected() > 0).then_some(id)
            }
            IncidentStep::Resolve { id } => {
                let resolved = sqlx::query(
                    "UPDATE incidents SET state = 'resolved', resolved_at = GREATEST($2, opened_at),
                                          last_value = COALESCE($3, last_value),
                                          last_value_at = GREATEST($2, last_value_at)
                     WHERE id = $1 AND state = 'open'",
                )
                .bind(id)
                .bind(at)
                .bind(value)
                .execute(&mut *tx)
                .await?;
                (resolved.rows_affected() > 0).then_some(id)
            }
        };
        if let (Some(id), Some(transition)) = (incident, step.transition) {
            let details = step
                .reason
                .map_or_else(|| json!({}), |r| json!({ "reason": r }));
            sqlx::query(
                "INSERT INTO incident_events (incident_id, occurred_at, kind, severity, value, details)
                 VALUES ($1, $2, $3, $4, $5, $6)",
            )
            .bind(id)
            .bind(at)
            .bind(transition.kind())
            .bind(transition.severity().map(Severity::as_str))
            .bind(value)
            .bind(details)
            .execute(&mut *tx)
            .await?;
        }
        if let Some(state) = step.state {
            save_state(&mut tx, step.key, state).await?;
        }
        tx.commit().await?;
        Ok(opened)
    }
}

/// Inserts the open incident of the series, or takes the one already open.
async fn open(
    tx: &mut Transaction<'_, Postgres>,
    step: &Step<'_>,
    severity: Severity,
    rule: &Rule,
) -> Result<(Uuid, f64, bool), sqlx::Error> {
    let key = step.key;
    let at = time(step.at);
    let value = step.value.unwrap_or_default();
    let inserted: Option<Uuid> = sqlx::query_scalar(
        "INSERT INTO incidents (server_id, metric, instance, severity, opened_at, last_value,
                                last_value_at, peak_value, threshold)
         VALUES ($1, $2, $3, $4, $5, $6, $5, $6, $7)
         ON CONFLICT (server_id, metric, instance) WHERE state = 'open' DO NOTHING
         RETURNING id",
    )
    .bind(key.server_id)
    .bind(&key.metric)
    .bind(&key.instance)
    .bind(severity.as_str())
    .bind(at)
    .bind(value)
    .bind(rule.snapshot())
    .fetch_optional(&mut **tx)
    .await?;
    if let Some(id) = inserted {
        return Ok((id, value, true));
    }
    // Another writer opened it first; it is committed, so this statement
    // sees it.
    let (id, peak): (Uuid, f64) = sqlx::query_as(
        "UPDATE incidents SET severity = $4, last_value = $5,
                              last_value_at = GREATEST($6, last_value_at),
                              peak_value = CASE WHEN $7 THEN GREATEST(peak_value, $5)
                                                ELSE LEAST(peak_value, $5) END
         WHERE server_id = $1 AND metric = $2 AND instance = $3 AND state = 'open'
         RETURNING id, peak_value",
    )
    .bind(key.server_id)
    .bind(&key.metric)
    .bind(&key.instance)
    .bind(severity.as_str())
    .bind(value)
    .bind(at)
    .bind(rule.comparison.above())
    .fetch_one(&mut **tx)
    .await?;
    Ok((id, peak, false))
}

async fn save_state(
    tx: &mut Transaction<'_, Postgres>,
    key: &SeriesKey,
    state: &SeriesState,
) -> Result<(), sqlx::Error> {
    if state.is_ok() {
        sqlx::query(
            "DELETE FROM alert_states WHERE server_id = $1 AND metric = $2 AND instance = $3",
        )
        .bind(key.server_id)
        .bind(&key.metric)
        .bind(&key.instance)
        .execute(&mut **tx)
        .await?;
        return Ok(());
    }
    sqlx::query(
        "INSERT INTO alert_states (server_id, metric, instance, severity, warning_since,
                                   critical_since, last_sample_at)
         VALUES ($1, $2, $3, $4, $5, $6, $7)
         ON CONFLICT (server_id, metric, instance) DO UPDATE
         SET severity = $4, warning_since = $5, critical_since = $6, last_sample_at = $7,
             updated_at = now()",
    )
    .bind(key.server_id)
    .bind(&key.metric)
    .bind(&key.instance)
    .bind(state.firing.map(Severity::as_str))
    .bind(state.warning_since.map(time))
    .bind(state.critical_since.map(time))
    .bind(time(state.last_at.unwrap_or_default()))
    .execute(&mut **tx)
    .await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{CheckIntervals, Spec};

    #[test]
    fn intervals_come_from_the_check_with_the_longest_matching_name() {
        let mut data = Spec::new("disk", 300_000);
        data.name = "disk_data".into();
        let mut root = Spec::new("disk", 60_000);
        root.instance = "/".into();
        let intervals = CheckIntervals::new(&[
            Spec::new("cpu", 3_600_000),
            Spec::new("disk", 10_000),
            root,
            data,
            Spec::new("diskio", 30_000),
        ]);
        assert_eq!(intervals.of("monitorflow_cpu_usage_ratio"), Some(3_600_000));
        // The longest of the instances of one check.
        assert_eq!(intervals.of("monitorflow_disk_used_ratio"), Some(60_000));
        assert_eq!(
            intervals.of("monitorflow_disk_data_used_ratio"),
            Some(300_000)
        );
        assert_eq!(intervals.of("monitorflow_diskio_busy_ratio"), Some(30_000));
        assert_eq!(intervals.of("monitorflow_memory_available_ratio"), None);
        assert_eq!(intervals.of("monitorflow_cpu"), None);
        assert_eq!(intervals.of("cpu_usage_ratio"), None);
        assert_eq!(intervals.of("offline"), None);
    }

    use super::*;

    #[test]
    fn times_round_trip() {
        assert_eq!(
            time(1_800_000_000_123).timestamp_millis(),
            1_800_000_000_123
        );
        assert_eq!(non_negative(-1), 0);
    }
}
