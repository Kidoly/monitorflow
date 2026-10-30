//! Thresholds, incidents and alert states in PostgreSQL (ADR-0017).

use chrono::{DateTime, Utc};
use serde_json::json;
use sqlx::{PgPool, Postgres, Transaction};
use uuid::Uuid;

use super::rules::{resolve, Row, Rule, Rules};
use super::state::{SeriesState, Transition};
use super::Severity;
use crate::sensor_config::Layer;

/// One series: server row, full metric name, instance (`''` for none).
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct SeriesKey {
    pub server_id: Uuid,
    pub metric: String,
    pub instance: String,
}

/// The effective thresholds of one server.
#[derive(Debug, Clone, PartialEq)]
pub struct ServerRules {
    pub server_id: Uuid,
    pub revision: u64,
    pub rules: Rules,
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

    /// The thresholds of the agent `peer_id`, or `None` without a
    /// `servers` row. One statement, so the revision matches the rows.
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
            "SELECT s.id, si.threshold_revision + s.threshold_revision,
                    t.server_id IS NOT NULL, t.metric, t.instance, t.comparison,
                    t.warning, t.critical, t.duration_s, t.hysteresis, t.enabled
             FROM servers s
             JOIN sites si ON si.id = s.site_id
             LEFT JOIN thresholds t ON t.site_id = s.site_id OR t.server_id = s.id
             WHERE s.peer_id = $1",
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
        Ok(Some(ServerRules {
            server_id,
            revision: non_negative(revision),
            rules: resolve(rows),
        }))
    }

    /// Current threshold revisions of `server_ids`.
    pub async fn revisions(&self, server_ids: &[Uuid]) -> Result<Vec<(Uuid, u64)>, sqlx::Error> {
        let rows: Vec<(Uuid, i64)> = sqlx::query_as(
            "SELECT s.id, si.threshold_revision + s.threshold_revision
             FROM servers s JOIN sites si ON si.id = s.site_id
             WHERE s.id = ANY($1)",
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
             -- Offline incidents belong to their own task (ADR-0057).
             FULL JOIN (SELECT * FROM incidents WHERE state = 'open' AND metric <> 'offline') i
                 USING (server_id, metric, instance)",
        )
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
                sqlx::query(
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
                Some(id)
            }
            IncidentStep::Resolve { id } => {
                sqlx::query(
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
                Some(id)
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
