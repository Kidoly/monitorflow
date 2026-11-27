//! Remote terminals and desktops opened from the web interface
//! (ADR-0190).
//!
//! The central server sends a signed `remote_session` command. Its handler
//! refuses it unless the host's administrator set `remote.terminal` in
//! `agent.toml`, then answers with its half of the session's keys, signed
//! with the agent's certificate key, starts a shell on a pseudo-terminal as
//! the agent's own user (or, on Linux, as `remote.terminal_user` through
//! the root helper of [`helper`]), and returns: the session goes on in a task of its
//! own, so the command dispatcher is free again.
//!
//! A remote desktop is the same session with another consent,
//! `remote.desktop`, and another command kind, `remote_desktop`: instead
//! of a shell, the agent connects to the host's own RDP service on
//! `127.0.0.1:3389` ([`desktop`]), never another address, and carries its
//! bytes both ways. The central server speaks RDP (TLS included) with it
//! inside the sealed frames. Its output waits for the server's
//! `delivered` once [`DESKTOP_WINDOW`] frames are in flight.
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

pub mod desktop;
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
use monitorflow_common::remote::{
    self as sealing, Channel, DESKTOP, DESKTOP_WINDOW, MAX_CHUNK, TERMINAL,
};
use prost::Message;
use tokio::sync::{mpsc, watch};
use tokio::time::{sleep_until, Instant};

use crate::commands::{Handler, Output};

/// Sessions open at once on one agent.
pub const MAX_SESSIONS: usize = 4;
/// A session closes after this long, whatever happens in it.
pub const MAX_DURATION: Duration = Duration::from_secs(12 * 60 * 60);
/// Output bytes a terminal sends per second at most, so a flood does not
/// hold the stream up for the samples and results that share it.
const OUTPUT_PER_SECOND: usize = 256 * 1024;
/// The same for a desktop, whose screen updates need more.
const DESKTOP_PER_SECOND: usize = 2 * 1024 * 1024;
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

/// Why `remote_desktop` is refused while `remote.desktop` is off.
pub const DESKTOP_DISABLED: &str =
    "remote desktops are not allowed on this server (remote.desktop); run \
     `monitorflow-agent.exe remote-access desktop on` as an administrator and restart the \
     MonitorFlowAgent service, or install with REMOTE_DESKTOP=1, to allow them";

/// The two kinds of session, each with its own consent and command kind.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    /// A shell, `remote_session` commands, `remote.terminal`.
    Terminal,
    /// The host's RDP service, `remote_desktop` commands,
    /// `remote.desktop`.
    Desktop,
}

impl Kind {
    /// `RemoteSessionOpen.kind`.
    pub fn name(self) -> &'static str {
        match self {
            Kind::Terminal => TERMINAL,
            Kind::Desktop => DESKTOP,
        }
    }

    /// In messages: "remote terminal".
    fn what(self) -> &'static str {
        match self {
            Kind::Terminal => "remote terminal",
            Kind::Desktop => "remote desktop",
        }
    }

    fn disabled(self) -> &'static str {
        match self {
            Kind::Terminal => DISABLED,
            Kind::Desktop => DESKTOP_DISABLED,
        }
    }

    fn per_second(self) -> usize {
        match self {
            Kind::Terminal => OUTPUT_PER_SECOND,
            Kind::Desktop => DESKTOP_PER_SECOND,
        }
    }

    /// Why the session ends when its output closes.
    fn ended(self) -> &'static str {
        match self {
            Kind::Terminal => "the shell exited",
            Kind::Desktop => "the remote desktop service closed the connection",
        }
    }
}

/// What the host's administrator allowed, in `agent.toml`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Allowed {
    pub terminal: bool,
    pub desktop: bool,
}

/// How each kind starts: a shell, and the connection to the RDP service.
pub struct Spawners {
    pub terminal: Spawner,
    pub desktop: Spawner,
}

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
/// resize it and end it. A desktop's connection is one too, without sizes.
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
    allowed: Allowed,
    proof: Box<dyn Proof>,
    spawners: Spawners,
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

/// The sessions and their stream end. `allowed` is `remote.terminal` and
/// `remote.desktop`.
pub fn link(
    allowed: Allowed,
    proof: Box<dyn Proof>,
    spawners: Spawners,
    announce: Announce,
) -> (Remote, StreamEnd) {
    let (up, up_rx) = mpsc::channel(UP_QUEUE);
    let remote = Remote {
        shared: Arc::new(Shared {
            allowed,
            proof,
            spawners,
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
    /// Whether the host allowed sessions of `kind`.
    pub fn enabled(&self, kind: Kind) -> bool {
        match kind {
            Kind::Terminal => self.shared.allowed.terminal,
            Kind::Desktop => self.shared.allowed.desktop,
        }
    }

    /// Sessions open now.
    pub fn open_sessions(&self) -> usize {
        self.shared.sessions().len()
    }

    async fn open(
        &self,
        kind: Kind,
        open: RemoteSessionOpen,
        request: Vec<u8>,
    ) -> Result<String, Refused> {
        use Refused::{Failed, Rejected};
        if !self.enabled(kind) {
            return Err(Rejected(kind.disabled().into()));
        }
        // The kind is signed with the command: a terminal's consent never
        // opens a desktop, nor the other way round.
        if open.kind != kind.name() {
            return Err(Rejected(format!(
                "remote sessions of kind `{}` are not supported by this command",
                open.kind.escape_debug()
            )));
        }
        if !valid_session_id(&open.session_id) {
            return Err(Rejected("invalid remote session ID".into()));
        }
        if kind == Kind::Terminal && !sealing::valid_size(open.cols, open.rows) {
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
        let spawner = match kind {
            Kind::Terminal => self.shared.spawners.terminal.clone(),
            Kind::Desktop => self.shared.spawners.desktop.clone(),
        };
        let started = tokio::task::spawn_blocking(move || spawner(&spec))
            .await
            .map_err(|e| e.to_string())
            .and_then(|r| r.map_err(|e| e.to_string()));
        let terminal = match started {
            Ok(terminal) => terminal,
            Err(error) => {
                self.shared.sessions().remove(&open.session_id);
                return Err(Failed(match kind {
                    Kind::Terminal => format!("cannot start a shell: {error}"),
                    Kind::Desktop => format!(
                        "cannot reach the remote desktop service on {}: {error}; is Remote \
                         Desktop turned on in the server's settings?",
                        desktop::RDP_ADDRESS
                    ),
                }));
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
            "MonitorFlow: {requested_by} opened a {} on this server from the web \
             interface{recorded}.",
            kind.what()
        ));
        let session = Session {
            kind,
            window: (kind == Kind::Desktop).then_some(Window::default()),
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
        tracing::info!(session_id = %open.session_id, requested_by = %requested_by, kind = kind.name(), "remote session opened");
        tokio::spawn(session.run());
        Ok(format!("{} opened for {requested_by}\n", kind.what()))
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

/// A desktop's output frames: sent, and passed on to the browser as the
/// server last said.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
struct Window {
    sent: u64,
    delivered: u64,
}

impl Window {
    fn open(&self) -> bool {
        self.sent - self.delivered < DESKTOP_WINDOW
    }
}

/// One open session.
struct Session {
    kind: Kind,
    /// Desktops only: output waits while it is full.
    window: Option<Window>,
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
                output = self.terminal.output.recv(), if self.window.is_none_or(|w| w.open()) => match output {
                    Some(bytes) => {
                        let sent = bytes.len();
                        if !self.send(sealing::output(bytes)).await {
                            break "the agent is shutting down".to_owned();
                        }
                        if let Some(window) = &mut self.window {
                            window.sent += 1;
                        }
                        pace(&mut window, sent, self.kind.per_second()).await;
                    }
                    None => {
                        let reason = self.kind.ended();
                        self.send(sealing::close(reason)).await;
                        break reason.to_owned();
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
            "MonitorFlow: the {} {} opened is closed.",
            self.kind.what(),
            self.requested_by
        ));
        tracing::info!(session_id = %self.id, %reason, kind = self.kind.name(), minutes = started.elapsed().as_secs() / 60, "remote session closed");
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
                        return Err(self.kind.ended().into());
                    }
                }
                Ok(true)
            }
            Some(remote_data::Body::Delivered(frames)) => {
                if let Some(window) = &mut self.window {
                    // Never beyond what was sent, never backwards.
                    window.delivered = frames.clamp(window.delivered, window.sent);
                }
                Ok(false)
            }
            Some(remote_data::Body::Resize(size)) => {
                if self.kind == Kind::Terminal && sealing::valid_size(size.cols, size.rows) {
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

/// Waits when this second's output passed `per_second`.
async fn pace(window: &mut (Instant, usize), sent: usize, per_second: usize) {
    let now = Instant::now();
    if now.duration_since(window.0) >= Duration::from_secs(1) {
        *window = (now, 0);
    }
    window.1 += sent;
    if window.1 >= per_second {
        sleep_until(window.0 + Duration::from_secs(1)).await;
        *window = (Instant::now(), 0);
    }
}

/// Runs `remote_session` commands, or `remote_desktop` ones.
pub struct RemoteSessionHandler {
    remote: Remote,
    kind: Kind,
}

impl RemoteSessionHandler {
    /// `remote_session`: terminals.
    pub fn new(remote: Remote) -> Self {
        Self {
            remote,
            kind: Kind::Terminal,
        }
    }

    /// `remote_desktop`: the host's RDP service.
    pub fn desktop(remote: Remote) -> Self {
        Self {
            remote,
            kind: Kind::Desktop,
        }
    }
}

#[async_trait::async_trait]
impl Handler for RemoteSessionHandler {
    fn acceptance(&self) -> CommandAcceptance {
        if self.remote.enabled(self.kind) {
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
        match self.remote.open(self.kind, open, request).await {
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
