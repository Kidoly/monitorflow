//! The root side of a major upgrade (ADR-0182).
//!
//! [`start`] runs as `sudo -n monitorflow-agent major-upgrade` once the
//! signed command checked: the host's consent, the releases, no upgrade
//! running, the checks, then the state file and the transient unit that
//! runs [`run`]. [`run`] does the steps, records each phase in the state
//! file and writes everything the tools print to the log.

use std::fs::{self, File, OpenOptions};
use std::io::{self, Write};
use std::os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use monitorflow_common::config::{self, AgentConfig, Env};
use monitorflow_common::major_upgrade::{self, Phase, Upgrade};
use monitorflow_common::proto::v1::MajorUpgrade;
use prost::Message;

use super::checks::{self, Probe};
use super::sources::{self, FilePlan};
use super::state::{self, State, BACKUP, LOG};

/// The transient unit's name.
pub const UNIT: &str = "monitorflow-major-upgrade";
const LOCK: &str = "lock";

/// apt without questions, keeping every changed configuration file.
const APT_ENV: &[(&str, &str)] = &[
    ("DEBIAN_FRONTEND", "noninteractive"),
    ("APT_LISTCHANGES_FRONTEND", "none"),
    ("NEEDRESTART_MODE", "a"),
    ("LC_ALL", "C"),
];
const APT_OPTIONS: &[&str] = &[
    "-q",
    "-y",
    "-o",
    "Dpkg::Options::=--force-confdef",
    "-o",
    "Dpkg::Options::=--force-confold",
];

/// Whether the host agreed: `packages.major_upgrades` in the file itself,
/// never from the environment.
pub fn consent(config: &Path) -> Result<bool, String> {
    let text =
        fs::read_to_string(config).map_err(|e| format!("cannot read {}: {e}", config.display()))?;
    let config: AgentConfig =
        config::parse_with_env(&text, config, &Env::new()).map_err(|e| e.to_string())?;
    Ok(config.packages.major_upgrades)
}

/// The line that allows major upgrades on this host.
pub const ALLOW_LINE: &str = "run `monitorflow-agent major-upgrades on` as root on the server, or install it again with MF_MAJOR_UPGRADES_FROM_WEB=1";

/// The upgrade a signed payload asks for, if this host runs its `from`.
pub fn requested(payload: &[u8], running: &str) -> Result<&'static Upgrade, String> {
    let request = MajorUpgrade::decode(payload).map_err(|e| format!("invalid payload: {e}"))?;
    let Some(upgrade) = major_upgrade::step(&request.from, &request.to) else {
        return Err(format!(
            "this agent does not know an upgrade from {} to {}",
            request.from, request.to
        ));
    };
    if major_upgrade::next(running) != Some(upgrade) {
        return Err(format!(
            "this server runs {running}, not {}",
            upgrade.from_release()
        ));
    }
    Ok(upgrade)
}

/// Holds the state directory's lock until dropped.
pub struct Lock(#[allow(dead_code)] File);

pub fn lock(dir: &Path) -> io::Result<Lock> {
    let file = OpenOptions::new()
        .create(true)
        .append(true)
        .mode(0o600)
        .open(dir.join(LOCK))?;
    file.lock()?;
    Ok(Lock(file))
}

/// What [`start`] did, printed by the helper.
#[derive(Debug, Default)]
pub struct Started {
    pub lines: Vec<String>,
}

/// Checks everything again as root, writes the state and starts the unit.
/// `upgradable` counts the updates of the current release, after a refresh.
pub fn start(
    command_id: &str,
    upgrade: &Upgrade,
    probe: &dyn Probe,
    upgradable: usize,
    dir: &Path,
    now: i64,
) -> Result<Started, String> {
    state::prepare(dir).map_err(|e| e.to_string())?;
    let _lock = lock(dir).map_err(|e| format!("cannot lock {}: {e}", dir.display()))?;
    let boot = state::boot_id();
    let running = crate::os::detect_from(None).os_version;
    if let Some(previous) = state::read(dir).map_err(|e| e.to_string())? {
        let previous = previous.effective(&boot, &running);
        if !previous.phase.is_final() {
            return Err(format!(
                "an upgrade to {} is already running ({})",
                previous.to,
                previous.phase.as_str()
            ));
        }
    }
    let plan = sources::plan(Path::new("/"), upgrade);
    let checks = checks::evaluate(probe, upgradable, &plan);
    let mut started = Started::default();
    for check in &checks {
        started.lines.push(format!(
            "check {}: {} ({})",
            check.name, check.state, check.detail
        ));
    }
    if let Some(failure) = checks::first_failure(&checks) {
        return Err(failure);
    }
    let state = State::new(
        command_id,
        &upgrade.from_release(),
        &upgrade.to_release(),
        &boot,
        now,
    );
    state::write(dir, &state).map_err(|e| e.to_string())?;
    let output = Command::new("/usr/bin/systemd-run")
        .args([
            "--unit",
            UNIT,
            "--collect",
            "--description",
            "MonitorFlow major upgrade (ADR-0182)",
            "/usr/bin/monitorflow-agent",
            "major-upgrade",
            "run",
        ])
        .stdin(Stdio::null())
        .output()
        .map_err(|e| format!("cannot run systemd-run: {e}"))?;
    if !output.status.success() {
        let error = format!(
            "systemd-run failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        );
        let mut failed = state;
        failed.phase = Phase::Failed;
        failed.error = error.clone();
        let _ = state::write(dir, &failed);
        return Err(error);
    }
    started.lines.push(format!(
        "started the upgrade from {} to {} in {UNIT}.service; its log is {}",
        upgrade.from_label(),
        upgrade.to_label(),
        dir.join(LOG).display()
    ));
    Ok(started)
}

/// Runs the programs of the steps. [`System`] is the real one.
pub trait Exec {
    /// Runs `program args` with `env` added, appending its output to the
    /// log; its exit code and output.
    fn run(
        &mut self,
        program: &str,
        args: &[&str],
        env: &[(&str, &str)],
    ) -> io::Result<(i32, String)>;
    fn note(&mut self, line: &str);
}

/// The real system, logging to the upgrade's log.
pub struct System {
    log: File,
}

impl System {
    pub fn new(dir: &Path) -> io::Result<Self> {
        let log = OpenOptions::new()
            .create(true)
            .append(true)
            .mode(0o600)
            .open(dir.join(LOG))?;
        Ok(Self { log })
    }
}

impl Exec for System {
    fn run(
        &mut self,
        program: &str,
        args: &[&str],
        env: &[(&str, &str)],
    ) -> io::Result<(i32, String)> {
        let _ = writeln!(self.log, "$ {program} {}", args.join(" "));
        let output = Command::new(program)
            .args(args)
            .envs(env.iter().copied())
            .stdin(Stdio::null())
            .output()?;
        let _ = self.log.write_all(&output.stdout);
        let _ = self.log.write_all(&output.stderr);
        let mut text = String::from_utf8_lossy(&output.stdout).into_owned();
        text.push_str(&String::from_utf8_lossy(&output.stderr));
        Ok((output.status.code().unwrap_or(-1), text))
    }

    fn note(&mut self, line: &str) {
        let _ = writeln!(self.log, "{line}");
    }
}

/// Where [`run`] works: `/` on a host, a temporary directory in tests.
pub struct Paths {
    pub root: PathBuf,
    pub state_dir: PathBuf,
}

/// The steps, from the state [`start`] wrote. Returns the final state,
/// already written: `rebooting` once the reboot is asked.
pub fn run(paths: &Paths, exec: &mut dyn Exec, now: impl Fn() -> i64) -> Result<State, String> {
    let Some(mut state) = state::read(&paths.state_dir).map_err(|e| e.to_string())? else {
        return Err("no upgrade to run".into());
    };
    if state.phase != Phase::Starting {
        return Err(format!(
            "the upgrade is {}, not starting",
            state.phase.as_str()
        ));
    }
    let Some(upgrade) = major_upgrade::step(&state.from, &state.to) else {
        return Err(format!(
            "unknown upgrade from {} to {}",
            state.from, state.to
        ));
    };
    let set = |state: &mut State, phase: Phase, error: Option<String>| {
        state.phase = phase;
        if let Some(error) = error {
            state.error = error;
        }
        state.updated_at_unix_ms = now();
        tracing::info!(phase = state.phase.as_str(), "major upgrade");
        state::write(&paths.state_dir, state).map_err(|e| e.to_string())
    };

    set(&mut state, Phase::Sources, None)?;
    let plan = sources::plan(&paths.root, upgrade);
    if let Some(refusal) = &plan.refusal {
        set(&mut state, Phase::Failed, Some(refusal.clone()))?;
        return Ok(state);
    }
    let changed: Vec<&FilePlan> = plan.files.iter().filter(|f| f.changed()).collect();
    if let Err(error) = save_and_write(paths, &changed) {
        let restored = restore(paths, &changed).is_ok();
        state.sources_restored = restored;
        set(
            &mut state,
            Phase::Failed,
            Some(format!("cannot rewrite the sources: {error}")),
        )?;
        return Ok(state);
    }
    state.disabled_sources = plan.third_party.clone();
    exec.note(&format!(
        "pointed {} entries at {}; switched off {} third-party entries",
        plan.release_entries,
        upgrade.to_codename,
        plan.third_party.len()
    ));

    set(&mut state, Phase::Refreshing, None)?;
    if let Err(error) = refresh(exec) {
        let restored = restore(paths, &changed).is_ok();
        if restored {
            let _ = refresh(exec);
        }
        state.sources_restored = restored;
        let error = if restored {
            format!("{error}; the sources were put back as they were")
        } else {
            format!("{error}; putting the sources back failed too")
        };
        set(&mut state, Phase::Failed, Some(error))?;
        return Ok(state);
    }

    for (phase, verb) in [
        (
            Phase::Upgrading,
            ["upgrade", "--without-new-pkgs"].as_slice(),
        ),
        (Phase::FullUpgrade, ["full-upgrade"].as_slice()),
    ] {
        set(&mut state, phase, None)?;
        let mut args: Vec<&str> = APT_OPTIONS.to_vec();
        args.extend_from_slice(verb);
        match exec.run("/usr/bin/apt-get", &args, APT_ENV) {
            Ok((0, _)) => {}
            Ok((code, output)) => {
                let error = format!(
                    "`apt-get {}` exited with {code}: {}; only the snapshot goes back now",
                    verb.join(" "),
                    last_error(&output)
                );
                set(&mut state, Phase::Failed, Some(error))?;
                return Ok(state);
            }
            Err(e) => {
                set(
                    &mut state,
                    Phase::Failed,
                    Some(format!("cannot run apt-get: {e}")),
                )?;
                return Ok(state);
            }
        }
    }

    // A server pointed at the mirrors keeps a backup of its own sources
    // (ADR-0032): they move to the new release too, so going back to them
    // does not bring the old release back.
    let rewritten = crate::packages::sources::rewrite_originals(&paths.root, |path, text| {
        let name = path.file_name()?.to_str()?;
        let new = if name.ends_with(".sources") {
            sources::rewrite_deb822(text, upgrade, false).text
        } else {
            sources::rewrite_classic(text, upgrade).text
        };
        (new != text).then_some(new)
    });
    match rewritten {
        Ok(paths) if !paths.is_empty() => exec.note(&format!(
            "moved the saved sources of the server to {}: {}",
            upgrade.to_codename,
            paths
                .iter()
                .map(|p| p.display().to_string())
                .collect::<Vec<_>>()
                .join(", ")
        )),
        Ok(_) => {}
        Err(error) => exec.note(&format!("warning: cannot move the saved sources: {error}")),
    }

    state.boot_id = state::boot_id();
    set(&mut state, Phase::Rebooting, None)?;
    match exec.run("/usr/bin/systemctl", &["reboot"], &[]) {
        Ok((0, _)) => Ok(state),
        Ok((code, output)) => {
            set(
                &mut state,
                Phase::Failed,
                Some(format!(
                    "the upgrade is done but `systemctl reboot` exited with {code}: {}; reboot the server",
                    last_error(&output)
                )),
            )?;
            Ok(state)
        }
        Err(e) => {
            set(
                &mut state,
                Phase::Failed,
                Some(format!("cannot reboot: {e}; reboot the server")),
            )?;
            Ok(state)
        }
    }
}

/// `apt-get update`, failing on any warning or error line, as apt before
/// 2.1 exits 0 when an index fails (ADR-0032).
fn refresh(exec: &mut dyn Exec) -> Result<(), String> {
    let (code, output) = exec
        .run("/usr/bin/apt-get", &["-q", "update"], APT_ENV)
        .map_err(|e| format!("cannot run apt-get update: {e}"))?;
    let bad = output
        .lines()
        .find(|l| l.starts_with("W:") || l.starts_with("E:") || l.starts_with("Err:"));
    match (code, bad) {
        (0, None) => Ok(()),
        (_, Some(line)) => Err(format!("`apt-get update` failed: {}", line.trim())),
        (code, None) => Err(format!("`apt-get update` exited with {code}")),
    }
}

fn last_error(output: &str) -> String {
    output
        .lines()
        .rev()
        .find(|l| l.starts_with("E:"))
        .or_else(|| output.lines().rev().find(|l| !l.trim().is_empty()))
        .unwrap_or("")
        .trim()
        .chars()
        .take(300)
        .collect()
}

fn disk(root: &Path, path: &Path) -> PathBuf {
    root.join(path.strip_prefix("/").unwrap_or(path))
}

/// Copies every file before changing any, then writes each through a
/// temporary file and a rename, keeping its mode and owner.
fn save_and_write(paths: &Paths, files: &[&FilePlan]) -> io::Result<()> {
    let backup = paths.state_dir.join(BACKUP);
    let _ = fs::remove_dir_all(&backup);
    fs::create_dir_all(&backup)?;
    fs::set_permissions(&backup, fs::Permissions::from_mode(0o700))?;
    for file in files {
        let copy = disk(&backup, &file.path);
        if let Some(parent) = copy.parent() {
            fs::create_dir_all(parent)?;
        }
        write_atomic(&copy, file.original.as_bytes(), None)?;
    }
    for file in files {
        let target = disk(&paths.root, &file.path);
        let meta = fs::metadata(&target)?;
        write_atomic(&target, file.rewritten.as_bytes(), Some(&meta))?;
    }
    Ok(())
}

/// Puts back the copies [`save_and_write`] made.
fn restore(paths: &Paths, files: &[&FilePlan]) -> io::Result<()> {
    let backup = paths.state_dir.join(BACKUP);
    for file in files {
        let original = fs::read(disk(&backup, &file.path))?;
        let target = disk(&paths.root, &file.path);
        let meta = fs::metadata(&target).ok();
        write_atomic(&target, &original, meta.as_ref())?;
    }
    Ok(())
}

fn write_atomic(target: &Path, content: &[u8], like: Option<&fs::Metadata>) -> io::Result<()> {
    let name = target
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let temporary = target.with_file_name(format!(".{name}.mf-upgrade"));
    let _ = fs::remove_file(&temporary);
    let mode = like.map_or(0o600, |m| m.mode() & 0o7777);
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(mode)
        .open(&temporary)?;
    file.write_all(content)?;
    file.sync_all()?;
    if let Some(meta) = like {
        fs::set_permissions(&temporary, fs::Permissions::from_mode(mode))?;
        if rustix::process::geteuid().is_root() {
            std::os::unix::fs::chown(&temporary, Some(meta.uid()), Some(meta.gid()))?;
        }
    }
    fs::rename(&temporary, target)?;
    if let Some(parent) = target.parent() {
        File::open(parent)?.sync_all()?;
    }
    Ok(())
}

/// `major-upgrades on|off`, as root: sets `packages.major_upgrades` in the
/// configuration file at `path`, keeping every other line. Returns whether
/// the file changed.
pub fn set_consent(path: &Path, on: bool) -> Result<bool, String> {
    let text =
        fs::read_to_string(path).map_err(|e| format!("cannot read {}: {e}", path.display()))?;
    let mut document: toml_edit::DocumentMut = text
        .parse()
        .map_err(|e| format!("{} is not valid TOML: {e}", path.display()))?;
    document
        .entry("packages")
        .or_insert_with(toml_edit::table)
        .as_table_like_mut()
        .ok_or_else(|| format!("{}: `packages` is not a table", path.display()))?
        .insert("major_upgrades", toml_edit::value(on));
    let edited = document.to_string();
    let config: AgentConfig =
        config::parse_with_env(&edited, path, &Env::new()).map_err(|e| e.to_string())?;
    if config.packages.major_upgrades != on {
        return Err(format!("{}: the setting did not apply", path.display()));
    }
    if edited == text {
        return Ok(false);
    }
    fs::write(path, edited).map_err(|e| format!("cannot write {}: {e}", path.display()))?;
    Ok(true)
}
