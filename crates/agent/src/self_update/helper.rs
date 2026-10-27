//! The root side of a self-update (ADR-0053): `monitorflow-agent
//! self-update`, run by the agent through an exact sudo rule, and
//! `self-update watch`, run by systemd from the previous binary.
//!
//! The helper trusts nothing on its stdin: it verifies the release with
//! the keys built into itself and refuses older versions unless the header
//! allows them. The binary is written next to the installed one, synced
//! and renamed over it, so a crash leaves one or the other, never a part.

use std::fs::{self, File, OpenOptions};
use std::io::{self, BufRead, Read, Write};
use std::os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};

use base64::Engine;
use monitorflow_common::release::{
    self, check_executable, check_upgrade, Platform, PublicKey, ReleaseError, Version, HEADER_LEN,
};
use ring::digest::{Context, SHA256};

use super::{unix_ms, Connected, Header, Phase, Status, PREVIOUS_FILE, STATUS_FILE};

/// Longest header line.
pub const MAX_HEADER: usize = 256 * 1024;
/// Largest binary accepted.
pub const MAX_BINARY: u64 = 512 * 1024 * 1024;
/// The transient unit running the watchdog.
pub const WATCH_UNIT: &str = "monitorflow-agent-self-update";
pub const AGENT_SERVICE: &str = "monitorflow-agent";

#[derive(Debug, thiserror::Error)]
pub enum HelperError {
    #[error("invalid input: {0}")]
    Input(String),
    #[error(transparent)]
    Release(#[from] ReleaseError),
    #[error("releases are not built for this platform")]
    Platform,
    #[error("another update is in progress (to {0})")]
    InProgress(String),
    #[error("{0}: {1}")]
    Io(String, io::Error),
    #[error("cannot start the watchdog: {0}; the previous binary is back")]
    Launch(String),
}

fn io_error(what: impl AsRef<Path>) -> impl FnOnce(io::Error) -> HelperError {
    let what = what.as_ref().display().to_string();
    move |error| HelperError::Io(what, error)
}

/// Where the helper works. [`Default`] is the real system.
#[derive(Debug, Clone)]
pub struct Paths {
    /// The installed agent, as sudoers names it.
    pub bin: PathBuf,
    /// Root-owned: the previous binary, the status and the lock.
    pub state_dir: PathBuf,
    /// The agent's data directory, where it writes [`Connected`].
    pub data_dir: PathBuf,
}

impl Paths {
    pub fn system(data_dir: PathBuf) -> Self {
        Self {
            bin: "/usr/bin/monitorflow-agent".into(),
            state_dir: super::STATE_DIR.into(),
            data_dir,
        }
    }

    fn status(&self) -> PathBuf {
        self.state_dir.join(STATUS_FILE)
    }

    fn previous(&self) -> PathBuf {
        self.state_dir.join(PREVIOUS_FILE)
    }

    /// A sibling of `bin`, on its file system, for the atomic rename.
    fn staging(&self) -> PathBuf {
        let name = self.bin.file_name().unwrap_or_default().to_string_lossy();
        self.bin.with_file_name(format!(".{name}.new"))
    }
}

/// systemd, behind a trait for the tests.
pub trait Services {
    /// Starts `program self-update watch` in its own transient unit.
    fn start_watch(&self, program: &Path) -> io::Result<()>;
    fn restart_agent(&self) -> io::Result<()>;
}

/// `systemd-run` and `systemctl`.
pub struct Systemd;

fn status_ok(what: &str, status: io::Result<std::process::ExitStatus>) -> io::Result<()> {
    match status {
        Ok(s) if s.success() => Ok(()),
        Ok(s) => Err(io::Error::other(format!("{what} exited with {s}"))),
        Err(e) => Err(io::Error::other(format!("{what}: {e}"))),
    }
}

impl Services for Systemd {
    fn start_watch(&self, program: &Path) -> io::Result<()> {
        // A failed unit of an earlier watch would keep the name.
        let _ = Command::new("systemctl")
            .args(["reset-failed", WATCH_UNIT])
            .stderr(std::process::Stdio::null())
            .status();
        let status = Command::new("systemd-run")
            .args(["--unit", WATCH_UNIT, "--collect", "--quiet", "--"])
            .arg(program)
            .args(["self-update", "watch"])
            .status();
        status_ok("systemd-run", status)
    }

    fn restart_agent(&self) -> io::Result<()> {
        status_ok(
            "systemctl restart",
            Command::new("systemctl")
                .args(["restart", AGENT_SERVICE])
                .status(),
        )
    }
}

/// What [`install`] did.
#[derive(Debug, PartialEq, Eq)]
pub enum Installed {
    /// The binary is replaced and the watchdog started.
    Restarting { from: Version, to: Version },
    /// The target is the running version: nothing to do.
    Current(Version),
}

/// Reads the header line, verifies the release, installs the binary that
/// follows and starts the watchdog. `current` is this binary's version.
pub fn install(
    input: &mut impl BufRead,
    paths: &Paths,
    keys: &[PublicKey],
    current: &Version,
    platform: Platform,
    services: &dyn Services,
) -> Result<Installed, HelperError> {
    let header = read_header(input)?;
    let signature = base64::engine::general_purpose::STANDARD
        .decode(header.signature.as_bytes())
        .map_err(|_| HelperError::Input("the signature is not base64".into()))?;
    let release = release::verify(
        header.sums.as_bytes(),
        &signature,
        header.version_file.as_bytes(),
        keys,
    )?;
    if !crate::commands::journal::valid_id(&header.command_id) {
        return Err(HelperError::Input("invalid command ID".into()));
    }
    let health = super::health_timeout(0).expect("default");
    let health_timeout_ms = match header.health_timeout_ms {
        0 => health.as_millis() as u64,
        ms => ms.min(u64::from(super::MAX_HEALTH_TIMEOUT_S) * 1000),
    };
    if !check_upgrade(current, &release.version, header.allow_downgrade)? {
        return Ok(Installed::Current(current.clone()));
    }
    let asset = platform.agent_asset();
    let expected = release.digest(asset)?;

    prepare_state_dir(&paths.state_dir)?;
    let _lock = lock(&paths.state_dir)?;
    if let Some(status) = Status::read(&paths.status()).map_err(io_error(paths.status()))? {
        if status.phase == Phase::Watching && unix_ms() <= status.deadline_unix_ms() {
            return Err(HelperError::InProgress(status.to));
        }
    }

    let staging = paths.staging();
    let written = write_binary(input, &staging);
    let checked = written.and_then(|(digest, head)| {
        if digest != expected {
            return Err(ReleaseError::Mismatch(asset.to_owned()).into());
        }
        check_executable(platform, &head)?;
        Ok(())
    });
    if let Err(error) = checked {
        let _ = fs::remove_file(&staging);
        return Err(error);
    }
    copy_atomic(&paths.bin, &paths.previous(), 0o755)?;
    fs::rename(&staging, &paths.bin).map_err(io_error(&paths.bin))?;
    sync_dir(&paths.bin)?;

    let mut status = Status {
        command_id: header.command_id,
        from: current.to_string(),
        to: release.version.to_string(),
        phase: Phase::Watching,
        data_dir: paths.data_dir.clone(),
        health_timeout_ms,
        installed_at_unix_ms: unix_ms(),
        reason: None,
    };
    status
        .write(&paths.status())
        .map_err(io_error(paths.status()))?;
    if let Err(error) = services.start_watch(&paths.previous()) {
        copy_atomic(&paths.previous(), &paths.bin, 0o755)?;
        status.phase = Phase::Failed;
        status.reason = Some(format!("cannot start the watchdog: {error}"));
        status
            .write(&paths.status())
            .map_err(io_error(paths.status()))?;
        return Err(HelperError::Launch(error.to_string()));
    }
    Ok(Installed::Restarting {
        from: current.clone(),
        to: release.version,
    })
}

fn read_header(input: &mut impl BufRead) -> Result<Header, HelperError> {
    let mut line = Vec::new();
    input
        .take(MAX_HEADER as u64 + 1)
        .read_until(b'\n', &mut line)
        .map_err(io_error("stdin"))?;
    if line.pop() != Some(b'\n') {
        return Err(HelperError::Input(format!(
            "no header line of at most {MAX_HEADER} bytes"
        )));
    }
    serde_json::from_slice(&line).map_err(|e| HelperError::Input(e.to_string()))
}

/// The state directory exists, belongs to us (root) and nobody else can
/// write it: a binary we run as root is kept there.
fn prepare_state_dir(dir: &Path) -> Result<(), HelperError> {
    match fs::DirBuilder::new().mode(0o755).create(dir) {
        Ok(()) => {}
        Err(e) if e.kind() == io::ErrorKind::AlreadyExists => {}
        Err(e) => return Err(io_error(dir)(e)),
    }
    let meta = fs::symlink_metadata(dir).map_err(io_error(dir))?;
    let ours = meta.uid() == rustix::process::geteuid().as_raw();
    if !meta.is_dir() || !ours || meta.mode() & 0o022 != 0 {
        return Err(HelperError::Input(format!(
            "{} must be a directory of the helper's user, writable by it only",
            dir.display()
        )));
    }
    Ok(())
}

/// Held while installing; a second helper waits.
fn lock(dir: &Path) -> Result<File, HelperError> {
    let path = dir.join("lock");
    let file = OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .mode(0o600)
        .open(&path)
        .map_err(io_error(&path))?;
    rustix::fs::flock(&file, rustix::fs::FlockOperation::LockExclusive)
        .map_err(|e| io_error(&path)(e.into()))?;
    Ok(file)
}

/// Streams the rest of `input` to a new file at `path`, mode 0755, synced;
/// returns its SHA-256 and first bytes.
fn write_binary(input: &mut impl Read, path: &Path) -> Result<([u8; 32], Vec<u8>), HelperError> {
    match fs::remove_file(path) {
        Err(e) if e.kind() != io::ErrorKind::NotFound => return Err(io_error(path)(e)),
        _ => {}
    }
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o755)
        .open(path)
        .map_err(io_error(path))?;
    let mut digest = Context::new(&SHA256);
    let (mut head, mut total) = (Vec::with_capacity(HEADER_LEN), 0u64);
    let mut buffer = vec![0u8; 64 * 1024];
    loop {
        let n = input.read(&mut buffer).map_err(io_error("stdin"))?;
        if n == 0 {
            break;
        }
        total += n as u64;
        if total > MAX_BINARY {
            return Err(HelperError::Input(format!(
                "the binary is larger than {MAX_BINARY} bytes"
            )));
        }
        let chunk = &buffer[..n];
        if head.len() < HEADER_LEN {
            let take = (HEADER_LEN - head.len()).min(n);
            head.extend_from_slice(&chunk[..take]);
        }
        digest.update(chunk);
        file.write_all(chunk).map_err(io_error(path))?;
    }
    // The mode is set again: the umask may have cleared bits.
    file.set_permissions(fs::Permissions::from_mode(0o755))
        .map_err(io_error(path))?;
    file.sync_all().map_err(io_error(path))?;
    let digest = digest.finish().as_ref().try_into().expect("32 bytes");
    Ok((digest, head))
}

/// Copies `from` over `to` with `mode`, through a synced file renamed
/// into place.
fn copy_atomic(from: &Path, to: &Path, mode: u32) -> Result<(), HelperError> {
    let tmp = to.with_file_name(format!(
        ".{}.tmp",
        to.file_name().unwrap_or_default().to_string_lossy()
    ));
    let mut source = File::open(from).map_err(io_error(from))?;
    write_binary(&mut source, &tmp)?;
    fs::set_permissions(&tmp, fs::Permissions::from_mode(mode)).map_err(io_error(&tmp))?;
    fs::rename(&tmp, to).map_err(io_error(to))?;
    sync_dir(to)
}

fn sync_dir(path: &Path) -> Result<(), HelperError> {
    let dir = path.parent().unwrap_or(Path::new("/"));
    File::open(dir)
        .and_then(|d| d.sync_all())
        .map_err(io_error(dir))
}

/// What [`watch`] decided.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Watched {
    Succeeded,
    RolledBack,
    /// No update was being watched.
    Nothing,
}

/// The watchdog: restarts the agent, waits for the new version's
/// [`Connected`] file, and puts the previous binary back when it does not
/// come within the status's health timeout.
pub fn watch(
    paths: &Paths,
    services: &dyn Services,
    poll: Duration,
) -> Result<Watched, HelperError> {
    let Some(mut status) = Status::read(&paths.status()).map_err(io_error(paths.status()))? else {
        return Ok(Watched::Nothing);
    };
    if status.phase != Phase::Watching {
        return Ok(Watched::Nothing);
    }
    let restarted_at = unix_ms();
    let started = Instant::now();
    let restart = services.restart_agent();
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
    copy_atomic(&paths.previous(), &paths.bin, 0o755)?;
    let restored = services.restart_agent();
    status.phase = Phase::RolledBack;
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
    status.reason = Some(reason);
    status
        .write(&paths.status())
        .map_err(io_error(paths.status()))?;
    Ok(Watched::RolledBack)
}

#[cfg(test)]
mod tests;
