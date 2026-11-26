//! Software from the central server's catalogue (ADR-0188), the parts
//! every platform shares: whether the host allows it now, and the
//! `software_off` command that takes the power away from the web
//! interface.
//!
//! The host's administrator allows software installs with
//! `commands.software` in `agent.toml`, which the agent's user cannot
//! write. The central server can only take that away: `software_off`
//! writes `<data_dir>/software-off`, and from then on the agent, the Linux
//! root helper and the Windows update service all refuse installs and
//! removals. Nothing the server sends removes the file; only
//! `monitorflow-agent software on|off` on the host does, so turning
//! software back on always needs the host's administrator.
//!
//! The file is in the agent's own data directory, which its user can
//! write: removing it by hand gives back exactly what `agent.toml` already
//! allows, never more.

#[cfg(unix)]
pub mod helper;
#[cfg(unix)]
pub mod linux;
#[cfg(test)]
mod tests;

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use monitorflow_common::proto::v1::{CommandAcceptance, SoftwareOff};
use prost::Message;
use tokio::sync::{watch, Mutex};

use crate::commands::{Handler, Output};

/// The file in the data directory that says the central server turned
/// software installs off.
pub const OFF_FILE: &str = "software-off";

/// Why a software change is refused once the web interface turned it off,
/// with the way to turn it on again.
#[cfg(unix)]
pub const TURNED_OFF: &str = "software installs were turned off from the web interface; an \
     administrator of this server turns them on again with `sudo monitorflow-agent software on`";
#[cfg(windows)]
pub const TURNED_OFF: &str = "software installs were turned off from the web interface; an \
     administrator of this server turns them on again with `monitorflow-agent.exe software on`";

/// Why a software change is refused on a host that never allowed it.
#[cfg(unix)]
pub const NOT_ALLOWED: &str = "software installs are not allowed on this server; an \
     administrator allows them with `sudo monitorflow-agent software on`, or the installer \
     with MF_SOFTWARE_FROM_WEB=1";
#[cfg(windows)]
pub use crate::packages::windows_update::SOFTWARE_NOT_ALLOWED as NOT_ALLOWED;

/// Whether the web interface turned software installs off on the host
/// whose data directory is `data_dir`. A file that cannot be read counts
/// as off: a damaged state must not give the power back.
pub fn turned_off(data_dir: &Path) -> bool {
    match std::fs::symlink_metadata(data_dir.join(OFF_FILE)) {
        Ok(_) => true,
        Err(error) => error.kind() != std::io::ErrorKind::NotFound,
    }
}

/// Removes the web interface's "off", which `software on|off` on the host
/// does. True when there was one.
pub fn forget(data_dir: &Path) -> std::io::Result<bool> {
    match std::fs::remove_file(data_dir.join(OFF_FILE)) {
        Ok(()) => Ok(true),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(error),
    }
}

/// Whether software installs are allowed now, and what turns them off.
pub struct SoftwareSwitch {
    /// `commands.software`.
    consent: bool,
    /// `None` keeps the state in memory only, for tests.
    data_dir: Option<PathBuf>,
    off: AtomicBool,
    writing: Mutex<()>,
    changed: watch::Sender<()>,
}

impl SoftwareSwitch {
    /// The state from the consent and the file in `data_dir`.
    pub fn load(consent: bool, data_dir: &Path) -> Self {
        let off = turned_off(data_dir);
        if off {
            tracing::info!(
                consent,
                "software installs were turned off from the web interface"
            );
        }
        Self::new(consent, Some(data_dir.to_owned()), off)
    }

    /// A state never stored, for tests.
    pub fn fixed(consent: bool) -> Self {
        Self::new(consent, None, false)
    }

    fn new(consent: bool, data_dir: Option<PathBuf>, off: bool) -> Self {
        Self {
            consent,
            data_dir,
            off: AtomicBool::new(off),
            writing: Mutex::new(()),
            changed: watch::channel(()).0,
        }
    }

    /// Whether the host allowed it at all.
    pub fn consent(&self) -> bool {
        self.consent
    }

    /// Whether the web interface turned it off.
    pub fn is_turned_off(&self) -> bool {
        self.off.load(Ordering::SeqCst)
    }

    /// Whether a software change may run now.
    pub fn allowed(&self) -> bool {
        self.consent && !self.is_turned_off()
    }

    /// Why a software change is refused now, if it is.
    pub fn refusal(&self) -> Option<&'static str> {
        if !self.consent {
            Some(NOT_ALLOWED)
        } else if self.is_turned_off() {
            Some(TURNED_OFF)
        } else {
            None
        }
    }

    /// `acceptance` of the handlers that install software.
    pub fn acceptance(&self) -> CommandAcceptance {
        if self.allowed() {
            CommandAcceptance::Accepted
        } else {
            CommandAcceptance::Disabled
        }
    }

    /// Fires each time [`Self::allowed`] changes.
    pub fn subscribe(&self) -> watch::Receiver<()> {
        self.changed.subscribe()
    }

    /// Turns software installs off: stored first, so it survives restarts
    /// and the privileged side sees it, then in effect. True when it
    /// changed what the agent does.
    pub async fn turn_off(&self) -> Result<bool, String> {
        let _writing = self.writing.lock().await;
        if let Some(dir) = self.data_dir.clone() {
            tokio::task::spawn_blocking(move || write_off(&dir))
                .await
                .map_err(|e| e.to_string())?
                .map_err(|e| format!("cannot store the choice: {e}"))?;
        }
        let before = self.allowed();
        self.off.store(true, Ordering::SeqCst);
        if before {
            tracing::info!("software installs turned off from the web interface");
            self.changed.send_replace(());
        }
        Ok(before)
    }
}

/// Writes the file through a temporary one and a rename.
fn write_off(data_dir: &Path) -> std::io::Result<()> {
    let path = data_dir.join(OFF_FILE);
    let temporary = path.with_extension("new");
    std::fs::write(&temporary, "off\n")?;
    std::fs::rename(&temporary, path)
}

/// Runs `software_off` commands. Always accepted, so the server knows the
/// agent obeys them, whatever the consent.
pub struct SoftwareOffHandler {
    switch: Arc<SoftwareSwitch>,
}

impl SoftwareOffHandler {
    pub fn new(switch: Arc<SoftwareSwitch>) -> Self {
        Self { switch }
    }
}

#[async_trait::async_trait]
impl Handler for SoftwareOffHandler {
    async fn run(&self, payload: &[u8]) -> Output {
        if SoftwareOff::decode(payload).is_err() {
            return Output::rejected("invalid SoftwareOff payload");
        }
        match self.switch.turn_off().await {
            Ok(changed) => {
                let already = if changed { "" } else { " already" };
                Output {
                    stdout: format!(
                        "software installs are{already} off on this server; only its \
                         administrator can turn them on again\n"
                    )
                    .into_bytes(),
                    ..Output::default()
                }
            }
            Err(error) => Output {
                exit_code: 1,
                error: Some(error),
                ..Output::default()
            },
        }
    }
}
