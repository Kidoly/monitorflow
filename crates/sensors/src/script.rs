//! Custom checks written by the host's administrator. See ADR-0024.
//!
//! The server's configuration only names a script; the agent runs it only
//! if an administrator installed it in the agent's script directory
//! ([`Local::script_dir`]), owned by root or the agent's user and not
//! writable by anyone else. Checked before every run.
//!
//! Metrics (prefixed with the configured name), with label `script`:
//! - `status`: the Nagios exit code, 0 OK, 1 WARNING, 2 CRITICAL,
//!   3 UNKNOWN; also 3 when the script timed out, was killed by a signal,
//!   exited with another code or wrote more than [`MAX_OUTPUT`];
//! - `duration_seconds`: how long the run took;
//! - one gauge per `name=value` line of standard output (perfdata), at
//!   most [`MAX_PERFDATA`]; other lines are ignored.
//!
//! Parameters: `script` (required, a file name) and `args` (optional, a
//! comma-separated list of plain words passed as arguments).
//!
//! The script runs without a shell, with an empty environment apart from
//! `PATH` and `LC_ALL=C`, in `/`, in its own process group, which is
//! killed at the deadline and after every run.

use std::io;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::process::{ExitStatus, Stdio};
use std::time::Duration;

use tokio::io::AsyncReadExt;
use tokio::time::{timeout_at, Instant};

use crate::registry::{BuildError, Local, Params};
use crate::{validate, Sample, Sensor, SensorError};

/// Default [`Local::script_dir`].
pub const DEFAULT_DIR: &str = "/etc/monitorflow/scripts.d";
/// Most bytes of standard output read; a script writing more is UNKNOWN.
pub const MAX_OUTPUT: usize = 64 * 1024;
/// Most perfdata gauges per run; later lines are ignored.
pub const MAX_PERFDATA: usize = 32;
/// Most arguments.
pub const MAX_ARGS: usize = 8;
const MAX_ARG_LEN: usize = 128;
const MAX_NAME_LEN: usize = 64;
/// Nagios' UNKNOWN.
const UNKNOWN: f64 = 3.0;
/// Metric suffixes the sensor sets itself; perfdata cannot use them.
const OWN_METRICS: [&str; 2] = ["status", "duration_seconds"];
/// Time kept before the deadline to report UNKNOWN rather than be
/// cancelled with nothing, which could not alert.
const MARGIN: Duration = Duration::from_millis(200);
/// The only environment, with `LC_ALL=C`.
const PATH: &str = "/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin";

/// A file name of `[A-Za-z0-9_.-]`, not starting with `.` or `-`: no path
/// separator, no `..`, nothing hidden or read as an option.
pub fn script_name(name: &str) -> Result<(), String> {
    let ok = !name.is_empty()
        && name.len() <= MAX_NAME_LEN
        && !name.starts_with(['.', '-'])
        && !name.contains("..")
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || "_.-".contains(c));
    ok.then_some(()).ok_or_else(|| {
        format!(
            "`{}` is not a script name: a file name of at most {MAX_NAME_LEN} bytes of [A-Za-z0-9_.-], not starting with `.` or `-`",
            shown(name)
        )
    })
}

/// A plain word: `[A-Za-z0-9_.:/=@%+-]`, at most 128 bytes. There is no
/// shell, so this only keeps arguments readable in logs and unsurprising
/// for scripts that pass them on.
pub fn script_arg(arg: &str) -> Result<(), String> {
    let ok = arg.len() <= MAX_ARG_LEN
        && arg
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || "_.:/=@%+-".contains(c));
    ok.then_some(()).ok_or_else(|| {
        format!(
            "`{}` is not a plain argument: at most {MAX_ARG_LEN} bytes of [A-Za-z0-9_.:/=@%+-]",
            shown(arg)
        )
    })
}

fn shown(text: &str) -> String {
    text.chars()
        .take(64)
        .collect::<String>()
        .escape_debug()
        .to_string()
}

/// Checks that `name` is installed safely in `dir` and returns the path to
/// run, symlinks resolved. The directory, the file and the file's own
/// directory must be owned by one of `owners` and not writable by group
/// or other; the file must be a regular executable starting with `#!` or
/// an ELF header, so nothing falls back to a shell.
pub fn check(dir: &Path, name: &str, owners: &[u32]) -> Result<PathBuf, String> {
    script_name(name)?;
    check_dir(dir, owners)?;
    let path = std::fs::canonicalize(dir.join(name)).map_err(|e| match e.kind() {
        io::ErrorKind::NotFound => format!("script `{name}` is not installed in {}", dir.display()),
        _ => format!("script `{name}`: {e}"),
    })?;
    let meta = std::fs::metadata(&path).map_err(|e| format!("script `{name}`: {e}"))?;
    let refuse = |why: &str| Err(format!("script `{name}` ({}) {why}", path.display()));
    if !meta.is_file() {
        return refuse("is not a regular file");
    }
    if !owners.contains(&meta.uid()) {
        return refuse(&format!(
            "is owned by uid {}, not root or the agent",
            meta.uid()
        ));
    }
    if meta.mode() & 0o022 != 0 {
        return refuse("is writable by group or other");
    }
    if meta.mode() & 0o111 == 0 {
        return refuse("is not executable");
    }
    if let Some(parent) = path.parent().filter(|p| *p != dir) {
        check_dir(parent, owners)?;
    }
    let mut magic = [0u8; 4];
    let read = std::fs::File::open(&path)
        .and_then(|mut f| io::Read::read(&mut f, &mut magic))
        .map_err(|e| format!("script `{name}`: {e}"))?;
    if !(magic[..read].starts_with(b"#!") || magic[..read] == *b"\x7fELF") {
        return refuse("does not start with `#!` nor is an ELF binary");
    }
    Ok(path)
}

fn check_dir(dir: &Path, owners: &[u32]) -> Result<(), String> {
    let meta =
        std::fs::metadata(dir).map_err(|e| format!("script directory {}: {e}", dir.display()))?;
    let refuse = |why: String| Err(format!("script directory {} {why}", dir.display()));
    if !meta.is_dir() {
        return refuse("is not a directory".into());
    }
    if !owners.contains(&meta.uid()) {
        return refuse(format!(
            "is owned by uid {}, not root or the agent",
            meta.uid()
        ));
    }
    if meta.mode() & 0o022 != 0 {
        return refuse("is writable by group or other".into());
    }
    Ok(())
}

/// Perfdata: `name=value` lines whose name is a metric suffix (ADR-0015)
/// other than `status` and `duration_seconds` and whose value is a finite
/// number. The first of a repeated name wins; other lines are ignored.
pub fn parse_perfdata(output: &str) -> Vec<(String, f64)> {
    let mut gauges: Vec<(String, f64)> = Vec::new();
    for line in output.lines() {
        let Some((name, value)) = line.trim().split_once('=') else {
            continue;
        };
        let (name, value) = (name.trim(), value.trim());
        let ok = validate::sensor_name(name).is_ok()
            && !OWN_METRICS.contains(&name)
            && !gauges.iter().any(|(n, _)| n == name);
        match value.parse::<f64>() {
            Ok(v) if ok && v.is_finite() => gauges.push((name.to_owned(), v)),
            _ => continue,
        }
        if gauges.len() == MAX_PERFDATA {
            break;
        }
    }
    gauges
}

/// The Nagios status of an exit: its code from 0 to 3, else UNKNOWN.
pub fn status(exit: ExitStatus) -> f64 {
    match exit.code() {
        Some(code @ 0..=3) => f64::from(code),
        _ => UNKNOWN,
    }
}

/// Kills a process group when dropped: at the deadline, when the
/// scheduler cancels the collection, and after a normal exit, so nothing
/// the script started in the background outlives the run.
struct Group(Option<rustix::process::Pid>);

impl Drop for Group {
    fn drop(&mut self) {
        if let Some(pid) = self.0 {
            // ESRCH when the group is already gone.
            let _ = rustix::process::kill_process_group(pid, rustix::process::Signal::KILL);
        }
    }
}

#[derive(Debug)]
pub struct ScriptSensor {
    script: String,
    args: Vec<String>,
    dir: PathBuf,
    /// Allowed owners of the script and its directories.
    owners: Vec<u32>,
}

impl ScriptSensor {
    /// Runs `script` from `dir` with `args`, all already checked, when it
    /// is owned by one of `owners`.
    pub fn new(script: String, args: Vec<String>, dir: PathBuf, owners: Vec<u32>) -> Self {
        Self {
            script,
            args,
            dir,
            owners,
        }
    }

    pub(crate) fn from_params(
        params: &mut Params<'_>,
        local: Option<&Local>,
    ) -> Result<Self, BuildError> {
        let Some(script) = params.get("script") else {
            return Err(params.bad("script", "is required"));
        };
        script_name(script).map_err(|reason| params.bad("script", reason))?;
        let args = params.list("args")?.unwrap_or_default();
        if args.len() > MAX_ARGS {
            return Err(params.bad("args", format!("may list at most {MAX_ARGS} arguments")));
        }
        for arg in &args {
            script_arg(arg).map_err(|reason| params.bad("args", reason))?;
        }
        let owners = vec![0, rustix::process::geteuid().as_raw()];
        let dir = local.map_or_else(|| DEFAULT_DIR.into(), |l| l.script_dir.clone());
        let sensor = Self::new(script.to_owned(), args, dir, owners);
        if local.is_some() {
            check(&sensor.dir, &sensor.script, &sensor.owners)
                .map_err(|reason| params.refused(reason))?;
        }
        Ok(sensor)
    }

    /// Runs the script until `cutoff`: its exit, or `None` when it timed
    /// out or wrote too much, and its output.
    async fn run(
        &self,
        path: &Path,
        cutoff: Instant,
    ) -> Result<Option<(ExitStatus, Vec<u8>)>, SensorError> {
        let mut child = tokio::process::Command::new(path)
            .args(&self.args)
            .env_clear()
            .env("PATH", PATH)
            .env("LC_ALL", "C")
            .current_dir("/")
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .process_group(0)
            .kill_on_drop(true)
            .spawn()
            .map_err(|e| {
                let message = format!("cannot run script `{}`: {e}", self.script);
                match e.kind() {
                    io::ErrorKind::NotFound | io::ErrorKind::PermissionDenied => {
                        SensorError::Permanent(message)
                    }
                    _ => SensorError::Transient(message),
                }
            })?;
        // The child leads its own group, whose ID is its PID.
        let _group = Group(
            child
                .id()
                .and_then(|id| rustix::process::Pid::from_raw(id as i32)),
        );
        let mut stdout = child.stdout.take().expect("stdout is piped");
        let run = async {
            let mut output = Vec::new();
            (&mut stdout)
                .take(MAX_OUTPUT as u64 + 1)
                .read_to_end(&mut output)
                .await?;
            if output.len() > MAX_OUTPUT {
                return Ok(None);
            }
            Ok::<_, io::Error>(Some((child.wait().await?, output)))
        };
        match timeout_at(cutoff, run).await {
            Ok(Ok(Some(done))) => Ok(Some(done)),
            Ok(Ok(None)) => {
                tracing::debug!(script = %self.script, "output over {MAX_OUTPUT} bytes");
                Ok(None)
            }
            Ok(Err(e)) => Err(SensorError::Transient(format!(
                "script `{}`: {e}",
                self.script
            ))),
            Err(_) => {
                tracing::debug!(script = %self.script, "timed out");
                Ok(None)
            }
        }
    }
}

#[async_trait::async_trait]
impl Sensor for ScriptSensor {
    async fn collect(&self, deadline: Instant) -> Result<Vec<Sample>, SensorError> {
        let started = Instant::now();
        let cutoff = deadline - MARGIN.min(deadline.saturating_duration_since(started) / 2);
        // A few stats of a local directory, like the /proc reads.
        let path = check(&self.dir, &self.script, &self.owners).map_err(SensorError::Permanent)?;
        let outcome = self.run(&path, cutoff).await?;
        let sample = |metric: &str, value: f64| {
            Sample::metric(metric, value).with_label("script", &self.script)
        };
        let mut samples = vec![
            sample(
                "status",
                outcome.as_ref().map_or(UNKNOWN, |(exit, _)| status(*exit)),
            ),
            sample("duration_seconds", started.elapsed().as_secs_f64()),
        ];
        if let Some((_, output)) = outcome {
            let output = String::from_utf8_lossy(&output);
            samples.extend(
                parse_perfdata(&output)
                    .into_iter()
                    .map(|(n, v)| sample(&n, v)),
            );
        }
        Ok(samples)
    }
}

#[cfg(test)]
mod tests;
