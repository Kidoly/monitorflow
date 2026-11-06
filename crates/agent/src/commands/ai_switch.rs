//! AI commands turned on and off from the central server (ADR-0098): the
//! `ai_commands` command and the state `shell` commands read.
//!
//! The server's choice is kept in `<data_dir>/ai-commands`, `on` or `off`,
//! which the agent's user can write and which upgrades keep. What it does
//! depends on the host's administrator, who alone sets the configuration:
//!
//! - `off` always applies: turning AI commands off needs no consent.
//! - `on` applies only with `commands.from_web`; without it the command is
//!   refused and a stored `on` is ignored, so taking the consent back
//!   returns to `commands.shell`.
//! - Without a stored choice, `commands.shell` applies.
//!
//! The installer and `ai-commands on|off` remove the file, so a change
//! made on the host itself always wins over the last one from the server.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use monitorflow_common::proto::v1::{AiCommandsSwitch, CommandAcceptance};
use prost::Message;
use tokio::sync::{watch, Mutex};

use super::{Handler, Output};

/// The file in the data directory that holds the server's choice.
pub const STATE_FILE: &str = "ai-commands";

/// Why a server's "on" is refused, with the way to allow it here.
#[cfg(unix)]
const NOT_ALLOWED: &str = "this server does not let AI commands be turned on from the web \
                           interface (commands.from_web); run the installer with \
                           MF_AI_COMMANDS_FROM_WEB=1 to allow it";
#[cfg(windows)]
const NOT_ALLOWED: &str = "this server does not let AI commands be turned on from the web \
                           interface (commands.from_web); run `monitorflow-agent.exe ai-commands \
                           from-web on` as an administrator and restart the MonitorFlowAgent \
                           service to allow it";

/// Whether AI commands are on now, and what changes it.
pub struct AiSwitch {
    /// `commands.shell`.
    local: bool,
    /// `commands.from_web`.
    from_web: bool,
    /// `None` keeps the state in memory only, for tests.
    path: Option<PathBuf>,
    on: AtomicBool,
    /// Held while the file is written, so two writes cannot cross.
    writing: Mutex<()>,
    changed: watch::Sender<()>,
}

impl AiSwitch {
    /// The state from the configuration and the file in `data_dir`.
    pub fn load(local: bool, from_web: bool, data_dir: &Path) -> Self {
        let path = data_dir.join(STATE_FILE);
        let stored = read(&path);
        let switch = Self::new(local, from_web, Some(path), stored);
        if stored.is_some() {
            tracing::info!(
                on = switch.is_on(),
                stored = ?stored,
                from_web,
                "AI commands as last set from the web interface"
            );
        }
        switch
    }

    /// A state that only `ai_commands` commands change, never stored.
    pub fn fixed(on: bool) -> Self {
        Self::new(on, false, None, None)
    }

    fn new(local: bool, from_web: bool, path: Option<PathBuf>, stored: Option<bool>) -> Self {
        Self {
            local,
            from_web,
            path,
            on: AtomicBool::new(effective(local, from_web, stored)),
            writing: Mutex::new(()),
            changed: watch::channel(()).0,
        }
    }

    pub fn is_on(&self) -> bool {
        self.on.load(Ordering::SeqCst)
    }

    /// Whether the server may turn AI commands on.
    pub fn from_web(&self) -> bool {
        self.from_web
    }

    /// Fires each time [`Self::is_on`] changes.
    pub fn subscribe(&self) -> watch::Receiver<()> {
        self.changed.subscribe()
    }

    /// Applies the server's choice: stored first, so it survives restarts,
    /// then in effect. `Err` says why nothing changed.
    pub async fn set(&self, on: bool) -> Result<bool, String> {
        if on && !self.from_web {
            return Err(NOT_ALLOWED.into());
        }
        let _writing = self.writing.lock().await;
        if let Some(path) = self.path.clone() {
            tokio::task::spawn_blocking(move || write(&path, on))
                .await
                .map_err(|e| e.to_string())?
                .map_err(|e| format!("cannot store the choice: {e}"))?;
        }
        let now = effective(self.local, self.from_web, Some(on));
        let before = self.on.swap(now, Ordering::SeqCst);
        if before != now {
            tracing::info!(
                on = now,
                "AI commands turned {} from the web interface",
                word(now)
            );
            self.changed.send_replace(());
        }
        Ok(now)
    }
}

/// What applies: see the module's documentation.
fn effective(local: bool, from_web: bool, stored: Option<bool>) -> bool {
    match stored {
        Some(false) => false,
        Some(true) if from_web => true,
        _ => local,
    }
}

fn word(on: bool) -> &'static str {
    if on {
        "on"
    } else {
        "off"
    }
}

/// The stored choice. A file that cannot be read or holds anything else
/// counts as `off`: a damaged file must not leave commands on.
fn read(path: &Path) -> Option<bool> {
    match std::fs::read_to_string(path) {
        Ok(text) => match text.trim() {
            "on" => Some(true),
            "off" => Some(false),
            _ => {
                tracing::warn!(path = %path.display(), "unreadable AI commands state; AI commands are off");
                Some(false)
            }
        },
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
        Err(error) => {
            tracing::warn!(path = %path.display(), %error, "cannot read the AI commands state; AI commands are off");
            Some(false)
        }
    }
}

/// Writes the choice through a temporary file and a rename, so a crash
/// leaves the old file or the new one.
fn write(path: &Path, on: bool) -> std::io::Result<()> {
    let temporary = path.with_extension("new");
    std::fs::write(&temporary, format!("{}\n", word(on)))?;
    std::fs::rename(&temporary, path)
}

/// Removes the server's choice, so the configuration applies again: what
/// the installer and `ai-commands on|off` do. True when there was one.
pub fn forget(data_dir: &Path) -> std::io::Result<bool> {
    match std::fs::remove_file(data_dir.join(STATE_FILE)) {
        Ok(()) => Ok(true),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(error),
    }
}

/// Runs `ai_commands` commands.
pub struct AiSwitchHandler {
    switch: Arc<AiSwitch>,
}

impl AiSwitchHandler {
    pub fn new(switch: Arc<AiSwitch>) -> Self {
        Self { switch }
    }
}

#[async_trait::async_trait]
impl Handler for AiSwitchHandler {
    /// Accepted when the server may turn AI commands on; `off` otherwise,
    /// though turning them off is still obeyed.
    fn acceptance(&self) -> CommandAcceptance {
        if self.switch.from_web() {
            CommandAcceptance::Accepted
        } else {
            CommandAcceptance::Disabled
        }
    }

    async fn run(&self, payload: &[u8]) -> Output {
        let Ok(request) = AiCommandsSwitch::decode(payload) else {
            return Output::rejected("invalid AiCommandsSwitch payload");
        };
        match self.switch.set(request.on).await {
            Ok(now) => Output {
                stdout: format!("AI commands are {}\n", word(now)).into_bytes(),
                ..Output::default()
            },
            Err(error) if request.on && !self.switch.from_web() => Output::rejected(error),
            Err(error) => Output {
                exit_code: 1,
                error: Some(error),
                ..Output::default()
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn payload(on: bool) -> Vec<u8> {
        AiCommandsSwitch { on }.encode_to_vec()
    }

    fn handler(local: bool, from_web: bool, dir: &Path) -> (Arc<AiSwitch>, AiSwitchHandler) {
        let switch = Arc::new(AiSwitch::load(local, from_web, dir));
        (switch.clone(), AiSwitchHandler::new(switch))
    }

    #[test]
    fn the_server_turns_off_always_and_on_only_with_consent() {
        for local in [false, true] {
            assert!(!effective(local, false, Some(false)));
            assert!(!effective(local, true, Some(false)));
            assert!(effective(local, true, Some(true)));
            assert_eq!(effective(local, false, Some(true)), local);
            assert_eq!(effective(local, true, None), local);
        }
    }

    #[tokio::test]
    async fn without_consent_on_is_refused_and_off_obeyed() {
        let dir = tempfile::tempdir().unwrap();
        let (switch, handler) = handler(true, false, dir.path());
        assert_eq!(handler.acceptance(), CommandAcceptance::Disabled);
        let mut changes = switch.subscribe();
        let output = handler.run(&payload(false)).await;
        assert_eq!(
            output.status(),
            monitorflow_common::proto::v1::CommandStatus::Succeeded
        );
        assert!(!switch.is_on());
        assert!(changes.has_changed().unwrap());
        changes.mark_unchanged();
        let output = handler.run(&payload(true)).await;
        assert!(output.rejected, "{output:?}");
        assert!(output.error.unwrap().contains("MF_AI_COMMANDS_FROM_WEB=1") || cfg!(windows));
        assert!(!switch.is_on());
        assert!(!changes.has_changed().unwrap());
        // Restarted: still off, though agent.toml says on.
        assert!(!AiSwitch::load(true, false, dir.path()).is_on());
    }

    #[tokio::test]
    async fn with_consent_both_are_obeyed_and_kept_across_restarts() {
        let dir = tempfile::tempdir().unwrap();
        let (switch, handler) = handler(false, true, dir.path());
        assert_eq!(handler.acceptance(), CommandAcceptance::Accepted);
        assert!(!switch.is_on());
        let output = handler.run(&payload(true)).await;
        assert_eq!(output.stdout, b"AI commands are on\n");
        assert!(switch.is_on());
        assert!(AiSwitch::load(false, true, dir.path()).is_on());
        // Consent taken back on the host: the stored on is ignored.
        assert!(!AiSwitch::load(false, false, dir.path()).is_on());
        handler.run(&payload(false)).await;
        assert!(!switch.is_on());
        assert!(!AiSwitch::load(true, true, dir.path()).is_on());
        // A change on the host forgets the server's choice.
        assert!(forget(dir.path()).unwrap());
        assert!(!forget(dir.path()).unwrap());
        assert!(AiSwitch::load(true, true, dir.path()).is_on());
    }

    #[tokio::test]
    async fn a_damaged_state_is_off_and_bad_payloads_are_refused() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join(STATE_FILE), "yes").unwrap();
        assert!(!AiSwitch::load(true, true, dir.path()).is_on());
        let (_, handler) = handler(true, true, dir.path());
        assert!(handler.run(b"\xff").await.rejected);
    }

    #[tokio::test]
    async fn a_failed_write_changes_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let missing = dir.path().join("missing");
        let (switch, handler) = handler(true, true, &missing);
        let output = handler.run(&payload(false)).await;
        assert!(!output.rejected && output.exit_code == 1, "{output:?}");
        assert!(output.error.unwrap().contains("cannot store"));
        assert!(switch.is_on());
    }
}
