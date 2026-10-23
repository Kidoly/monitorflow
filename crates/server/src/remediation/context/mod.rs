//! The context of an incident for the AI (architecture §8, flow step 2;
//! ADR-0040): the breach, the server's inventory, the breached series'
//! recent history, the server's recent logs and recent changes.
//!
//! Every section is bounded in items, bytes and time, and says what it
//! dropped. Every free-text field goes through one [`Redactor`] while the
//! context is built, so a [`Context`] never holds unredacted text and its
//! [`Redactions`] count everything replaced. Everything is read for one
//! server, and [`Context::check_owner`] asserts that every row, series and
//! log line belongs to that server's customer before it is rendered:
//! one AI request carries one customer's data (MSP mode).

mod render;
mod sources;

use std::time::Duration as StdDuration;

use chrono::{DateTime, Duration, Utc};
use sqlx::PgPool;
use uuid::Uuid;

use super::redact::{Redacted, Redactions, Redactor};
use crate::alerting::Severity;
use crate::vm_query::VmQuery;

/// Bounds of each section.
#[derive(Debug, Clone, PartialEq)]
pub struct Limits {
    /// How far back the breached series is read.
    pub history_window: Duration,
    /// Most points per series; the step is the window over this.
    pub history_points: usize,
    /// Most series of the breached metric (one, unless labels split it).
    pub history_series: usize,
    pub log_window: Duration,
    /// The newest lines kept.
    pub log_lines: usize,
    /// Longest log line, in bytes.
    pub log_line_bytes: usize,
    /// How far back commands and campaigns are read.
    pub changes_window: Duration,
    pub commands: usize,
    /// The end of each command's stdout and stderr kept, in bytes.
    pub output_bytes: usize,
    /// Longest error or parameter text, in bytes.
    pub text_bytes: usize,
    pub campaigns: usize,
    /// Upgradable packages listed.
    pub packages: usize,
    /// Largest rendered section, in bytes.
    pub section_bytes: usize,
    /// Time allowed to read one section.
    pub section_timeout: StdDuration,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            history_window: Duration::hours(6),
            history_points: 72,
            history_series: 4,
            log_window: Duration::hours(24),
            log_lines: 40,
            log_line_bytes: 400,
            changes_window: Duration::days(14),
            commands: 10,
            output_bytes: 2048,
            text_bytes: 512,
            campaigns: 5,
            packages: 30,
            section_bytes: 16 * 1024,
            section_timeout: StdDuration::from_secs(10),
        }
    }
}

/// Where the context reads from. A backend that is not configured makes
/// its section unavailable, not the whole context.
#[derive(Debug, Clone)]
pub struct Backends {
    pub pool: PgPool,
    pub metrics: Option<VmQuery>,
    /// A [`VmQuery`] on `victorialogs_url`.
    pub logs: Option<VmQuery>,
}

/// The threshold breach that opened an incident.
#[derive(Debug, Clone, PartialEq)]
pub struct Trigger {
    pub incident_id: Option<Uuid>,
    pub server_id: Uuid,
    pub metric: String,
    /// `''` for a series without an instance.
    pub instance: String,
    pub severity: Severity,
    pub opened_at: DateTime<Utc>,
    pub value: f64,
    pub peak: f64,
    /// The threshold when the incident opened, `Rule::snapshot`.
    pub threshold: serde_json::Value,
}

impl Trigger {
    /// The breach of incident `id`, with its latest value.
    pub async fn of_incident(pool: &PgPool, id: Uuid) -> Result<Option<Self>, sqlx::Error> {
        type Line = (
            Uuid,
            String,
            String,
            String,
            DateTime<Utc>,
            f64,
            f64,
            serde_json::Value,
        );
        let line: Option<Line> = sqlx::query_as(
            "SELECT server_id, metric, instance, severity, opened_at, last_value, peak_value,
                    threshold
             FROM incidents WHERE id = $1",
        )
        .bind(id)
        .fetch_optional(pool)
        .await?;
        Ok(line.map(
            |(server_id, metric, instance, severity, opened_at, value, peak, threshold)| Self {
                incident_id: Some(id),
                server_id,
                metric,
                instance,
                // The column's CHECK allows nothing else.
                severity: severity.parse().unwrap_or(Severity::Critical),
                opened_at,
                value,
                peak,
                threshold,
            },
        ))
    }
}

#[derive(Debug, thiserror::Error)]
pub enum ContextError {
    #[error("server {0} not found")]
    ServerNotFound(Uuid),
    #[error("database: {0}")]
    Database(#[from] sqlx::Error),
    #[error("timed out reading the server inventory")]
    Timeout,
    /// Data of another server or customer reached the context: a bug in a
    /// query or a backend, and a reason to send nothing.
    #[error("{section} holds data of another tenant: {detail}")]
    ForeignData {
        section: &'static str,
        detail: String,
    },
}

/// Whose data the context may hold.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Owner {
    pub customer_id: Uuid,
    pub site_id: Uuid,
    pub server_id: Uuid,
    pub peer: String,
    /// The `site` label of the server's series and log streams.
    pub site: String,
}

/// A section, or why it could not be read.
#[derive(Debug, Clone, PartialEq)]
pub enum Section<T> {
    Ready(T),
    Unavailable(&'static str),
}

/// The breach, redacted.
#[derive(Debug, Clone, PartialEq)]
pub struct Incident {
    pub id: Option<Uuid>,
    pub server_id: Uuid,
    pub metric: String,
    pub instance: Redacted,
    pub severity: Severity,
    pub opened_at: DateTime<Utc>,
    pub value: f64,
    pub peak: f64,
    /// The threshold snapshot as JSON.
    pub threshold: Redacted,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Inventory {
    pub peer: String,
    pub hostname: Redacted,
    pub customer: Redacted,
    pub site: Redacted,
    pub relay: Option<Redacted>,
    pub os_family: String,
    pub os_version: Option<Redacted>,
    pub agent_version: Option<Redacted>,
    pub last_seen_at: Option<DateTime<Utc>>,
    pub revoked_at: Option<DateTime<Utc>>,
    pub reboot_required: Option<bool>,
    pub packages_reported_at: Option<DateTime<Utc>>,
    pub package_count: i64,
    /// Upgradable packages by name, at most [`Limits::packages`].
    pub upgradable: Vec<Package>,
    pub upgradable_omitted: usize,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Package {
    pub name: Redacted,
    pub arch: Redacted,
    pub version: Redacted,
    pub candidate: Redacted,
}

#[derive(Debug, Clone, PartialEq)]
pub struct History {
    pub start: i64,
    pub end: i64,
    pub step_s: i64,
    /// Sorted by instance.
    pub series: Vec<Series>,
    pub series_omitted: usize,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Series {
    /// Labels as answered, checked by [`Context::check_owner`].
    pub peer: Option<String>,
    pub site: Option<String>,
    pub instance: Redacted,
    /// `(unix seconds, value)`, oldest first, the newest kept.
    pub points: Vec<(i64, f64)>,
    pub points_omitted: usize,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Logs {
    pub start: DateTime<Utc>,
    pub end: DateTime<Utc>,
    /// Oldest first.
    pub lines: Vec<LogLine>,
    /// More lines matched than [`Limits::log_lines`]; the oldest were left
    /// out.
    pub truncated: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub struct LogLine {
    pub peer: Option<String>,
    pub site: Option<String>,
    pub time: DateTime<Utc>,
    /// The message, then the other fields as `key=value`.
    pub text: Redacted,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Changes {
    pub since: DateTime<Utc>,
    /// Newest first.
    pub commands: Vec<CommandEntry>,
    pub commands_omitted: usize,
    pub campaigns: Vec<CampaignEntry>,
    pub campaigns_omitted: usize,
}

/// A command from the commands journal. Package changes are
/// `apply_updates` commands; their parameters name the packages.
#[derive(Debug, Clone, PartialEq)]
pub struct CommandEntry {
    pub id: Uuid,
    pub server_id: Uuid,
    pub kind: String,
    pub state: String,
    pub issued_at: DateTime<Utc>,
    pub finished_at: Option<DateTime<Utc>>,
    pub exit_code: Option<i32>,
    pub campaign_id: Option<Uuid>,
    pub parameters: Redacted,
    pub error: Option<Redacted>,
    pub stdout: Option<Redacted>,
    pub stderr: Option<Redacted>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct CampaignEntry {
    pub id: Uuid,
    pub customer_id: Uuid,
    pub name: Redacted,
    pub state: String,
    pub wave: i32,
    pub packages: Redacted,
    pub security_only: bool,
    pub allow_reboot: bool,
    pub created_at: DateTime<Utc>,
    pub started_at: Option<DateTime<Utc>>,
    pub finished_at: Option<DateTime<Utc>>,
    pub error: Option<Redacted>,
}

/// The whole context of one incident. Build it with [`gather`]; render it
/// with [`Context::render`].
#[derive(Debug, Clone, PartialEq)]
pub struct Context {
    pub owner: Owner,
    pub gathered_at: DateTime<Utc>,
    pub limits: Limits,
    pub incident: Incident,
    pub inventory: Inventory,
    pub history: Section<History>,
    pub logs: Section<Logs>,
    pub changes: Section<Changes>,
    /// Everything redacted while building it, per kind.
    pub redactions: Redactions,
}

/// Reads the context of `trigger` as of `now`. Only a missing server or a
/// failed inventory read is an error; other sections become
/// [`Section::Unavailable`].
pub async fn gather(
    backends: &Backends,
    trigger: &Trigger,
    limits: &Limits,
    now: DateTime<Utc>,
) -> Result<Context, ContextError> {
    let pool = &backends.pool;
    let server = tokio::time::timeout(
        limits.section_timeout,
        sources::server(pool, trigger.server_id),
    )
    .await
    .map_err(|_| ContextError::Timeout)??
    .ok_or(ContextError::ServerNotFound(trigger.server_id))?;
    let owner = Owner {
        customer_id: server.customer_id,
        site_id: server.site_id,
        server_id: server.id,
        peer: server.peer_id.clone(),
        site: server.site.clone(),
    };

    let history_range = sources::history_range(limits, now);
    let log_start = now - limits.log_window;
    let since = now - limits.changes_window;
    let history = async {
        let Some(vm) = &backends.metrics else {
            return Err("VictoriaMetrics is not configured");
        };
        timed(
            "history",
            limits,
            sources::history(vm, &owner, trigger, history_range),
        )
        .await
    };
    let logs = async {
        let Some(vl) = &backends.logs else {
            return Err("VictoriaLogs is not configured");
        };
        timed(
            "logs",
            limits,
            sources::logs(vl, &owner, log_start, now, limits.log_lines + 1),
        )
        .await
    };
    let changes = async {
        let commands = sources::commands(pool, owner.server_id, since, limits.commands);
        let campaigns = sources::campaigns(pool, owner.server_id, since, limits.campaigns);
        timed("changes", limits, async {
            Ok::<_, sqlx::Error>((commands.await?, campaigns.await?))
        })
        .await
    };
    let packages = timed(
        "packages",
        limits,
        sources::upgradable(pool, owner.server_id, limits.packages),
    );
    let (history, logs, changes, packages) = tokio::join!(history, logs, changes, packages);

    let mut redactor = Redactor::new();
    let r = &mut redactor;
    let incident = Incident {
        id: trigger.incident_id,
        server_id: trigger.server_id,
        metric: trigger.metric.clone(),
        instance: r.redact(&trigger.instance),
        severity: trigger.severity,
        opened_at: trigger.opened_at,
        value: trigger.value,
        peak: trigger.peak,
        threshold: r
            .redact(&trigger.threshold.to_string())
            .head(limits.text_bytes),
    };
    let packages = packages.unwrap_or_default();
    let inventory = Inventory {
        peer: server.peer_id,
        hostname: r.redact(&server.hostname),
        customer: r.redact(&server.customer),
        site: r.redact(&server.site),
        relay: server.relay.as_deref().map(|t| r.redact(t)),
        os_family: server.os_family,
        os_version: server.os_version.as_deref().map(|t| r.redact(t)),
        agent_version: server.agent_version.as_deref().map(|t| r.redact(t)),
        last_seen_at: server.last_seen_at,
        revoked_at: server.revoked_at,
        reboot_required: server.reboot_required,
        packages_reported_at: server.packages_reported_at,
        package_count: server.package_count,
        upgradable_omitted: omitted(packages.first().map(|p| p.total), packages.len()),
        upgradable: packages
            .iter()
            .map(|p| Package {
                name: r.redact(&p.name),
                arch: r.redact(&p.arch),
                version: r.redact(&p.version),
                candidate: r.redact(&p.upgradable_version),
            })
            .collect(),
    };
    let history = section(history.map(|series| {
        let (start, end, step_s) = history_range;
        build_history(r, limits, series, start, end, step_s)
    }));
    let logs = section(logs.map(|lines| build_logs(r, limits, lines, log_start, now)));
    let changes = section(
        changes.map(|(commands, campaigns)| build_changes(r, limits, commands, campaigns, since)),
    );
    let context = Context {
        owner,
        gathered_at: now,
        limits: limits.clone(),
        incident,
        inventory,
        history,
        logs,
        changes,
        redactions: redactor.into_redactions(),
    };
    context.check_owner()?;
    Ok(context)
}

/// Runs `read` within the section timeout; failures become a fixed
/// reason, the detail goes to the log only (it may quote backend text).
async fn timed<T, E: std::fmt::Display>(
    name: &'static str,
    limits: &Limits,
    read: impl std::future::Future<Output = Result<T, E>>,
) -> Result<T, &'static str> {
    match tokio::time::timeout(limits.section_timeout, read).await {
        Ok(Ok(value)) => Ok(value),
        Ok(Err(error)) => {
            tracing::warn!(section = name, %error, "incident context section unavailable");
            Err("the backend failed")
        }
        Err(_) => {
            tracing::warn!(section = name, "incident context section timed out");
            Err("timed out")
        }
    }
}

fn section<T>(read: Result<T, &'static str>) -> Section<T> {
    read.map_or_else(Section::Unavailable, Section::Ready)
}

/// How many of `total` rows were not returned when `kept` were.
fn omitted(total: Option<i64>, kept: usize) -> usize {
    usize::try_from(total.unwrap_or(0))
        .unwrap_or(0)
        .saturating_sub(kept)
}

fn build_history(
    r: &mut Redactor,
    limits: &Limits,
    mut raw: Vec<sources::RawSeries>,
    start: i64,
    end: i64,
    step_s: i64,
) -> History {
    raw.sort_by(|a, b| a.labels.cmp(&b.labels));
    let series_omitted = raw.len().saturating_sub(limits.history_series);
    raw.truncate(limits.history_series);
    let series = raw
        .into_iter()
        .map(|s| {
            let points_omitted = s.points.len().saturating_sub(limits.history_points);
            Series {
                peer: s.labels.get("peer").cloned(),
                site: s.labels.get("site").cloned(),
                instance: r.redact(s.labels.get("instance").map_or("", String::as_str)),
                points: s.points[points_omitted..].to_vec(),
                points_omitted,
            }
        })
        .collect();
    History {
        start,
        end,
        step_s,
        series,
        series_omitted,
    }
}

fn build_logs(
    r: &mut Redactor,
    limits: &Limits,
    mut raw: Vec<sources::RawLog>,
    start: DateTime<Utc>,
    end: DateTime<Utc>,
) -> Logs {
    // The newest `log_lines`; one more was asked to see if any is left.
    let truncated = raw.len() > limits.log_lines;
    raw.drain(..raw.len().saturating_sub(limits.log_lines));
    let lines = raw
        .into_iter()
        .map(|mut line| {
            let peer = line.fields.remove("peer");
            let site = line.fields.remove("site");
            let mut text = line.msg;
            for (key, value) in &line.fields {
                text.push_str(&format!(" {key}={value}"));
            }
            LogLine {
                peer,
                site,
                time: line.time,
                text: r.redact(&text).head(limits.log_line_bytes),
            }
        })
        .collect();
    Logs {
        start,
        end,
        lines,
        truncated,
    }
}

fn build_changes(
    r: &mut Redactor,
    limits: &Limits,
    commands: Vec<sources::CommandRow>,
    campaigns: Vec<sources::CampaignRow>,
    since: DateTime<Utc>,
) -> Changes {
    let commands_omitted = omitted(commands.first().map(|c| c.total), commands.len());
    let campaigns_omitted = omitted(campaigns.first().map(|c| c.total), campaigns.len());
    let text = limits.text_bytes;
    let output = limits.output_bytes;
    let commands = commands
        .into_iter()
        .map(|c| CommandEntry {
            parameters: r
                .redact(&crate::api::commands::parameters(&c.kind, &c.payload).to_string())
                .head(text),
            error: c.error.as_deref().map(|t| r.redact(t).head(text)),
            stdout: non_empty(c.stdout).map(|t| r.redact(&t).tail(output)),
            stderr: non_empty(c.stderr).map(|t| r.redact(&t).tail(output)),
            id: c.id,
            server_id: c.server_id,
            kind: c.kind,
            state: c.state,
            issued_at: c.issued_at,
            finished_at: c.finished_at,
            exit_code: c.exit_code,
            campaign_id: c.campaign_id,
        })
        .collect();
    let campaigns = campaigns
        .into_iter()
        .map(|c| CampaignEntry {
            name: r.redact(&c.name).head(text),
            packages: r.redact(&c.packages.join(" ")).head(text),
            error: c.error.as_deref().map(|t| r.redact(t).head(text)),
            id: c.id,
            customer_id: c.customer_id,
            state: c.state,
            wave: c.wave,
            security_only: c.security_only,
            allow_reboot: c.allow_reboot,
            created_at: c.created_at,
            started_at: c.started_at,
            finished_at: c.finished_at,
        })
        .collect();
    Changes {
        since,
        commands,
        commands_omitted,
        campaigns,
        campaigns_omitted,
    }
}

fn non_empty(text: Option<String>) -> Option<String> {
    text.filter(|t| !t.trim().is_empty())
}

impl Context {
    /// Asserts that every part of the context belongs to [`Self::owner`]:
    /// the breach and commands to its server, series and log lines to its
    /// `peer` and `site`, campaigns to its customer.
    pub fn check_owner(&self) -> Result<(), ContextError> {
        let owner = &self.owner;
        let foreign = |section, detail: String| Err(ContextError::ForeignData { section, detail });
        if self.incident.server_id != owner.server_id {
            return foreign(
                "incident",
                format!("incident of server {}", self.incident.server_id),
            );
        }
        let labels_match = |peer: &Option<String>, site: &Option<String>| {
            peer.as_deref() == Some(owner.peer.as_str())
                && site.as_deref() == Some(owner.site.as_str())
        };
        if let Section::Ready(history) = &self.history {
            if let Some(s) = history
                .series
                .iter()
                .find(|s| !labels_match(&s.peer, &s.site))
            {
                return foreign("history", format!("series of peer {:?}", s.peer));
            }
        }
        if let Section::Ready(logs) = &self.logs {
            if let Some(l) = logs.lines.iter().find(|l| !labels_match(&l.peer, &l.site)) {
                return foreign("logs", format!("line of peer {:?}", l.peer));
            }
        }
        if let Section::Ready(changes) = &self.changes {
            if let Some(c) = changes
                .commands
                .iter()
                .find(|c| c.server_id != owner.server_id)
            {
                return foreign(
                    "changes",
                    format!("command {} of server {}", c.id, c.server_id),
                );
            }
            if let Some(c) = changes
                .campaigns
                .iter()
                .find(|c| c.customer_id != owner.customer_id)
            {
                return foreign(
                    "changes",
                    format!("campaign {} of customer {}", c.id, c.customer_id),
                );
            }
        }
        Ok(())
    }
}
