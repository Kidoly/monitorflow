//! Drives one incident: each pass reads its events ([`progress::of`]),
//! takes the one action they call for and records it, until the incident
//! closes or waits for approval.

use std::time::Duration;

use chrono::Utc;
use monitorflow_ai::{Action, StepRequest, StopOutcome};
use monitorflow_common::command::SHELL;
use monitorflow_common::proto::v1::ShellCommand;
use prost::Message;
use serde_json::json;
use uuid::Uuid;

use super::hook::Change;
use super::progress::{self, head, Next, Progress};
use super::store::{self, Incident};
use super::{prompt, DriveError, Shared};
use crate::commands::{CommandRow, CommandStore, NewCommand, State};
use crate::remediation::audit::{self, Actor, IncidentState, NewEvent};
use crate::remediation::context::{self, Trigger};
use crate::remediation::redact::Redactor;
use crate::remediation::risk::{self, Decision, RuleSet, Tier};

/// Longest diagnosis, reasoning or reason of the AI stored, in bytes.
const AI_TEXT_BYTES: usize = 8 * 1024;
/// Time allowed after the command timeout for the agent's result to come.
const RESULT_GRACE: Duration = Duration::from_secs(15);

/// Whether a pass should run again.
enum Pass {
    Again,
    Done,
}

/// Runs passes until `incident` closes, waits for approval, or another
/// process drives it.
pub(super) async fn drive(shared: &Shared, id: Uuid) -> Result<(), DriveError> {
    loop {
        let Some(incident) = store::incident(&shared.pool, id).await? else {
            return Ok(());
        };
        match incident.state {
            IncidentState::Open => {
                let mut tx = shared.pool.begin().await?;
                if store::lock(&mut tx, id).await? == IncidentState::Open {
                    store::transition(&mut tx, id, IncidentState::Open, IncidentState::Running)
                        .await?;
                }
                tx.commit().await?;
                continue;
            }
            IncidentState::Running => {}
            _ => return Ok(()),
        }
        if let Pass::Done = pass(shared, &incident).await? {
            return Ok(());
        }
    }
}

async fn pass(shared: &Shared, incident: &Incident) -> Result<Pass, DriveError> {
    let events = audit::events(&shared.pool, incident.id).await?;
    let progress = progress::of(&events);
    // A command in flight is waited for first, so its result is recorded.
    if !matches!(progress.next, Next::Wait { .. }) {
        if alert_resolved(shared, incident).await? {
            return finish(
                shared,
                incident,
                IncidentState::Resolved,
                "the sensor returned to normal",
            )
            .await;
        }
        if Utc::now() >= incident.deadline() {
            return duration_limit(shared, incident).await;
        }
    }
    match progress.next.clone() {
        Next::Context => send_context(shared, incident).await,
        Next::Ask => ask(shared, incident, progress).await,
        Next::Classify {
            seq,
            diagnosis,
            proposal,
        } => {
            if progress.commands_queued >= incident.max_commands() {
                let reason = format!(
                    "the limit of {} commands was reached",
                    incident.max_commands
                );
                return finish(shared, incident, IncidentState::Escalated, &reason).await;
            }
            route(
                shared,
                incident,
                seq,
                &diagnosis,
                &proposal.command,
                &proposal.reasoning,
            )
            .await
        }
        Next::Run {
            command,
            by_system,
            snapshot_done,
        } => run(shared, incident, &command, by_system, snapshot_done).await,
        Next::Wait {
            command_id,
            command,
        } => wait(shared, incident, command_id, &command).await,
        Next::Stopped {
            outcome: StopOutcome::Escalate,
            reason,
            ..
        } => {
            let reason = format!("the AI escalated: {reason}");
            finish(shared, incident, IncidentState::Escalated, &reason).await
        }
        Next::Stopped {
            outcome: StopOutcome::Resolved,
            at,
            ..
        } => confirm(shared, incident, at).await,
    }
}

async fn alert_resolved(shared: &Shared, incident: &Incident) -> Result<bool, DriveError> {
    match incident.alert_incident_id {
        Some(alert) => Ok(store::alert_resolved(&shared.pool, alert).await?),
        None => Ok(false),
    }
}

async fn duration_limit(shared: &Shared, incident: &Incident) -> Result<Pass, DriveError> {
    let reason = format!(
        "the duration limit of {} s was reached",
        incident.max_duration_s
    );
    finish_as(
        shared,
        incident,
        IncidentState::Escalated,
        &reason,
        Pending::Expire,
    )
    .await
}

/// Gathers, redacts and records the context.
async fn send_context(shared: &Shared, incident: &Incident) -> Result<Pass, DriveError> {
    let trigger = match incident.alert_incident_id {
        Some(alert) => Trigger::of_incident(&shared.pool, alert).await?,
        None => None,
    };
    let Some(trigger) = trigger else {
        return finish(
            shared,
            incident,
            IncidentState::Failed,
            "the alert that opened the incident is gone",
        )
        .await;
    };
    let gathered = context::gather(
        &shared.backends,
        &trigger,
        &shared.settings.context_limits,
        Utc::now(),
    )
    .await
    .and_then(|context| Ok((context.render()?, context.redactions)));
    let (rendered, redactions) = match gathered {
        Ok(gathered) => gathered,
        Err(error) => {
            let reason = format!("the context could not be gathered: {error}");
            return finish(shared, incident, IncidentState::Failed, &reason).await;
        }
    };
    let event = NewEvent::context_sent(
        &shared.model,
        json!({ "rendered": rendered, "redactions": redactions }),
    );
    audit::append(&shared.pool, incident.id, event).await?;
    Ok(Pass::Again)
}

/// Asks the AI for the next step and records its answer.
async fn ask(shared: &Shared, incident: &Incident, progress: Progress) -> Result<Pass, DriveError> {
    let request = StepRequest {
        system_prompt: prompt::system(prompt::Limits {
            max_commands: incident.max_commands(),
            commands_left: incident
                .max_commands()
                .saturating_sub(progress.commands_queued),
            command_timeout: shared.settings.command_timeout,
        }),
        context: vec![prompt::context_block(
            progress.context.as_deref().unwrap_or_default(),
        )],
        history: progress.history,
    };
    let deadline = instant(incident.deadline());
    let step = match tokio::time::timeout_at(deadline, shared.provider.next_step(&request)).await {
        // The next pass escalates.
        Err(_) => return Ok(Pass::Again),
        Ok(Err(error)) => {
            let reason = format!("the AI provider failed: {error}");
            return finish(shared, incident, IncidentState::Failed, &reason).await;
        }
        Ok(Ok(step)) => step,
    };
    let diagnosis = head(&step.diagnosis, AI_TEXT_BYTES);
    let event = match &step.action {
        Action::Run(proposal) => NewEvent::ai_proposal(
            &shared.model,
            &diagnosis,
            &head(&proposal.reasoning, AI_TEXT_BYTES),
            Some(&proposal.command),
        ),
        Action::Stop(stop) => {
            let outcome = match stop.outcome {
                StopOutcome::Resolved => "resolved",
                StopOutcome::Escalate => "escalate",
            };
            NewEvent::ai_stop(
                &shared.model,
                &diagnosis,
                outcome,
                &head(&stop.reason, AI_TEXT_BYTES),
            )
        }
    };
    audit::append(&shared.pool, incident.id, event).await?;
    Ok(Pass::Again)
}

async fn rules(shared: &Shared, incident: &Incident) -> Result<RuleSet, DriveError> {
    Ok(risk::store::rule_set(&shared.pool, incident.customer_id, Some(incident.site_id)).await?)
}

/// Whether `decision` lets a command run without a human.
async fn automatic(
    shared: &Shared,
    incident: &Incident,
    decision: &Decision,
) -> Result<bool, DriveError> {
    Ok(match decision.tier {
        Tier::ReadOnly => true,
        Tier::LowImpact => {
            risk::store::low_impact_auto(&shared.pool, incident.customer_id, Some(incident.site_id))
                .await?
        }
        Tier::HighImpact | Tier::Forbidden => false,
    })
}

/// Classifies the proposal by the rules and approves it, parks it for a
/// human or refuses it, with its classification, in one transaction.
async fn route(
    shared: &Shared,
    incident: &Incident,
    seq: i64,
    diagnosis: &str,
    command: &str,
    reasoning: &str,
) -> Result<Pass, DriveError> {
    let decision = risk::classify(command, &rules(shared, incident).await?);
    let automatic = automatic(shared, incident, &decision).await?;
    let mut tx = shared.pool.begin().await?;
    if store::lock(&mut tx, incident.id).await? != IncidentState::Running {
        return Ok(Pass::Again);
    }
    audit::append_in(
        &mut tx,
        incident.id,
        NewEvent::classification(command, &decision),
    )
    .await?;
    let pass = if automatic {
        audit::append_in(
            &mut tx,
            incident.id,
            NewEvent::approval(Actor::System, command),
        )
        .await?;
        Pass::Again
    } else if decision.tier == Tier::Forbidden {
        let reason = format!("the AI proposed a forbidden command ({})", decision.reason);
        close_in(
            &mut tx,
            incident.id,
            IncidentState::Escalated,
            &reason,
            Pending::Cancel,
        )
        .await?;
        Pass::Done
    } else {
        store::insert_pending(
            &mut tx,
            incident.id,
            seq,
            diagnosis,
            command,
            reasoning,
            &decision,
        )
        .await?;
        store::transition(
            &mut tx,
            incident.id,
            IncidentState::Running,
            IncidentState::AwaitingApproval,
        )
        .await?;
        Pass::Done
    };
    tx.commit().await?;
    if let Pass::Done = pass {
        store::release(&shared.pool, Some(incident.id), shared.holder).await?;
    }
    Ok(pass)
}

/// Runs the snapshot hook for a change, then queues the exact approved
/// command with its event.
async fn run(
    shared: &Shared,
    incident: &Incident,
    command: &str,
    by_system: bool,
    snapshot_done: bool,
) -> Result<Pass, DriveError> {
    // The rules again, on exactly what is about to run: an edited command
    // is still refused when forbidden, and a system approval needs a tier
    // that runs without one.
    let decision = risk::classify(command, &rules(shared, incident).await?);
    if decision.tier == Tier::Forbidden {
        let reason = format!("the approved command is forbidden ({})", decision.reason);
        return finish(shared, incident, IncidentState::Escalated, &reason).await;
    }
    if by_system && !automatic(shared, incident, &decision).await? {
        let reason = format!(
            "the command now needs approval ({}); the rules changed",
            decision.reason
        );
        return finish(shared, incident, IncidentState::Escalated, &reason).await;
    }
    if decision.tier != Tier::ReadOnly && !snapshot_done {
        let change = Change {
            incident_id: incident.id,
            server_id: incident.server_id,
            command,
            tier: decision.tier,
        };
        return match shared.hook.before_change(&change).await {
            Ok(snapshot) => {
                let event = NewEvent::snapshot(
                    &snapshot.method,
                    snapshot.name.as_deref(),
                    &snapshot.detail,
                );
                audit::append(&shared.pool, incident.id, event).await?;
                Ok(Pass::Again)
            }
            Err(error) => {
                let reason = format!("the snapshot before the change failed: {error}");
                finish(shared, incident, IncidentState::Escalated, &reason).await
            }
        };
    }
    let timeout = shared.settings.command_timeout;
    let payload = ShellCommand {
        command: command.to_owned(),
        timeout_seconds: u32::try_from(timeout.as_secs()).unwrap_or(u32::MAX),
    }
    .encode_to_vec();
    let mut tx = shared.pool.begin().await?;
    if store::lock(&mut tx, incident.id).await? != IncidentState::Running {
        return Ok(Pass::Again);
    }
    let row = CommandStore::enqueue_in(
        &mut *tx,
        &NewCommand {
            server_id: incident.server_id,
            kind: SHELL.into(),
            payload,
            ttl: timeout,
            requested_by: None,
            campaign_id: None,
        },
    )
    .await?;
    audit::append_in(
        &mut tx,
        incident.id,
        NewEvent::command_queued(row.id, command),
    )
    .await?;
    tx.commit().await?;
    shared.commands.dispatch(&incident.peer_id);
    Ok(Pass::Again)
}

/// Waits for the result of `command_id` and records it, redacted. A
/// command that did not run to completion, or no result in time,
/// escalates.
async fn wait(
    shared: &Shared,
    incident: &Incident,
    command_id: Uuid,
    command: &str,
) -> Result<Pass, DriveError> {
    let store = shared.commands.store();
    let Some(row) = store.get(command_id).await? else {
        return finish(
            shared,
            incident,
            IncidentState::Failed,
            "the queued command is gone",
        )
        .await;
    };
    let limit = (row.issued_at
        + chrono::Duration::from_std(shared.settings.command_timeout + RESULT_GRACE)
            .unwrap_or_default())
    .min(incident.deadline());
    let mut row = row;
    while !row.state.is_final() && Utc::now() < limit {
        tokio::time::sleep(shared.settings.result_poll).await;
        row = store.get(command_id).await?.unwrap_or(row);
    }
    if !row.state.is_final() {
        let reason = format!(
            "no result within {} s",
            (limit - row.issued_at).num_seconds()
        );
        if store::cancel_queued(&shared.pool, command_id, &reason).await? {
            tracing::info!(incident = %incident.id, command_id = %command_id, "cancelled an undelivered remediation command");
        }
        let event = NewEvent::command_result(
            &incident.peer_id,
            command_id,
            None,
            "",
            "",
            Some(&reason),
            elapsed(&row),
        );
        audit::append(&shared.pool, incident.id, event).await?;
        if limit >= incident.deadline() {
            return duration_limit(shared, incident).await;
        }
        let reason = format!("the command timed out: {command}");
        return finish(shared, incident, IncidentState::Escalated, &reason).await;
    }
    let mut redactor = Redactor::new();
    let stdout = redactor.redact(row.stdout.as_deref().unwrap_or_default());
    let stderr = redactor.redact(row.stderr.as_deref().unwrap_or_default());
    let error = row.error.as_deref().map(|e| redactor.redact(e));
    let completed = row.started_at.is_some()
        && matches!(row.state, State::Succeeded | State::Failed)
        && row.exit_code.is_some_and(|c| c >= 0);
    let exit_code = if completed { row.exit_code } else { None };
    let event = NewEvent::command_result(
        &incident.peer_id,
        command_id,
        exit_code,
        stdout.as_str(),
        stderr.as_str(),
        error.as_ref().map(|e| e.as_str()),
        elapsed(&row),
    );
    audit::append(&shared.pool, incident.id, event).await?;
    if !completed {
        let why = error.map_or_else(|| row.state.as_str().to_owned(), |e| e.to_string());
        let reason = format!("the command did not run to completion: {why}");
        return finish(shared, incident, IncidentState::Escalated, &reason).await;
    }
    Ok(Pass::Again)
}

fn elapsed(row: &CommandRow) -> Duration {
    let start = row.started_at.unwrap_or(row.issued_at);
    let end = row.finished_at.unwrap_or_else(Utc::now);
    (end - start).to_std().unwrap_or_default()
}

/// The AI says the problem is fixed: waits for the sensor to agree.
async fn confirm(
    shared: &Shared,
    incident: &Incident,
    said_at: chrono::DateTime<Utc>,
) -> Result<Pass, DriveError> {
    let limit = (said_at
        + chrono::Duration::from_std(shared.settings.resolve_wait).unwrap_or_default())
    .min(incident.deadline());
    loop {
        if alert_resolved(shared, incident).await? {
            return finish(
                shared,
                incident,
                IncidentState::Resolved,
                "the sensor returned to normal",
            )
            .await;
        }
        if Utc::now() >= limit {
            return finish(
                shared,
                incident,
                IncidentState::Escalated,
                "the AI considers the problem fixed, but the sensor still breaches its threshold",
            )
            .await;
        }
        tokio::time::sleep(shared.settings.result_poll).await;
    }
}

/// Closes `incident` as `to` and gives up its lease.
async fn finish(
    shared: &Shared,
    incident: &Incident,
    to: IncidentState,
    reason: &str,
) -> Result<Pass, DriveError> {
    finish_as(shared, incident, to, reason, Pending::Cancel).await
}

async fn finish_as(
    shared: &Shared,
    incident: &Incident,
    to: IncidentState,
    reason: &str,
    pending: Pending,
) -> Result<Pass, DriveError> {
    close(&shared.pool, incident.id, to, reason, pending).await?;
    store::release(&shared.pool, Some(incident.id), shared.holder).await?;
    tracing::info!(incident = %incident.id, state = to.as_str(), reason, "remediation incident closed");
    Ok(Pass::Done)
}

/// What becomes of a command still waiting for approval when its incident
/// closes.
#[derive(Debug, Clone, Copy)]
pub(super) enum Pending {
    /// The incident closed for another reason.
    Cancel,
    /// The duration limit passed.
    Expire,
}

/// Closes `incident` as `to` unless already closed.
pub(super) async fn close(
    pool: &sqlx::PgPool,
    incident: Uuid,
    to: IncidentState,
    reason: &str,
    pending: Pending,
) -> Result<(), DriveError> {
    let mut tx = pool.begin().await?;
    close_in(&mut tx, incident, to, reason, pending).await?;
    tx.commit().await?;
    Ok(())
}

/// [`close`] in `tx`: an `escalation` event for an incident handed over
/// (escalated or failed), a `note` for one resolved, its pending command
/// ended, then the state change.
async fn close_in(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    incident: Uuid,
    to: IncidentState,
    reason: &str,
    pending: Pending,
) -> Result<(), DriveError> {
    let from = store::lock(tx, incident).await?;
    if from.is_closed() {
        return Ok(());
    }
    let event = match to {
        IncidentState::Resolved => NewEvent::note(Actor::System, reason),
        _ => NewEvent::escalation(Actor::System, reason),
    };
    audit::append_in(tx, incident, event).await?;
    let pending = match pending {
        Pending::Cancel => "cancelled",
        Pending::Expire => "expired",
    };
    store::close_pending(tx, incident, pending).await?;
    store::transition(tx, incident, from, to).await?;
    Ok(())
}

/// `at` as a Tokio instant, now when past.
fn instant(at: chrono::DateTime<Utc>) -> tokio::time::Instant {
    let left = (at - Utc::now()).to_std().unwrap_or_default();
    tokio::time::Instant::now() + left
}
