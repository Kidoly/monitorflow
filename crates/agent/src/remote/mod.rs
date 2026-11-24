//! Remote terminals opened from the web interface (ADR-0178).
//!
//! The central server sends a signed `remote_session` command. Its handler
//! refuses it unless the host's administrator set `remote.terminal` in
//! `agent.toml`, then answers with its half of the session's keys, signed
//! with the agent's certificate key, starts a shell on a pseudo-terminal as
//! the agent's own user (or, on Linux, as `remote.terminal_user` through
//! the root helper of [`helper`]), and returns: the session goes on in a task of its
//! own, so the command dispatcher is free again.
//!
//! Frames come and go on the agent's stream as `RemoteFrame` envelopes,
//! sealed end to end ([`monitorflow_common::remote`]). The session ends
//! when the shell exits, when the server or the person closes it, after
//! the idle time the command gives without input, after
//! [`MAX_DURATION`], and when the stream to the central server drops,
//! since sealed frames lost on the way cannot be sent again.
//!
//! While a session is open, people logged in on the host are told who
//! opened it and whether it is recorded (`wall` on Unix, `msg` on Windows),
//! and again when it closes. The agent's log says it too.

#[cfg(unix)]
pub mod helper;
#[cfg(unix)]
mod pty_unix;
#[cfg(windows)]
mod pty_windows;
#[cfg(test)]
mod tests;

use std::collections::HashMap;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use monitorflow_common::client::identity::StoredIdentity;
use monitorflow_common::proto::v1::{
    remote_data, remote_frame, CommandAcceptance, RemoteClosed, RemoteData, RemoteFrame,
    RemoteOpened, RemoteSessionOpen,
};
use monitorflow_common::remote::{self as sealing, Channel, MAX_CHUNK, TERMINAL};
use prost::Message;
use tokio::sync::{mpsc, watch};
use tokio::time::{sleep_until, Instant};

use crate::commands::{Handler, Output};

/// Sessions open at once on one agent.
pub const MAX_SESSIONS: usize = 4;
/// A session closes after this long, whatever happens in it.
pub const MAX_DURATION: Duration = Duration::from_secs(12 * 60 * 60);
/// Output bytes a session sends per second at most, so a flood does not
/// hold the stream up for the samples and results that share it.
const OUTPUT_PER_SECOND: usize = 256 * 1024;
/// Frames waiting for the stream, from every session.
const UP_QUEUE: usize = 64;
/// Frames from the server waiting for one session. More close it: the
/// server sends keystrokes far slower than a shell reads them.
const INBOX: usize = 256;

/// Why `remote_session` is refused while `remote.terminal` is off, with
/// the way to allow it on this platform.
#[cfg(unix)]
pub const DISABLED: &str = "remote terminals are not allowed on this server (remote.terminal); \
                            run the installer with MF_REMOTE_TERMINAL=1, or `monitorflow-agent \
                            remote-access terminal on` as root, to allow them";
#[cfg(windows)]
pub const DISABLED: &str = "remote terminals are not allowed on this server (remote.terminal); \
                            run `monitorflow-agent.exe remote-access terminal on` as an \
                            administrator and restart the MonitorFlowAgent service to allow them";

/// What a terminal is started with.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Spec {
    pub session_id: String,
    pub requested_by: String,
    pub cols: u16,
    pub rows: u16,
    /// The command as the central server signed it, framed for a root
    /// helper ([`crate::helper_trust::HelperRequest`]), which checks it
    /// itself before it starts a shell as another account.
    pub request: Vec<u8>,
}

/// A shell on a pseudo-terminal: what it prints, and how to type into it,
/// resize it and end it.
pub struct Terminal {
    /// Chunks of at most [`MAX_CHUNK`] bytes; closed when the shell exits.
    pub output: mpsc::Receiver<Vec<u8>>,
    pub input: std::sync::mpsc::Sender<Vec<u8>>,
    pub control: Box<dyn Control>,
}

pub trait Control: Send {
    fn resize(&self, cols: u16, rows: u16) -> std::io::Result<()>;
    /// Ends the shell and what it started; must not block.
    fn kill(&mut self);
}

/// Starts terminals: the platform's own, or a fake in tests.
pub type Spawner = Arc<dyn Fn(&Spec) -> std::io::Result<Terminal> + Send + Sync>;

/// The platform's terminal: a pseudo-terminal on Unix, a pseudo console on
/// Windows. On Linux, with `user`, the shell is that account's, started by
/// the root helper `remote-shell`; without, the agent's own user's.
pub fn platform_spawner(user: Option<String>) -> Spawner {
    #[cfg(unix)]
    return match user {
        Some(_) => Arc::new(pty_unix::spawn_helper),
        None => Arc::new(pty_unix::spawn),
    };
    #[cfg(windows)]
    {
        if let Some(user) = user {
            tracing::warn!(%user, "remote.terminal_user is for Linux; Windows terminals run as the agent's account");
        }
        Arc::new(pty_windows::spawn)
    }
}

/// What proves the session's keys are this agent's: its certificate and
/// a signature with its key.
pub trait Proof: Send + Sync {
    fn certificate(&self) -> Vec<u8>;
    fn sign(&self, message: &[u8]) -> Option<Vec<u8>>;
}

/// The current identity, as the renewal task changes it.
impl Proof for watch::Receiver<StoredIdentity> {
    fn certificate(&self) -> Vec<u8> {
        self.borrow().tls.leaf().to_vec()
    }

    fn sign(&self, message: &[u8]) -> Option<Vec<u8>> {
        self.borrow().tls.sign(message)
    }
}

/// Tells people on the host about a session, best effort.
pub type Announce = Arc<dyn Fn(String) + Send + Sync>;

/// `wall` on Unix, `msg *` on Windows, and the agent's log.
pub fn platform_announce() -> Announce {
    Arc::new(|message: String| {
        tracing::warn!(target: "monitorflow::remote", "{message}");
        tokio::spawn(async move {
            #[cfg(unix)]
            let mut command = {
                let mut command = tokio::process::Command::new("wall");
                command.stdin(std::process::Stdio::piped());
                command
            };
            #[cfg(windows)]
            let mut command = {
                let mut command = tokio::process::Command::new("msg.exe");
                command.args(["*", "/TIME:60", &message]);
                command.stdin(std::process::Stdio::null());
                command
            };
            command
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .kill_on_drop(true);
            let Ok(mut child) = command.spawn() else {
                tracing::debug!("cannot tell logged-in users about the remote session");
                return;
            };
            #[cfg(unix)]
            if let Some(mut stdin) = child.stdin.take() {
                use tokio::io::AsyncWriteExt;
                let _ = stdin.write_all(format!("{message}\n").as_bytes()).await;
            }
            let _ = tokio::time::timeout(Duration::from_secs(10), child.wait()).await;
        });
    })
}

/// What the sessions share.
struct Shared {
    enabled: bool,
    proof: Box<dyn Proof>,
    spawner: Spawner,
    announce: Announce,
    up: mpsc::Sender<RemoteFrame>,
    sessions: Mutex<HashMap<String, mpsc::Sender<Inbound>>>,
}

impl Shared {
    fn sessions(&self) -> std::sync::MutexGuard<'_, HashMap<String, mpsc::Sender<Inbound>>> {
        self.sessions.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

/// What reaches a session.
enum Inbound {
    Frame(RemoteFrame),
    /// The stream to the central server dropped.
    Lost,
}

/// The sessions of this agent.
#[derive(Clone)]
pub struct Remote {
    shared: Arc<Shared>,
}

/// The stream's side: frames to send up, and where frames from the server
/// go.
pub struct StreamEnd {
    pub up: mpsc::Receiver<RemoteFrame>,
    remote: Remote,
}

impl StreamEnd {
    /// A frame from the server, for the session it names. Frames for no
    /// open session are dropped.
    pub fn deliver(&self, frame: RemoteFrame) {
        let inbox = self
            .remote
            .shared
            .sessions()
            .get(&frame.session_id)
            .cloned();
        match inbox {
            Some(inbox) => {
                if inbox.try_send(Inbound::Frame(frame)).is_err() {
                    tracing::warn!("a remote session is not keeping up with its input");
                }
            }
            None => tracing::debug!("dropping a frame of no open remote session"),
        }
    }

    /// The stream dropped: every session ends.
    pub fn disconnected(&self) {
        for inbox in self.remote.shared.sessions().values() {
            let _ = inbox.try_send(Inbound::Lost);
        }
    }
}

/// The sessions and their stream end. `enabled` is `remote.terminal`.
pub fn link(
    enabled: bool,
    proof: Box<dyn Proof>,
    spawner: Spawner,
    announce: Announce,
) -> (Remote, StreamEnd) {
    let (up, up_rx) = mpsc::channel(UP_QUEUE);
    let remote = Remote {
        shared: Arc::new(Shared {
            enabled,
            proof,
            spawner,
            announce,
            up,
            sessions: Mutex::new(HashMap::new()),
        }),
    };
    let end = StreamEnd {
        up: up_rx,
        remote: remote.clone(),
    };
    (remote, end)
}

/// Why a session did not open.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Refused {
    /// Nothing ran: the command is not acceptable here.
    Rejected(String),
    /// Acceptable, but it could not start.
    Failed(String),
}

impl Remote {
    pub fn enabled(&self) -> bool {
        self.shared.enabled
    }

    /// Sessions open now.
    pub fn open_sessions(&self) -> usize {
        self.shared.sessions().len()
    }

    async fn open(&self, open: RemoteSessionOpen, request: Vec<u8>) -> Result<String, Refused> {
        use Refused::{Failed, Rejected};
        if !self.shared.enabled {
            return Err(Rejected(DISABLED.into()));
        }
        if open.kind != TERMINAL {
            return Err(Rejected(format!(
                "remote sessions of kind `{}` are not supported",
                open.kind.escape_debug()
            )));
        }
        if !valid_session_id(&open.session_id) {
            return Err(Rejected("invalid remote session ID".into()));
        }
        if !sealing::valid_size(open.cols, open.rows) {
            return Err(Rejected("invalid terminal size".into()));
        }
        let idle = open
            .idle_timeout_seconds
            .clamp(sealing::MIN_IDLE_SECONDS, sealing::MAX_IDLE_SECONDS);
        let requested_by: String = open
            .requested_by
            .chars()
            .filter(|c| !c.is_control())
            .take(64)
            .collect();
        let (agent_key, channel) = sealing::agent_handshake(&open.session_id, &open.server_key)
            .map_err(|e| Rejected(e.to_string()))?;
        let message = sealing::opened_signed_bytes(&open.session_id, &open.server_key, &agent_key);
        let signature = self.shared.proof.sign(&message).ok_or_else(|| {
            Failed("this agent's key cannot sign the session's keys (ECDSA P-256 only)".into())
        })?;
        let (inbox, inbox_rx) = mpsc::channel(INBOX);
        {
            let mut sessions = self.shared.sessions();
            if sessions.contains_key(&open.session_id) {
                return Err(Rejected("this remote session is already open".into()));
            }
            if sessions.len() >= MAX_SESSIONS {
                return Err(Failed(format!(
                    "{MAX_SESSIONS} remote sessions are open on this server already"
                )));
            }
            sessions.insert(open.session_id.clone(), inbox);
        }
        let spec = Spec {
            session_id: open.session_id.clone(),
            requested_by: requested_by.clone(),
            cols: open.cols as u16,
            rows: open.rows as u16,
            request,
        };
        let spawner = self.shared.spawner.clone();
        let started = tokio::task::spawn_blocking(move || spawner(&spec))
            .await
            .map_err(|e| e.to_string())
            .and_then(|r| r.map_err(|e| e.to_string()));
        let terminal = match started {
            Ok(terminal) => terminal,
            Err(error) => {
                self.shared.sessions().remove(&open.session_id);
                return Err(Failed(format!("cannot start a shell: {error}")));
            }
        };
        let opened = RemoteFrame {
            session_id: open.session_id.clone(),
            body: Some(remote_frame::Body::Opened(RemoteOpened {
                agent_key: agent_key.to_vec(),
                certificate: self.shared.proof.certificate(),
                signature,
            })),
            counter: 0,
        };
        let recorded = if open.recorded {
            "; it is recorded"
        } else {
            ""
        };
        (self.shared.announce)(format!(
            "MonitorFlow: {requested_by} opened a remote terminal on this server from the web \
             interface{recorded}."
        ));
        let session = Session {
            id: open.session_id.clone(),
            requested_by: requested_by.clone(),
            channel,
            terminal,
            inbox: inbox_rx,
            idle: Duration::from_secs(idle.into()),
            shared: self.shared.clone(),
        };
        if self.shared.up.send(opened).await.is_err() {
            self.shared.sessions().remove(&open.session_id);
            return Err(Failed("the agent is shutting down".into()));
        }
        tracing::info!(session_id = %open.session_id, requested_by = %requested_by, "remote terminal opened");
        tokio::spawn(session.run());
        Ok(format!("remote terminal opened for {requested_by}\n"))
    }
}

/// A UUID, as the server makes them.
fn valid_session_id(id: &str) -> bool {
    id.len() == 36
        && id.char_indices().all(|(i, c)| match i {
            8 | 13 | 18 | 23 => c == '-',
            _ => c.is_ascii_hexdigit(),
        })
}

/// One open session.
struct Session {
    id: String,
    requested_by: String,
    channel: Channel,
    terminal: Terminal,
    inbox: mpsc::Receiver<Inbound>,
    idle: Duration,
    shared: Arc<Shared>,
}

impl Session {
    async fn run(mut self) {
        let started = Instant::now();
        let mut idle_at = started + self.idle;
        let ends_at = started + MAX_DURATION;
        let mut window = (Instant::now(), 0usize);
        let reason = loop {
            tokio::select! {
                inbound = self.inbox.recv() => match inbound {
                    Some(Inbound::Frame(frame)) => match self.receive(frame) {
                        Ok(true) => idle_at = Instant::now() + self.idle,
                        Ok(false) => {}
                        Err(reason) => break reason,
                    },
                    Some(Inbound::Lost) | None => {
                        break "the connection to the central server was lost".to_owned()
                    }
                },
                output = self.terminal.output.recv() => match output {
                    Some(bytes) => {
                        let sent = bytes.len();
                        if !self.send(sealing::output(bytes)).await {
                            break "the agent is shutting down".to_owned();
                        }
                        pace(&mut window, sent).await;
                    }
                    None => {
                        self.send(sealing::close("the shell exited")).await;
                        break "the shell exited".to_owned();
                    }
                },
                () = sleep_until(idle_at) => {
                    let reason = format!(
                        "closed after {} minutes without input",
                        self.idle.as_secs() / 60
                    );
                    self.send(sealing::close(reason.clone())).await;
                    break reason;
                }
                () = sleep_until(ends_at) => {
                    let reason = format!(
                        "closed after {} hours, the longest a session lasts",
                        MAX_DURATION.as_secs() / 3600
                    );
                    self.send(sealing::close(reason.clone())).await;
                    break reason;
                }
            }
        };
        self.terminal.control.kill();
        self.shared.sessions().remove(&self.id);
        let _ = self.shared.up.try_send(RemoteFrame {
            session_id: self.id.clone(),
            body: Some(remote_frame::Body::Closed(RemoteClosed {
                reason: reason.clone(),
            })),
            counter: 0,
        });
        (self.shared.announce)(format!(
            "MonitorFlow: the remote terminal {} opened is closed.",
            self.requested_by
        ));
        tracing::info!(session_id = %self.id, %reason, minutes = started.elapsed().as_secs() / 60, "remote terminal closed");
    }

    /// Applies a frame from the server: `Ok(true)` for input, `Err` with
    /// why the session must end.
    fn receive(&mut self, frame: RemoteFrame) -> Result<bool, String> {
        let sealed = match frame.body {
            Some(remote_frame::Body::Sealed(sealed)) => sealed,
            Some(remote_frame::Body::Closed(closed)) => {
                return Err(closed_reason(
                    &closed.reason,
                    "closed by the central server",
                ))
            }
            Some(remote_frame::Body::Opened(_)) | None => return Ok(false),
        };
        let data = self
            .channel
            .open(frame.counter, &sealed)
            .map_err(|e| format!("closed: {e}"))?;
        match data.body {
            Some(remote_data::Body::Input(bytes)) => {
                for chunk in bytes.chunks(MAX_CHUNK) {
                    if self.terminal.input.send(chunk.to_vec()).is_err() {
                        return Err("the shell exited".into());
                    }
                }
                Ok(true)
            }
            Some(remote_data::Body::Resize(size)) => {
                if sealing::valid_size(size.cols, size.rows) {
                    let _ = self
                        .terminal
                        .control
                        .resize(size.cols as u16, size.rows as u16);
                }
                Ok(false)
            }
            Some(remote_data::Body::Close(reason)) => {
                Err(closed_reason(&reason, "closed from the web interface"))
            }
            Some(remote_data::Body::Output(_)) | None => Ok(false),
        }
    }

    /// Seals and queues `data`; false once the stream end is gone.
    async fn send(&mut self, data: RemoteData) -> bool {
        let (counter, sealed) = self.channel.seal(&data);
        self.shared
            .up
            .send(RemoteFrame {
                session_id: self.id.clone(),
                body: Some(remote_frame::Body::Sealed(sealed)),
                counter,
            })
            .await
            .is_ok()
    }
}

/// A reason from the other side, bounded, or `fallback`.
fn closed_reason(reason: &str, fallback: &str) -> String {
    let reason: String = reason
        .chars()
        .filter(|c| !c.is_control())
        .take(200)
        .collect();
    if reason.is_empty() {
        fallback.to_owned()
    } else {
        reason
    }
}

/// Waits when this second's output passed [`OUTPUT_PER_SECOND`].
async fn pace(window: &mut (Instant, usize), sent: usize) {
    let now = Instant::now();
    if now.duration_since(window.0) >= Duration::from_secs(1) {
        *window = (now, 0);
    }
    window.1 += sent;
    if window.1 >= OUTPUT_PER_SECOND {
        sleep_until(window.0 + Duration::from_secs(1)).await;
        *window = (Instant::now(), 0);
    }
}

/// Runs `remote_session` commands.
pub struct RemoteSessionHandler {
    remote: Remote,
}

impl RemoteSessionHandler {
    pub fn new(remote: Remote) -> Self {
        Self { remote }
    }
}

#[async_trait::async_trait]
impl Handler for RemoteSessionHandler {
    fn acceptance(&self) -> CommandAcceptance {
        if self.remote.enabled() {
            CommandAcceptance::Accepted
        } else {
            CommandAcceptance::Disabled
        }
    }

    async fn run(&self, payload: &[u8]) -> Output {
        self.run_command(&monitorflow_common::proto::v1::Command {
            payload: payload.to_vec(),
            ..Default::default()
        })
        .await
    }

    async fn run_command(&self, command: &monitorflow_common::proto::v1::Command) -> Output {
        let Ok(open) = RemoteSessionOpen::decode(command.payload.as_slice()) else {
            return Output::rejected("invalid RemoteSessionOpen payload");
        };
        let request = crate::helper_trust::HelperRequest::of(command).framed();
        match self.remote.open(open, request).await {
            Ok(message) => Output {
                stdout: message.into_bytes(),
                ..Output::default()
            },
            Err(Refused::Rejected(error)) => Output::rejected(error),
            Err(Refused::Failed(error)) => Output {
                exit_code: 1,
                error: Some(error),
                ..Output::default()
            },
        }
    }
}
