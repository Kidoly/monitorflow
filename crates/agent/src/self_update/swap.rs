//! The Windows side of a self-update (ADR-0053), in the update service
//! (`MonitorFlowUpdate`, LocalSystem). The agent stages a checked release
//! in `<data_dir>\self-update\staged\` and asks the service on its pipe;
//! the service reads the files into memory, checks them again with the
//! keys built into itself, and swaps the executable by renames: Windows
//! allows renaming a running image, so the service, which runs the same
//! file, keeps running the old one. It then restarts the agent's service
//! and waits for the new agent's [`Connected`] file; on a timeout it
//! renames the files back.
//!
//! Only `std::fs` and [`AgentService`], so the logic builds and is tested
//! everywhere; the service control manager is behind the trait.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use monitorflow_common::release::{
    self, check_executable, check_upgrade, Platform, PublicKey, ReleaseError, Version, SIGNATURE,
    SUMS, VERSION_FILE,
};
use serde::{Deserialize, Serialize};

use super::{unix_ms, Connected, Phase, Status, AGENT_DIR, STATUS_FILE};

/// Where the agent stages a release, under its data directory.
pub const STAGED_DIR: &str = "staged";
/// Largest executable read.
pub const MAX_BINARY: u64 = 512 * 1024 * 1024;
const MAX_SMALL: u64 = 64 * 1024;

/// What a self-update installs, as the update service read it from the
/// signed `self_update` command (ADR-0108), and `data_dir` from its own
/// copy of `agent.toml`, which the agent's account cannot write.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Request {
    pub command_id: String,
    /// The version the command asks for: the staged release must be it.
    pub version: String,
    /// The agent's data directory: the release is staged under it, and the
    /// new agent writes its [`Connected`] file there.
    pub data_dir: PathBuf,
    pub allow_downgrade: bool,
    pub health_timeout_ms: u64,
}

impl Request {
    pub fn validate(&self) -> Result<(), String> {
        if !crate::commands::journal::valid_id(&self.command_id) {
            return Err("invalid command ID".into());
        }
        local_absolute(&self.data_dir).map_err(|e| format!("the data directory {e}"))?;
        let max = u64::from(super::MAX_HEALTH_TIMEOUT_S) * 1000;
        if self.health_timeout_ms > max {
            return Err(format!("the health timeout is at most {max} ms"));
        }
        Ok(())
    }

    /// `<data_dir>\self-update\staged`.
    pub fn staged(&self) -> PathBuf {
        staged_dir(&self.data_dir)
    }
}

pub fn staged_dir(data_dir: &Path) -> PathBuf {
    data_dir.join(AGENT_DIR).join(STAGED_DIR)
}

/// Refuses a path LocalSystem should not open: relative, with `..`, or a
/// network or device path (`\\host\share`, `\\?\`, `\\.\`), which would
/// make it authenticate to another host or reach a raw device. On Windows
/// only a drive path, `C:\…`, passes.
pub fn local_absolute(path: &Path) -> Result<(), String> {
    let text = path.to_string_lossy();
    let sep = |c: char| c == '\\' || c == '/';
    let mut chars = text.chars();
    if chars.next().is_some_and(sep) && chars.next().is_some_and(sep) {
        return Err("must not be a network or device path".into());
    }
    if text.split(sep).any(|part| part == "..") {
        return Err("must not contain `..`".into());
    }
    let absolute = if cfg!(windows) {
        windows_drive_absolute(&text)
    } else {
        path.is_absolute()
    };
    if !absolute {
        return Err("must be an absolute local path".into());
    }
    Ok(())
}

/// `X:\` or `X:/` and more, nothing else.
fn windows_drive_absolute(text: &str) -> bool {
    let b = text.as_bytes();
    b.len() >= 3 && b[0].is_ascii_alphabetic() && b[1] == b':' && matches!(b[2], b'\\' | b'/')
}

#[derive(Debug, thiserror::Error)]
pub enum SwapError {
    #[error("invalid request: {0}")]
    Input(String),
    #[error(transparent)]
    Release(#[from] ReleaseError),
    #[error("another update is in progress (to {0})")]
    InProgress(String),
    #[error("{0}: {1}")]
    Io(String, io::Error),
}

fn io_error(what: impl AsRef<Path>) -> impl FnOnce(io::Error) -> SwapError {
    let what = what.as_ref().display().to_string();
    move |error| SwapError::Io(what, error)
}

/// The agent's executable, where the old one goes, and the service's own
/// directory for the status.
#[derive(Debug, Clone)]
pub struct Paths {
    pub exe: PathBuf,
    pub state_dir: PathBuf,
}

impl Paths {
    /// `monitorflow-agent.previous.exe` next to the executable: a rename
    /// stays on its volume.
    pub fn previous(&self) -> PathBuf {
        self.exe.with_file_name("monitorflow-agent.previous.exe")
    }

    fn failed(&self) -> PathBuf {
        self.exe.with_file_name("monitorflow-agent.failed.exe")
    }

    fn incoming(&self) -> PathBuf {
        self.exe.with_file_name(".monitorflow-agent.new.exe")
    }

    pub fn status(&self) -> PathBuf {
        self.state_dir.join(STATUS_FILE)
    }

    fn validate(&self) -> Result<(), String> {
        local_absolute(&self.exe).map_err(|e| format!("the executable {e}"))?;
        local_absolute(&self.state_dir).map_err(|e| format!("the state directory {e}"))
    }
}

/// The agent's Windows service, behind a trait for the tests.
pub trait AgentService {
    /// Stops the agent if it runs, then starts it.
    fn restart(&self) -> io::Result<()>;
}

/// The service control manager.
#[cfg(windows)]
pub struct Scm;

#[cfg(windows)]
impl AgentService for Scm {
    fn restart(&self) -> io::Result<()> {
        use windows_service::service::{ServiceAccess, ServiceState};
        use windows_service::service_manager::{ServiceManager, ServiceManagerAccess};

        let fail = |what: &str| {
            let what = what.to_owned();
            move |e: windows_service::Error| io::Error::other(format!("{what}: {e}"))
        };
        let manager = ServiceManager::local_computer(None::<&str>, ServiceManagerAccess::CONNECT)
            .map_err(fail("opening the service control manager"))?;
        let service = manager
            .open_service(
                crate::packages::windows_update::AGENT_SERVICE,
                ServiceAccess::QUERY_STATUS | ServiceAccess::STOP | ServiceAccess::START,
            )
            .map_err(fail("opening the agent's service"))?;
        let status = service
            .query_status()
            .map_err(fail("querying the agent's service"))?;
        if status.current_state != ServiceState::Stopped {
            // Already stopping is fine.
            let _ = service.stop();
            let deadline = Instant::now() + Duration::from_secs(60);
            loop {
                let state = service
                    .query_status()
                    .map_err(fail("querying the agent's service"))?
                    .current_state;
                if state == ServiceState::Stopped {
                    break;
                }
                if Instant::now() >= deadline {
                    return Err(io::Error::other(
                        "the agent's service did not stop within 60 s",
                    ));
                }
                std::thread::sleep(Duration::from_millis(250));
            }
        }
        service
            .start::<&str>(&[])
            .map_err(fail("starting the agent's service"))
    }
}

/// What [`install`] did.
#[derive(Debug, PartialEq, Eq)]
pub enum Swapped {
    /// The new executable is in place; [`watch`] restarts the agent.
    Restarting {
        from: Version,
        to: Version,
    },
    Current(Version),
}

/// Opens a staged file, in a directory the agent's account writes, for
/// reading: a final symlink or junction is opened as itself and refused,
/// not followed. On Windows the open also asks for identification-level
/// impersonation, so a named pipe the agent's account put in its place
/// cannot act as LocalSystem; what is read is checked against the
/// release's signature anyway.
fn open_staged(path: &Path) -> io::Result<fs::File> {
    let mut options = fs::OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(rustix::fs::OFlags::NOFOLLOW.bits() as i32);
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        // FILE_FLAG_OPEN_REPARSE_POINT; SECURITY_IDENTIFICATION, to which
        // std adds SECURITY_SQOS_PRESENT.
        options.custom_flags(0x0020_0000);
        options.security_qos_flags(0x0001_0000);
    }
    let file = options.open(path)?;
    if !file.metadata()?.is_file() {
        return Err(io::Error::other("not a regular file"));
    }
    Ok(file)
}

fn read_limited(path: &Path, limit: u64) -> Result<Vec<u8>, SwapError> {
    use std::io::Read;
    let file = open_staged(path).map_err(io_error(path))?;
    let mut bytes = Vec::new();
    file.take(limit + 1)
        .read_to_end(&mut bytes)
        .map_err(io_error(path))?;
    if bytes.len() as u64 > limit {
        return Err(SwapError::Input(format!("{} is too large", path.display())));
    }
    Ok(bytes)
}

/// Checks the staged release and swaps the executable; [`watch`] does the
/// rest. `current` is the running agent's version, this binary's.
pub fn install(
    request: &Request,
    paths: &Paths,
    keys: &[PublicKey],
    current: &Version,
    platform: Platform,
) -> Result<Swapped, SwapError> {
    request.validate().map_err(SwapError::Input)?;
    paths.validate().map_err(SwapError::Input)?;
    let staged = request.staged();
    // Read once into the service's memory: what is checked is what is
    // installed, whatever the agent's user does to the files meanwhile.
    // Nothing is written or removed there as LocalSystem: the agent
    // removes its staged files itself.
    let sums = read_limited(&staged.join(SUMS), MAX_SMALL)?;
    let signature = read_limited(&staged.join(SIGNATURE), MAX_SMALL)?;
    let version_file = read_limited(&staged.join(VERSION_FILE), MAX_SMALL)?;
    let verified = release::verify(&sums, &signature, &version_file, keys)?;
    if request.version.parse::<Version>().ok().as_ref() != Some(&verified.version) {
        return Err(SwapError::Input(format!(
            "the release is {}, and the command asks for {}",
            verified.version, request.version
        )));
    }
    if !check_upgrade(current, &verified.version, request.allow_downgrade)? {
        return Ok(Swapped::Current(current.clone()));
    }
    let asset = platform.agent_asset();
    let binary = read_limited(&staged.join(asset), MAX_BINARY)?;
    verified.check(asset, &binary)?;
    check_executable(platform, &binary)?;

    fs::create_dir_all(&paths.state_dir).map_err(io_error(&paths.state_dir))?;
    if let Some(status) = Status::read(&paths.status()).map_err(io_error(paths.status()))? {
        if status.phase == Phase::Watching && unix_ms() <= status.deadline_unix_ms() {
            return Err(SwapError::InProgress(status.to));
        }
    }
    let incoming = paths.incoming();
    write_synced(&incoming, &binary)?;
    // A previous executable from an earlier update is not running any
    // more: the service restarted on the new one.
    remove_if_present(&paths.previous())?;
    fs::rename(&paths.exe, paths.previous()).map_err(io_error(&paths.exe))?;
    if let Err(error) = fs::rename(&incoming, &paths.exe) {
        let _ = fs::rename(paths.previous(), &paths.exe);
        return Err(io_error(&paths.exe)(error));
    }
    let health_timeout_ms = match request.health_timeout_ms {
        0 => u64::from(super::DEFAULT_HEALTH_TIMEOUT_S) * 1000,
        ms => ms,
    };
    Status {
        command_id: request.command_id.clone(),
        from: current.to_string(),
        to: verified.version.to_string(),
        phase: Phase::Watching,
        data_dir: request.data_dir.clone(),
        health_timeout_ms,
        installed_at_unix_ms: unix_ms(),
        reason: None,
    }
    .write(&paths.status())
    .map_err(io_error(paths.status()))?;
    Ok(Swapped::Restarting {
        from: current.clone(),
        to: verified.version,
    })
}

fn write_synced(path: &Path, bytes: &[u8]) -> Result<(), SwapError> {
    use std::io::Write;
    remove_if_present(path)?;
    let mut file = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
        .map_err(io_error(path))?;
    file.write_all(bytes).map_err(io_error(path))?;
    file.sync_all().map_err(io_error(path))
}

fn remove_if_present(path: &Path) -> Result<(), SwapError> {
    match fs::remove_file(path) {
        Err(e) if e.kind() != io::ErrorKind::NotFound => Err(io_error(path)(e)),
        _ => Ok(()),
    }
}

/// What [`watch`] decided.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Watched {
    Succeeded,
    RolledBack,
    Nothing,
}

/// Restarts the agent and waits for the new version to connect; puts the
/// previous executable back when it does not within the health timeout.
pub fn watch(
    paths: &Paths,
    agent: &dyn AgentService,
    poll: Duration,
) -> Result<Watched, SwapError> {
    let Some(mut status) = Status::read(&paths.status()).map_err(io_error(paths.status()))? else {
        return Ok(Watched::Nothing);
    };
    if status.phase != Phase::Watching {
        return Ok(Watched::Nothing);
    }
    let restarted_at = unix_ms();
    let started = Instant::now();
    let restart = agent.restart();
    let timeout = Duration::from_millis(status.health_timeout_ms);
    let healthy = restart.is_ok()
        && loop {
            let connected = Connected::read(&status.data_dir).ok().flatten();
            if connected.is_some_and(|c| c.version == status.to && c.at_unix_ms >= restarted_at) {
                break true;
            }
            if started.elapsed() >= timeout {
                break false;
            }
            std::thread::sleep(poll);
        };
    if healthy {
        status.phase = Phase::Succeeded;
        status
            .write(&paths.status())
            .map_err(io_error(paths.status()))?;
        return Ok(Watched::Succeeded);
    }
    // The failed executable may still be running; renaming it works.
    remove_if_present(&paths.failed())?;
    fs::rename(&paths.exe, paths.failed()).map_err(io_error(&paths.exe))?;
    fs::rename(paths.previous(), &paths.exe).map_err(io_error(paths.previous()))?;
    let restored = agent.restart();
    let mut reason = match restart {
        Err(error) => format!("cannot restart the agent: {error}"),
        Ok(()) => format!(
            "{} did not connect within {} s",
            status.to,
            timeout.as_secs()
        ),
    };
    if let Err(error) = restored {
        reason.push_str(&format!(
            "; restarting the previous agent failed too: {error}"
        ));
    }
    status.phase = Phase::RolledBack;
    status.reason = Some(reason);
    status
        .write(&paths.status())
        .map_err(io_error(paths.status()))?;
    Ok(Watched::RolledBack)
}

#[cfg(test)]
mod tests;
