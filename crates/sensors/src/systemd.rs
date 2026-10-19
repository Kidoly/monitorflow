//! State of systemd units, from `systemctl show`. See ADR-0021.
//!
//! Metrics (prefixed with the configured name, `systemd` by default), with
//! the unit name as instance:
//! - `loaded`: 1 when systemd found and loaded the unit file;
//! - `active`: 1 when `ActiveState` is `active`, `reloading` or
//!   `refreshing`, so a unit that does not exist reports 0 and can alert;
//! - `failed`: 1 when `ActiveState` is `failed`;
//! - `restarts`: `NRestarts`, automatic restarts counted by systemd
//!   (services only);
//! - `state_age_seconds`: time since the last change of `ActiveState`,
//!   omitted when the unit never changed state.
//!
//! Parameter `units` (required): a comma-separated list of unit names. A
//! name without a known type suffix gets `.service`.
//!
//! The sensor runs `systemctl show` directly, without a shell, once per
//! collection for all its units, bounded by the deadline.

use std::collections::BTreeSet;
use std::path::PathBuf;
use std::process::Stdio;

use tokio::time::Instant;

use crate::registry::{BuildError, Params};
use crate::{Sample, Sensor, SensorError};

/// Most units one sensor may watch.
pub const MAX_UNITS: usize = 64;
/// systemd's `UNIT_NAME_MAX`.
const UNIT_NAME_MAX: usize = 255;
/// Unit types, from systemd.unit(5).
const UNIT_TYPES: &[&str] = &[
    "service",
    "socket",
    "device",
    "mount",
    "automount",
    "swap",
    "target",
    "path",
    "timer",
    "slice",
    "scope",
];
/// Asked in this order; `Id` starts each unit's block in the output.
const PROPERTIES: &str =
    "Id,LoadState,ActiveState,SubState,NRestarts,StateChangeTimestampMonotonic";
/// Exists when the host was booted with systemd, see sd_booted(3).
const BOOTED_MARKER: &str = "/run/systemd/system";
/// Bytes of stderr kept in an error message.
const STDERR_SHOWN: usize = 200;

/// Checks a unit name and adds `.service` when it has no type suffix.
pub fn unit_name(name: &str) -> Result<String, String> {
    let has_type = name
        .rsplit_once('.')
        .is_some_and(|(_, suffix)| UNIT_TYPES.contains(&suffix));
    let name = if has_type {
        name.to_owned()
    } else {
        format!("{name}.service")
    };
    let (stem, _) = name.rsplit_once('.').unwrap_or_default();
    // systemd's charset; `\` for its escapes like `\x2d`. A leading `-`
    // would read as an option, `@` alone is a template with no instance.
    let charset = name
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || ":-_.\\@".contains(c));
    if name.len() > UNIT_NAME_MAX || !charset || stem.is_empty() {
        return Err(format!(
            "`{}` is not a unit name: at most {UNIT_NAME_MAX} bytes of [A-Za-z0-9:_.\\@-]",
            name.escape_debug()
        ));
    }
    if name.starts_with(['-', '.', '@']) || stem.ends_with('@') {
        return Err(format!("`{name}` is not a unit name"));
    }
    Ok(name)
}

/// What `systemctl show` says about one unit.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct UnitState {
    pub load_state: String,
    pub active_state: String,
    pub sub_state: String,
    /// Services only.
    pub restarts: Option<u64>,
    /// `CLOCK_MONOTONIC` in µs; `None` when the unit never changed state.
    pub state_change_us: Option<u64>,
}

/// Parses `systemctl show --property=Id,...` for `count` units. Each unit
/// is a block starting with `Id=`, in the order the units were asked; the
/// `Id` itself may differ (an alias like `dbus.service` shows its target),
/// so units are matched by position. Unknown lines are ignored.
pub fn parse_show(text: &str, count: usize) -> Result<Vec<UnitState>, SensorError> {
    let malformed = |what: &str| SensorError::Transient(format!("systemctl show: {what}"));
    let mut units: Vec<UnitState> = Vec::with_capacity(count);
    for line in text.lines().map(str::trim).filter(|l| !l.is_empty()) {
        let Some((key, value)) = line.split_once('=') else {
            return Err(malformed("line without `=`"));
        };
        if key == "Id" {
            units.push(UnitState::default());
            continue;
        }
        let Some(unit) = units.last_mut() else {
            return Err(malformed("output does not start with `Id=`"));
        };
        let number = || value.parse::<u64>().map_err(|_| malformed("odd number"));
        match key {
            "LoadState" => unit.load_state = value.to_owned(),
            "ActiveState" => unit.active_state = value.to_owned(),
            "SubState" => unit.sub_state = value.to_owned(),
            // Empty when systemd does not know it, like on old versions.
            "NRestarts" if !value.is_empty() => unit.restarts = Some(number()?),
            "StateChangeTimestampMonotonic" if !value.is_empty() => {
                unit.state_change_us = Some(number()?).filter(|us| *us > 0);
            }
            _ => {}
        }
    }
    if units.len() != count {
        return Err(malformed(&format!("{} units, {count} asked", units.len())));
    }
    Ok(units)
}

impl UnitState {
    /// Samples for `unit`, given `CLOCK_MONOTONIC` now in µs.
    pub fn samples(&self, unit: &str, now_us: u64) -> Vec<Sample> {
        let sample = |metric: &str, value: f64| Sample::metric(metric, value).with_instance(unit);
        let flag = |b: bool| if b { 1.0 } else { 0.0 };
        let active = matches!(
            self.active_state.as_str(),
            "active" | "reloading" | "refreshing"
        );
        let mut samples = vec![
            sample("loaded", flag(self.load_state == "loaded")),
            sample("active", flag(active)),
            sample("failed", flag(self.active_state == "failed")),
        ];
        if let Some(restarts) = self.restarts {
            samples.push(sample("restarts", restarts as f64));
        }
        if let Some(age) = self.state_change_us.and_then(|t| now_us.checked_sub(t)) {
            samples.push(sample("state_age_seconds", age as f64 / 1e6));
        }
        samples
    }
}

/// Runs `systemctl show` for some units and returns its standard output.
#[async_trait::async_trait]
pub trait Systemctl: Send + Sync + 'static {
    async fn show(&self, units: &[String], deadline: Instant) -> Result<String, SensorError>;
}

/// The real `systemctl`, found on `PATH`.
#[derive(Debug, Clone)]
pub struct Command {
    program: PathBuf,
    booted_marker: PathBuf,
}

impl Default for Command {
    fn default() -> Self {
        Self::new("systemctl", BOOTED_MARKER)
    }
}

impl Command {
    /// `program` with `booted_marker` as the sd_booted(3) check; tests
    /// point both elsewhere.
    pub fn new(program: impl Into<PathBuf>, booted_marker: impl Into<PathBuf>) -> Self {
        Self {
            program: program.into(),
            booted_marker: booted_marker.into(),
        }
    }
}

#[async_trait::async_trait]
impl Systemctl for Command {
    async fn show(&self, units: &[String], deadline: Instant) -> Result<String, SensorError> {
        // One stat of a tmpfs path: fine on the runtime.
        if !self.booted_marker.is_dir() {
            return Err(SensorError::Permanent(
                "this host was not booted with systemd".into(),
            ));
        }
        let child = tokio::process::Command::new(&self.program)
            .args(["show", "--no-pager", "--property", PROPERTIES, "--"])
            .args(units)
            .env("LC_ALL", "C")
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true)
            .spawn()
            .map_err(|e| {
                let message = format!("cannot run {}: {e}", self.program.display());
                match e.kind() {
                    std::io::ErrorKind::NotFound => SensorError::Permanent(message),
                    _ => SensorError::Transient(message),
                }
            })?;
        // Dropping the future on timeout kills the child.
        let output = tokio::time::timeout_at(deadline, child.wait_with_output())
            .await
            .map_err(|_| SensorError::Transient("systemctl did not answer in time".into()))?
            .map_err(|e| SensorError::Transient(format!("systemctl: {e}")))?;
        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            let first: String = stderr
                .lines()
                .next()
                .unwrap_or("")
                .chars()
                .take(STDERR_SHOWN)
                .collect();
            return Err(SensorError::Transient(format!(
                "systemctl show failed ({}): {}",
                output.status,
                first.escape_debug()
            )));
        }
        String::from_utf8(output.stdout)
            .map_err(|_| SensorError::Transient("systemctl show: output is not UTF-8".into()))
    }
}

/// `CLOCK_MONOTONIC` in µs, the clock of systemd's `*Monotonic` stamps.
fn monotonic_us() -> u64 {
    let now = rustix::time::clock_gettime(rustix::time::ClockId::Monotonic);
    let secs = u64::try_from(now.tv_sec).unwrap_or(0);
    let micros = u64::try_from(now.tv_nsec).unwrap_or(0) / 1000;
    secs.saturating_mul(1_000_000).saturating_add(micros)
}

pub struct SystemdSensor {
    units: Vec<String>,
    systemctl: Box<dyn Systemctl>,
    clock: fn() -> u64,
}

impl std::fmt::Debug for SystemdSensor {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SystemdSensor")
            .field("units", &self.units)
            .finish_non_exhaustive()
    }
}

impl SystemdSensor {
    /// Watches `units` (already checked by [`unit_name`]) through
    /// `systemctl`, with `clock` giving `CLOCK_MONOTONIC` in µs.
    pub fn new(units: Vec<String>, systemctl: impl Systemctl, clock: fn() -> u64) -> Self {
        Self {
            units,
            systemctl: Box::new(systemctl),
            clock,
        }
    }

    pub(crate) fn from_params(params: &mut Params<'_>) -> Result<Self, BuildError> {
        let Some(names) = params.list("units")? else {
            return Err(params.bad("units", "is required"));
        };
        if names.len() > MAX_UNITS {
            return Err(params.bad("units", format!("may list at most {MAX_UNITS} units")));
        }
        let mut units = Vec::with_capacity(names.len());
        let mut seen = BTreeSet::new();
        for name in names {
            let unit = unit_name(&name).map_err(|reason| params.bad("units", reason))?;
            if !seen.insert(unit.clone()) {
                return Err(params.bad("units", format!("lists `{unit}` twice")));
            }
            units.push(unit);
        }
        Ok(Self::new(units, Command::default(), monotonic_us))
    }
}

#[async_trait::async_trait]
impl Sensor for SystemdSensor {
    async fn collect(&self, deadline: Instant) -> Result<Vec<Sample>, SensorError> {
        let text = self.systemctl.show(&self.units, deadline).await?;
        let now = (self.clock)();
        let states = parse_show(&text, self.units.len())?;
        Ok(self
            .units
            .iter()
            .zip(states)
            .flat_map(|(unit, state)| state.samples(unit, now))
            .collect())
    }
}

#[cfg(test)]
mod tests;
