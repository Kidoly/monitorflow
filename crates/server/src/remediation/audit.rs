//! Remediation incidents and their append-only, hash-chained history
//! (ADR-0042).
//!
//! An incident's events are numbered from 1. Each stores the SHA-256 of
//! the previous event's hash followed by its canonical content, which
//! PostgreSQL renders (`remediation_event_canonical`), so a changed,
//! removed or inserted row breaks the chain. [`append`] takes the
//! incident's row lock, so concurrent appends get consecutive numbers;
//! the database trigger refuses any other number and computes the hash.
//! [`verify`] recomputes every hash here, independently of the trigger.
//!
//! Payloads must already be redacted: they are shown in the interface
//! and kept forever.

use std::time::Duration;

use chrono::{DateTime, Utc};
use ring::digest::{Context, SHA256};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sqlx::{PgExecutor, PgPool, Postgres, Transaction};
use uuid::Uuid;

use super::risk::Decision;

/// Largest payload accepted, as JSON text.
pub const MAX_PAYLOAD_BYTES: usize = 1024 * 1024;

/// `prev_hash` of an incident's first event.
pub const GENESIS: [u8; 32] = [0; 32];

#[derive(Debug, thiserror::Error)]
pub enum AuditError {
    #[error("remediation incident not found")]
    NotFound,
    #[error("event payload is larger than {MAX_PAYLOAD_BYTES} bytes")]
    PayloadTooLarge,
    #[error("invalid incident: {0}")]
    Invalid(&'static str),
    #[error(transparent)]
    Database(#[from] sqlx::Error),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, sqlx::Type)]
#[serde(rename_all = "snake_case")]
#[sqlx(type_name = "text", rename_all = "snake_case")]
pub enum IncidentState {
    Open,
    Running,
    AwaitingApproval,
    Resolved,
    Escalated,
    Failed,
}

impl IncidentState {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Open => "open",
            Self::Running => "running",
            Self::AwaitingApproval => "awaiting_approval",
            Self::Resolved => "resolved",
            Self::Escalated => "escalated",
            Self::Failed => "failed",
        }
    }

    pub fn is_closed(self) -> bool {
        matches!(self, Self::Resolved | Self::Escalated | Self::Failed)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, sqlx::Type)]
#[serde(rename_all = "snake_case")]
#[sqlx(type_name = "text", rename_all = "snake_case")]
pub enum EventKind {
    /// The redacted context sent to the AI.
    ContextSent,
    /// A diagnosis and possibly a command from the AI.
    AiProposal,
    /// The risk tier the rules gave a command.
    Classification,
    Approval,
    Rejection,
    /// An approver changed the proposed command.
    Edit,
    CommandQueued,
    /// Exit code and output of a command.
    CommandResult,
    /// A snapshot taken (or not) before a change.
    Snapshot,
    /// Handed over to a human.
    Escalation,
    StateChange,
    Note,
}

/// Who caused an event; stored as `actor_kind` and `actor_id`, with the
/// kinds of `audit_log`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Actor {
    User(Uuid),
    System,
    /// The provider and model, like `claude/claude-opus-5-5`.
    Ai(String),
    /// A peer ID, like the agent that ran a command.
    Peer(String),
}

impl Actor {
    fn columns(&self) -> (&'static str, String) {
        match self {
            Self::User(id) => ("user", id.to_string()),
            Self::System => ("system", "server".to_owned()),
            Self::Ai(model) => ("ai", model.clone()),
            Self::Peer(peer) => ("peer", peer.clone()),
        }
    }
}

/// An event before it is appended. Build it with the constructors, which
/// fix each kind's payload fields.
#[derive(Debug, Clone, PartialEq)]
pub struct NewEvent {
    pub kind: EventKind,
    pub actor: Actor,
    pub payload: Value,
}

impl NewEvent {
    fn new(kind: EventKind, actor: Actor, payload: Value) -> Self {
        Self {
            kind,
            actor,
            payload,
        }
    }

    /// `context` exactly as sent to `model`, after redaction.
    pub fn context_sent(model: &str, context: Value) -> Self {
        Self::new(
            EventKind::ContextSent,
            Actor::System,
            json!({ "model": model, "context": context }),
        )
    }

    /// The AI's diagnosis and reasoning, and the command it proposes, if
    /// any. Untrusted text.
    pub fn ai_proposal(
        model: &str,
        diagnosis: &str,
        reasoning: &str,
        command: Option<&str>,
    ) -> Self {
        Self::new(
            EventKind::AiProposal,
            Actor::Ai(model.to_owned()),
            json!({ "diagnosis": diagnosis, "reasoning": reasoning, "command": command }),
        )
    }

    /// The AI's decision to stop, `outcome` being `resolved` or
    /// `escalate`. Untrusted text.
    pub fn ai_stop(model: &str, diagnosis: &str, outcome: &str, reason: &str) -> Self {
        Self::new(
            EventKind::AiProposal,
            Actor::Ai(model.to_owned()),
            json!({ "diagnosis": diagnosis, "command": null, "stop": outcome, "reason": reason }),
        )
    }

    /// The tier the rules gave `command`, the rule that decided it and
    /// why (ADR-0041).
    pub fn classification(command: &str, decision: &Decision) -> Self {
        Self::new(
            EventKind::Classification,
            Actor::System,
            json!({
                "command": command,
                "tier": decision.tier,
                "rule": decision.rule,
                "reason": decision.reason,
            }),
        )
    }

    /// `actor` approved `command`; [`Actor::System`] for a tier that runs
    /// without approval.
    pub fn approval(actor: Actor, command: &str) -> Self {
        Self::new(EventKind::Approval, actor, json!({ "command": command }))
    }

    pub fn rejection(user: Uuid, command: &str, reason: Option<&str>) -> Self {
        Self::new(
            EventKind::Rejection,
            Actor::User(user),
            json!({ "command": command, "reason": reason }),
        )
    }

    pub fn edit(user: Uuid, original: &str, edited: &str) -> Self {
        Self::new(
            EventKind::Edit,
            Actor::User(user),
            json!({ "original": original, "edited": edited }),
        )
    }

    /// The exact `command` queued as row `command_id` of `commands`.
    pub fn command_queued(command_id: Uuid, command: &str) -> Self {
        Self::new(
            EventKind::CommandQueued,
            Actor::System,
            json!({ "command_id": command_id, "command": command }),
        )
    }

    /// What `peer` reported for `command_id`; `exit_code` is `None` when
    /// the command did not run to completion, `error` says why.
    pub fn command_result(
        peer: &str,
        command_id: Uuid,
        exit_code: Option<i32>,
        stdout: &str,
        stderr: &str,
        error: Option<&str>,
        duration: Duration,
    ) -> Self {
        Self::new(
            EventKind::CommandResult,
            Actor::Peer(peer.to_owned()),
            json!({
                "command_id": command_id,
                "exit_code": exit_code,
                "stdout": stdout,
                "stderr": stderr,
                "error": error,
                "duration_ms": u64::try_from(duration.as_millis()).unwrap_or(u64::MAX),
            }),
        )
    }

    /// A snapshot attempt with `method` (`proxmox`, `lvm`, ..., or `none`
    /// when unsupported), the snapshot's name if one was taken.
    pub fn snapshot(method: &str, name: Option<&str>, detail: &str) -> Self {
        Self::new(
            EventKind::Snapshot,
            Actor::System,
            json!({ "method": method, "name": name, "detail": detail }),
        )
    }

    pub fn escalation(actor: Actor, reason: &str) -> Self {
        Self::new(EventKind::Escalation, actor, json!({ "reason": reason }))
    }

    pub fn state_change(actor: Actor, from: IncidentState, to: IncidentState) -> Self {
        Self::new(
            EventKind::StateChange,
            actor,
            json!({ "from": from.as_str(), "to": to.as_str() }),
        )
    }

    pub fn note(actor: Actor, text: &str) -> Self {
        Self::new(EventKind::Note, actor, json!({ "text": text }))
    }
}

/// What a new incident needs; the customer is the server's.
#[derive(Debug, Clone)]
pub struct NewIncident {
    pub server_id: Uuid,
    pub alert_incident_id: Option<Uuid>,
    pub metric: String,
    pub instance: String,
    pub max_commands: u32,
    pub max_duration: Duration,
}

/// Opens an incident in state `open`, without events.
pub async fn open(db: impl PgExecutor<'_>, incident: &NewIncident) -> Result<Uuid, AuditError> {
    let max_commands =
        i32::try_from(incident.max_commands).map_err(|_| AuditError::Invalid("max_commands"))?;
    let max_duration_s = i32::try_from(incident.max_duration.as_secs())
        .map_err(|_| AuditError::Invalid("max_duration"))?;
    if max_commands == 0 || max_duration_s == 0 {
        return Err(AuditError::Invalid("limits must be positive"));
    }
    sqlx::query_scalar(
        "INSERT INTO remediation_incidents
             (customer_id, server_id, alert_incident_id, metric, instance,
              max_commands, max_duration_s)
         SELECT si.customer_id, s.id, $2, $3, $4, $5, $6
         FROM servers s JOIN sites si ON si.id = s.site_id
         WHERE s.id = $1
         RETURNING id",
    )
    .bind(incident.server_id)
    .bind(incident.alert_incident_id)
    .bind(&incident.metric)
    .bind(&incident.instance)
    .bind(max_commands)
    .bind(max_duration_s)
    .fetch_optional(db)
    .await?
    .ok_or(AuditError::NotFound)
}

/// One stored event.
#[derive(Debug, Clone, PartialEq, Serialize, sqlx::FromRow)]
pub struct StoredEvent {
    pub seq: i64,
    pub kind: EventKind,
    pub actor_kind: String,
    pub actor_id: String,
    pub payload: Value,
    pub created_at: DateTime<Utc>,
    #[serde(serialize_with = "hex_bytes")]
    pub prev_hash: Vec<u8>,
    #[serde(serialize_with = "hex_bytes")]
    pub hash: Vec<u8>,
}

const EVENT_COLUMNS: &str = "seq, kind, actor_kind, actor_id, payload, created_at, prev_hash, hash";

/// Appends `event` to `incident` in its own transaction.
pub async fn append(
    pool: &PgPool,
    incident: Uuid,
    event: NewEvent,
) -> Result<StoredEvent, AuditError> {
    let mut tx = pool.begin().await?;
    let stored = append_in(&mut tx, incident, event).await?;
    tx.commit().await?;
    Ok(stored)
}

/// Appends `event` within `tx`, so it commits with the caller's other
/// changes (a state change, a queued command). The incident stays locked
/// until `tx` ends.
pub async fn append_in(
    tx: &mut Transaction<'_, Postgres>,
    incident: Uuid,
    event: NewEvent,
) -> Result<StoredEvent, AuditError> {
    let payload = event.payload.to_string();
    if payload.len() > MAX_PAYLOAD_BYTES {
        return Err(AuditError::PayloadTooLarge);
    }
    let head: Option<(i64, Option<Vec<u8>>)> = sqlx::query_as(
        "SELECT head_seq, head_hash FROM remediation_incidents WHERE id = $1 FOR UPDATE",
    )
    .bind(incident)
    .fetch_optional(&mut **tx)
    .await?;
    let (head_seq, head_hash) = head.ok_or(AuditError::NotFound)?;
    let (actor_kind, actor_id) = event.actor.columns();
    // The trigger overwrites `hash`; GENESIS only fills the column.
    let stored = sqlx::query_as(&format!(
        "INSERT INTO remediation_incident_events
             (incident_id, seq, kind, actor_kind, actor_id, payload, prev_hash, hash)
         VALUES ($1, $2, $3, $4, $5, $6::jsonb, $7, $8)
         RETURNING {EVENT_COLUMNS}"
    ))
    .bind(incident)
    .bind(head_seq + 1)
    .bind(event.kind)
    .bind(actor_kind)
    .bind(actor_id)
    .bind(payload)
    .bind(head_hash.unwrap_or_else(|| GENESIS.to_vec()))
    .bind(GENESIS.as_slice())
    .fetch_one(&mut **tx)
    .await?;
    Ok(stored)
}

/// Every event of `incident`, in order.
pub async fn events(
    db: impl PgExecutor<'_>,
    incident: Uuid,
) -> Result<Vec<StoredEvent>, AuditError> {
    Ok(sqlx::query_as(&format!(
        "SELECT {EVENT_COLUMNS} FROM remediation_incident_events
         WHERE incident_id = $1 ORDER BY seq"
    ))
    .bind(incident)
    .fetch_all(db)
    .await?)
}

/// Result of [`verify`]. Field names are part of the API contract.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ChainReport {
    pub valid: bool,
    /// Events checked.
    pub events: u64,
    /// The first sequence number that does not check out.
    pub broken_at: Option<i64>,
    /// `gap`, `prev_hash`, `hash` or `head`.
    pub reason: Option<&'static str>,
    /// Hash of the last event, hex; `null` without events.
    pub head_hash: Option<String>,
}

/// The fields of an event that [`verify_chain`] checks.
#[derive(Debug, Clone, sqlx::FromRow)]
pub struct ChainRow {
    pub seq: i64,
    pub prev_hash: Vec<u8>,
    pub hash: Vec<u8>,
    /// `remediation_event_canonical` of the row.
    pub canonical: String,
}

/// SHA-256 of `prev_hash` followed by `canonical`.
pub fn event_hash(prev_hash: &[u8], canonical: &str) -> [u8; 32] {
    let mut context = Context::new(&SHA256);
    context.update(prev_hash);
    context.update(canonical.as_bytes());
    let mut out = [0; 32];
    out.copy_from_slice(context.finish().as_ref());
    out
}

/// Checks `rows` (in sequence order) against each other and against the
/// incident's head.
pub fn verify_chain(rows: &[ChainRow], head_seq: i64, head_hash: Option<&[u8]>) -> ChainReport {
    let broken = |seq: i64, reason| ChainReport {
        valid: false,
        events: rows.len() as u64,
        broken_at: Some(seq),
        reason: Some(reason),
        head_hash: head_hash.map(hex),
    };
    let mut prev: &[u8] = &GENESIS;
    let mut expected = 1;
    for row in rows {
        if row.seq != expected {
            return broken(expected, "gap");
        }
        if row.prev_hash != prev {
            return broken(expected, "prev_hash");
        }
        if row.hash != event_hash(prev, &row.canonical) {
            return broken(expected, "hash");
        }
        prev = &row.hash;
        expected += 1;
    }
    let last = rows.last().map(|r| r.hash.as_slice());
    if head_seq != expected - 1 || head_hash != last {
        return broken(expected, "head");
    }
    ChainReport {
        valid: true,
        events: rows.len() as u64,
        broken_at: None,
        reason: None,
        head_hash: last.map(hex),
    }
}

/// Recomputes the chain of `incident` from its rows.
pub async fn verify(pool: &PgPool, incident: Uuid) -> Result<ChainReport, AuditError> {
    // One snapshot for the head and the rows, so a concurrent append does
    // not look like a broken chain.
    let mut tx = pool.begin().await?;
    sqlx::query("SET TRANSACTION ISOLATION LEVEL REPEATABLE READ, READ ONLY")
        .execute(&mut *tx)
        .await?;
    let head: Option<(i64, Option<Vec<u8>>)> =
        sqlx::query_as("SELECT head_seq, head_hash FROM remediation_incidents WHERE id = $1")
            .bind(incident)
            .fetch_optional(&mut *tx)
            .await?;
    let (head_seq, head_hash) = head.ok_or(AuditError::NotFound)?;
    let rows: Vec<ChainRow> = sqlx::query_as(
        "SELECT seq, prev_hash, hash,
                remediation_event_canonical(incident_id, seq, kind, actor_kind, actor_id,
                                            payload, created_at) AS canonical
         FROM remediation_incident_events WHERE incident_id = $1 ORDER BY seq",
    )
    .bind(incident)
    .fetch_all(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(verify_chain(&rows, head_seq, head_hash.as_deref()))
}

/// Lowercase hexadecimal.
pub fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn hex_bytes<S: serde::Serializer>(bytes: &[u8], serializer: S) -> Result<S::Ok, S::Error> {
    serializer.serialize_str(&hex(bytes))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn chain(contents: &[&str]) -> Vec<ChainRow> {
        let mut prev = GENESIS.to_vec();
        contents
            .iter()
            .zip(1..)
            .map(|(canonical, seq)| {
                let hash = event_hash(&prev, canonical).to_vec();
                ChainRow {
                    seq,
                    prev_hash: std::mem::replace(&mut prev, hash.clone()),
                    hash,
                    canonical: (*canonical).to_owned(),
                }
            })
            .collect()
    }

    fn check(rows: &[ChainRow]) -> ChainReport {
        let head = rows.last().map(|r| r.hash.as_slice());
        verify_chain(rows, rows.len() as i64, head)
    }

    #[test]
    fn intact_chains_verify() {
        let report = verify_chain(&[], 0, None);
        assert!(report.valid);
        assert_eq!(report.events, 0);
        assert_eq!(report.head_hash, None);
        let rows = chain(&["a", "b", "c"]);
        let report = check(&rows);
        assert!(report.valid, "{report:?}");
        assert_eq!(report.events, 3);
        assert_eq!(report.head_hash, Some(hex(&rows[2].hash)));
    }

    #[test]
    fn every_kind_of_tampering_is_located() {
        let rows = chain(&["a", "b", "c"]);
        let mut changed = rows.clone();
        changed[1].canonical = "B".into();
        assert_eq!(check(&changed).broken_at, Some(2));
        assert_eq!(check(&changed).reason, Some("hash"));

        // Rehashing the changed row breaks the link to the next one.
        changed[1].hash = event_hash(&changed[1].prev_hash, "B").to_vec();
        assert_eq!(check(&changed).reason, Some("prev_hash"));
        assert_eq!(check(&changed).broken_at, Some(3));

        let gap = vec![rows[0].clone(), rows[2].clone()];
        let report = verify_chain(&gap, 3, Some(&rows[2].hash));
        assert_eq!((report.broken_at, report.reason), (Some(2), Some("gap")));

        // A missing tail only shows against the head.
        let report = verify_chain(&rows[..2], 3, Some(&rows[2].hash));
        assert_eq!((report.broken_at, report.reason), (Some(3), Some("head")));
        assert!(!verify_chain(&rows, 3, Some(&GENESIS)).valid);
    }

    #[test]
    fn hashes_match_sha256_of_prefix_and_content() {
        // SHA-256 of 32 zero bytes followed by "abc", computed elsewhere.
        assert_eq!(
            hex(&event_hash(&GENESIS, "abc")),
            hex(ring::digest::digest(&SHA256, &[&GENESIS[..], b"abc"].concat()).as_ref())
        );
        assert_eq!(hex(&[0x00, 0xab, 0x0f]), "00ab0f");
    }

    #[test]
    fn constructors_fix_kinds_actors_and_fields() {
        let user = Uuid::from_u128(1);
        let event = NewEvent::ai_proposal("claude/x", "disk full", "logs", Some("df -h"));
        assert_eq!(event.kind, EventKind::AiProposal);
        assert_eq!(event.actor, Actor::Ai("claude/x".into()));
        assert_eq!(event.payload["command"], "df -h");
        let event = NewEvent::rejection(user, "rm -rf /tmp/x", None);
        assert_eq!(event.actor.columns(), ("user", user.to_string()));
        assert_eq!(event.payload["reason"], Value::Null);
        let event = NewEvent::command_result(
            "agent-1",
            user,
            Some(0),
            "ok",
            "",
            None,
            Duration::from_millis(1500),
        );
        assert_eq!(event.actor.columns(), ("peer", "agent-1".to_owned()));
        assert_eq!(event.payload["duration_ms"], 1500);
        let event =
            NewEvent::state_change(Actor::System, IncidentState::Open, IncidentState::Escalated);
        assert_eq!(event.payload, json!({ "from": "open", "to": "escalated" }));
        assert!(IncidentState::Escalated.is_closed());
        assert!(!IncidentState::AwaitingApproval.is_closed());
        assert_eq!(
            serde_json::to_value(EventKind::CommandQueued).unwrap(),
            "command_queued"
        );
    }
}
