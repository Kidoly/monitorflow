//! Commands from the server (ADR-0031): checks, the registry of handlers by
//! kind, and the dispatcher task that runs them one at a time.
//!
//! # Adding a kind
//!
//! Implement [`Handler`] and register it before starting the dispatcher:
//!
//! ```ignore
//! let mut handlers = Handlers::default();
//! handlers.register("configure_sources", Arc::new(ConfigureSources::new(..)));
//! ```
//!
//! A handler gets the payload bytes, the serialized message of its kind,
//! and returns an [`Output`]: [`Output::rejected`] for a payload it cannot
//! decode or accept (nothing ran), else the exit code and output of what
//! ran. It must not block the runtime and should kill what it started when
//! dropped: the dispatcher drops it at [`DispatcherSettings::run_timeout`].
//!
//! # Checks
//!
//! In order, without running anything when one fails: the signature and
//! the target agent (`REJECTED`), the journal (a command already finished
//! is answered with its stored result, one interrupted by a restart with
//! `FAILED`), the expiry (`EXPIRED`), then the kind (`REJECTED`).

pub mod journal;
#[cfg(unix)]
pub mod shell;
pub mod unsupported;

use std::collections::{HashMap, VecDeque};
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use monitorflow_common::command;
use monitorflow_common::proto::v1::{Command, CommandResult, CommandStatus};
use rustls::pki_types::CertificateDer;
use tokio::sync::mpsc;
use tokio::task::JoinHandle;

use crate::shutdown::Shutdown;
pub use journal::Journal;
use journal::{valid_id, Entry};

/// Longest `stdout` or `stderr` sent, in bytes; the end is kept.
pub const MAX_OUTPUT: usize = 64 * 1024;
/// Commands from the stream waiting for the dispatcher. More are dropped;
/// the server sends them again.
const COMMAND_QUEUE: usize = 16;
/// Results waiting for a stream. More are dropped; the server sends the
/// command again and the journal answers it.
const RESULT_QUEUE: usize = 64;

/// What a handler did.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Output {
    pub exit_code: i32,
    pub stdout: Vec<u8>,
    pub stderr: Vec<u8>,
    /// Why it failed, in one line. Set with a zero exit code, it still
    /// fails.
    pub error: Option<String>,
    /// Nothing ran: the payload was refused.
    pub rejected: bool,
}

impl Output {
    /// Refuses the command without running anything.
    pub fn rejected(error: impl Into<String>) -> Self {
        Self {
            exit_code: -1,
            error: Some(error.into()),
            rejected: true,
            ..Self::default()
        }
    }

    fn status(&self) -> CommandStatus {
        match self {
            Self { rejected: true, .. } => CommandStatus::Rejected,
            Self {
                exit_code: 0,
                error: None,
                ..
            } => CommandStatus::Succeeded,
            _ => CommandStatus::Failed,
        }
    }
}

/// Runs the commands of one kind.
#[async_trait::async_trait]
pub trait Handler: Send + Sync + 'static {
    async fn run(&self, payload: &[u8]) -> Output;
}

/// Handlers by kind.
#[derive(Clone, Default)]
pub struct Handlers {
    by_kind: HashMap<String, Arc<dyn Handler>>,
}

impl Handlers {
    /// Runs commands of `kind` with `handler`, replacing any previous one.
    pub fn register(&mut self, kind: &str, handler: Arc<dyn Handler>) -> &mut Self {
        self.by_kind.insert(kind.to_owned(), handler);
        self
    }

    pub fn kinds(&self) -> impl Iterator<Item = &str> {
        self.by_kind.keys().map(String::as_str)
    }
}

/// The stream's side: commands go to the dispatcher, results come back.
pub struct StreamEnd {
    pub commands: mpsc::Sender<Command>,
    pub results: mpsc::Receiver<CommandResult>,
}

/// The dispatcher's side of [`link`].
pub struct DispatcherEnd {
    commands: mpsc::Receiver<Command>,
    results: mpsc::Sender<CommandResult>,
}

pub fn link() -> (StreamEnd, DispatcherEnd) {
    let (commands_tx, commands_rx) = mpsc::channel(COMMAND_QUEUE);
    let (results_tx, results_rx) = mpsc::channel(RESULT_QUEUE);
    (
        StreamEnd {
            commands: commands_tx,
            results: results_rx,
        },
        DispatcherEnd {
            commands: commands_rx,
            results: results_tx,
        },
    )
}

#[derive(Debug, Clone)]
pub struct DispatcherSettings {
    /// A handler still running after this long is dropped and the command
    /// fails. Below the server's two hours (ADR-0031).
    pub run_timeout: Duration,
}

impl Default for DispatcherSettings {
    fn default() -> Self {
        Self {
            run_timeout: Duration::from_secs(60 * 60),
        }
    }
}

/// What to do with a command that arrived.
#[derive(Debug, PartialEq)]
enum Verdict {
    Run,
    Answer(CommandResult),
    /// A copy of a command already waiting.
    Ignore,
}

/// The command running now.
struct Running {
    command_id: String,
    expires_at_unix_ms: i64,
    started_at_unix_ms: i64,
    task: JoinHandle<Output>,
}

/// Checks and runs commands, one at a time.
pub struct Dispatcher {
    agent_id: String,
    ca_certs: Vec<CertificateDer<'static>>,
    handlers: Handlers,
    journal: Journal,
    settings: DispatcherSettings,
}

impl Dispatcher {
    /// `ca_certs` are the CA certificates the agent trusts; commands must
    /// be signed by one of their keys.
    pub fn new(
        agent_id: &str,
        ca_certs: Vec<CertificateDer<'static>>,
        handlers: Handlers,
        journal: Journal,
        settings: DispatcherSettings,
    ) -> Self {
        Self {
            agent_id: agent_id.to_owned(),
            ca_certs,
            handlers,
            journal,
            settings,
        }
    }

    /// Runs until `shutdown`. A command still running then is abandoned;
    /// its journal entry makes the next start report it interrupted.
    pub async fn run(self, mut end: DispatcherEnd, mut shutdown: Shutdown) {
        self.journal.prune(unix_ms()).await;
        let mut running: Option<Running> = None;
        let mut waiting: VecDeque<Command> = VecDeque::new();
        loop {
            tokio::select! {
                () = shutdown.wait() => return,
                command = end.commands.recv() => {
                    let Some(command) = command else { return };
                    match self.check(&command, running.as_ref(), &waiting).await {
                        Verdict::Answer(result) => send(&end.results, result),
                        Verdict::Ignore => {}
                        Verdict::Run if running.is_none() => {
                            running = self.start(command, &end.results).await;
                        }
                        Verdict::Run => waiting.push_back(command),
                    }
                }
                output = join(&mut running) => {
                    let done = running.take().expect("join resolves only while running");
                    self.finish(done, output, &end.results).await;
                    while running.is_none() {
                        let Some(next) = waiting.pop_front() else { break };
                        // Checked again: it may have expired meanwhile.
                        match self.check(&next, None, &waiting).await {
                            Verdict::Answer(result) => send(&end.results, result),
                            Verdict::Ignore => {}
                            Verdict::Run => running = self.start(next, &end.results).await,
                        }
                    }
                }
            }
        }
    }

    /// Whether `command` may run, or the answer when it must not: refused,
    /// already known, or already running.
    async fn check(
        &self,
        command: &Command,
        running: Option<&Running>,
        waiting: &VecDeque<Command>,
    ) -> Verdict {
        use Verdict::Answer;
        let id = &command.command_id;
        if !valid_id(id) {
            tracing::warn!("rejecting a command with an invalid ID");
            return Answer(not_run(id, CommandStatus::Rejected, "invalid command ID"));
        }
        if let Err(error) = command::verify(command, &self.ca_certs) {
            tracing::warn!(command_id = %id, %error, "rejecting a command");
            return Answer(not_run(id, CommandStatus::Rejected, &error.to_string()));
        }
        if command.agent_id != self.agent_id {
            tracing::warn!(command_id = %id, "rejecting a command for another agent");
            return Answer(not_run(
                id,
                CommandStatus::Rejected,
                "the command is for another agent",
            ));
        }
        if let Some(running) = running.filter(|r| &r.command_id == id) {
            return Answer(CommandResult {
                command_id: id.clone(),
                status: CommandStatus::Running as i32,
                started_at_unix_ms: running.started_at_unix_ms,
                ..CommandResult::default()
            });
        }
        if waiting.iter().any(|c| &c.command_id == id) {
            return Verdict::Ignore;
        }
        match self.journal.get(id).await {
            Ok(Some(Entry {
                result: Some(result),
                ..
            })) => {
                tracing::info!(command_id = %id, "command already ran; sending its result again");
                return Answer(result);
            }
            Ok(Some(entry)) => {
                tracing::warn!(command_id = %id, "command interrupted by an agent restart");
                let mut result =
                    not_run(id, CommandStatus::Failed, "interrupted by an agent restart");
                result.finished_at_unix_ms = unix_ms();
                self.record(id, entry.expires_at_unix_ms, &result).await;
                return Answer(result);
            }
            Ok(None) => {}
            Err(error) => {
                tracing::error!(command_id = %id, %error, "cannot read the command journal");
                return Answer(not_run(
                    id,
                    CommandStatus::Failed,
                    "cannot read the command journal",
                ));
            }
        }
        if unix_ms() >= command.expires_at_unix_ms {
            tracing::info!(command_id = %id, "command arrived after its expiry");
            return Answer(not_run(
                id,
                CommandStatus::Expired,
                "the command expired before it could start",
            ));
        }
        if !self.handlers.by_kind.contains_key(&command.kind) {
            tracing::warn!(command_id = %id, kind = %command.kind.escape_debug(), "rejecting a command of an unknown kind");
            let error = format!("unknown command kind `{}`", command.kind.escape_debug());
            return Answer(not_run(id, CommandStatus::Rejected, &error));
        }
        Verdict::Run
    }

    /// Journals `command` as started, reports it running and spawns its
    /// handler. Without a journal entry it does not run: a redelivery could
    /// not be told apart.
    async fn start(
        &self,
        command: Command,
        results: &mpsc::Sender<CommandResult>,
    ) -> Option<Running> {
        let id = command.command_id.clone();
        let entry = Entry {
            expires_at_unix_ms: command.expires_at_unix_ms,
            result: None,
        };
        if let Err(error) = self.journal.put(&id, &entry).await {
            tracing::error!(command_id = %id, %error, "cannot write the command journal; not running the command");
            send(
                results,
                not_run(
                    &id,
                    CommandStatus::Failed,
                    "cannot write the command journal",
                ),
            );
            return None;
        }
        let started_at_unix_ms = unix_ms();
        tracing::info!(command_id = %id, kind = %command.kind, "running command");
        send(
            results,
            CommandResult {
                command_id: id.clone(),
                status: CommandStatus::Running as i32,
                started_at_unix_ms,
                ..CommandResult::default()
            },
        );
        let handler = self.handlers.by_kind[&command.kind].clone();
        let run_timeout = self.settings.run_timeout;
        let task = tokio::spawn(async move {
            match tokio::time::timeout(run_timeout, handler.run(&command.payload)).await {
                Ok(output) => output,
                Err(_) => Output {
                    exit_code: -1,
                    error: Some(format!("timed out after {run_timeout:?}")),
                    ..Output::default()
                },
            }
        });
        Some(Running {
            command_id: id,
            expires_at_unix_ms: entry.expires_at_unix_ms,
            started_at_unix_ms,
            task,
        })
    }

    async fn finish(&self, done: Running, output: Output, results: &mpsc::Sender<CommandResult>) {
        let status = output.status();
        let result = CommandResult {
            command_id: done.command_id.clone(),
            exit_code: output.exit_code,
            stdout: tail(output.stdout),
            stderr: tail(output.stderr),
            started_at_unix_ms: done.started_at_unix_ms,
            finished_at_unix_ms: unix_ms(),
            status: status as i32,
            error: output.error.unwrap_or_default(),
        };
        tracing::info!(command_id = %done.command_id, status = status.as_str_name(), exit_code = result.exit_code, "command finished");
        self.record(&done.command_id, done.expires_at_unix_ms, &result)
            .await;
        send(results, result);
        self.journal.prune(unix_ms()).await;
    }

    async fn record(&self, id: &str, expires_at_unix_ms: i64, result: &CommandResult) {
        let entry = Entry {
            expires_at_unix_ms,
            result: Some(result.clone()),
        };
        if let Err(error) = self.journal.put(id, &entry).await {
            // A redelivery will then report the command interrupted.
            tracing::error!(command_id = %id, %error, "cannot record the command result in the journal");
        }
    }
}

fn send(results: &mpsc::Sender<CommandResult>, result: CommandResult) {
    if results.try_send(result).is_err() {
        tracing::warn!("result queue full; the server will send the command again");
    }
}

/// Resolves with the running command's output; never while idle.
async fn join(running: &mut Option<Running>) -> Output {
    let Some(running) = running else {
        return std::future::pending().await;
    };
    match (&mut running.task).await {
        Ok(output) => output,
        Err(error) => Output {
            exit_code: -1,
            error: Some(format!("the handler failed: {error}")),
            ..Output::default()
        },
    }
}

fn not_run(id: &str, status: CommandStatus, error: &str) -> CommandResult {
    CommandResult {
        command_id: id.to_owned(),
        exit_code: -1,
        status: status as i32,
        error: error.to_owned(),
        ..CommandResult::default()
    }
}

fn tail(mut bytes: Vec<u8>) -> Vec<u8> {
    if bytes.len() > MAX_OUTPUT {
        bytes.drain(..bytes.len() - MAX_OUTPUT);
    }
    bytes
}

fn unix_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_millis() as i64)
}

// Uses the server's CA, which only builds on Unix.
#[cfg(all(test, unix))]
mod tests;
