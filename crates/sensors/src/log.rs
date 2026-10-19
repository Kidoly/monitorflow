//! Lines matching a pattern in a log file. See ADR-0024.
//!
//! The server's configuration names the file; the agent reads it only if
//! it resolves, symlinks included, under one of its own log directories
//! ([`Local::log_dirs`]). Checked before every collection.
//!
//! Metrics (prefixed with the configured name), with label `path`:
//! - `matches`: lines that matched since the previous collection;
//! - `lag_bytes`: bytes written but not read yet, above zero when the
//!   file grows faster than [`MAX_READ`] per collection.
//!
//! Parameters: `path` (required, absolute), `pattern` (required, a regular
//! expression of the `regex` crate, which runs in linear time) and
//! `exclude` (optional, lines matching it are not counted).
//!
//! The first collection starts at the end of the file: history is not
//! counted. A file created later is read from its start. Rotation by
//! rename is followed (the old file is read to its end first) and so is
//! truncation. Only the first [`MAX_LINE`] bytes of a line are matched.

use std::fs::File;
use std::io;
use std::os::unix::fs::{FileExt, MetadataExt, OpenOptionsExt};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use regex::bytes::{Regex, RegexBuilder};
use tokio::time::{timeout_at, Instant};

use crate::registry::{BuildError, Local, Params};
use crate::{validate, Sample, Sensor, SensorError};

/// Default [`Local::log_dirs`].
pub const DEFAULT_DIRS: &[&str] = &["/var/log"];
/// Most bytes read per collection; the rest waits for the next one.
pub const MAX_READ: u64 = 1024 * 1024;
/// Bytes of a line that are matched; the rest of a longer line is skipped.
pub const MAX_LINE: usize = 4096;
/// Longest pattern, in bytes.
pub const MAX_PATTERN: usize = 1024;
/// Compiled size limit of a pattern, so `(a{1000}){1000}` is refused.
const REGEX_SIZE_LIMIT: usize = 1024 * 1024;
const CHUNK: usize = 64 * 1024;
/// Time kept before the deadline, so a slow read is reported as such.
const MARGIN: Duration = Duration::from_millis(200);

/// An absolute path without `.` or `..` segments, short enough to be a
/// label value.
pub fn log_path(path: &str) -> Result<PathBuf, String> {
    let ok = path.starts_with('/')
        && validate::instance(path).is_ok()
        && path
            .split('/')
            .skip(1)
            .all(|s| !s.is_empty() && s != "." && s != "..");
    ok.then(|| PathBuf::from(path)).ok_or_else(|| {
        format!(
            "`{}` must be an absolute path without `.` or `..`, at most {} bytes",
            path.chars().take(64).collect::<String>().escape_debug(),
            validate::MAX_VALUE_LEN
        )
    })
}

/// Compiles a pattern, bounded in length and compiled size.
pub fn pattern(text: &str) -> Result<Regex, String> {
    if text.is_empty() || text.len() > MAX_PATTERN {
        return Err(format!("must be 1 to {MAX_PATTERN} bytes"));
    }
    RegexBuilder::new(text)
        .size_limit(REGEX_SIZE_LIMIT)
        .dfa_size_limit(REGEX_SIZE_LIMIT)
        .build()
        .map_err(|e| format!("is not a valid pattern: {}", first_line(&e.to_string())))
}

fn first_line(text: &str) -> String {
    text.lines()
        .last()
        .unwrap_or("")
        .chars()
        .take(200)
        .collect()
}

/// `path` resolved, if it lies under one of `dirs` once both are resolved.
/// `Ok(None)` when the file does not exist.
pub fn resolve(path: &Path, dirs: &[PathBuf]) -> Result<Option<PathBuf>, String> {
    let real = match std::fs::canonicalize(path) {
        Ok(real) => real,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(format!("{}: {e}", path.display())),
    };
    let allowed = dirs
        .iter()
        .filter_map(|d| std::fs::canonicalize(d).ok())
        .any(|d| real.starts_with(d));
    if !allowed {
        return Err(format!(
            "{} resolves to {}, outside the agent's log directories",
            path.display(),
            real.display()
        ));
    }
    Ok(Some(real))
}

/// The file being read.
struct Tracked {
    file: File,
    dev: u64,
    ino: u64,
    offset: u64,
}

#[derive(Default)]
struct State {
    tracked: Option<Tracked>,
    /// A first collection ran; files found from now on are new.
    primed: bool,
    /// Start of a line whose end was not read yet, cut at [`MAX_LINE`].
    partial: Vec<u8>,
}

/// What one collection found.
struct Round {
    matches: u64,
    lag: u64,
}

pub struct LogSensor(Arc<Reader>);

/// The sensor's configuration and state, shared with the blocking read.
struct Reader {
    path: PathBuf,
    pattern: Regex,
    exclude: Option<Regex>,
    dirs: Vec<PathBuf>,
    state: Mutex<State>,
}

impl std::fmt::Debug for LogSensor {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LogSensor")
            .field("path", &self.0.path)
            .field("pattern", &self.0.pattern.as_str())
            .finish_non_exhaustive()
    }
}

impl LogSensor {
    pub(crate) fn from_params(
        params: &mut Params<'_>,
        local: Option<&Local>,
    ) -> Result<Self, BuildError> {
        let Some(path) = params.get("path") else {
            return Err(params.bad("path", "is required"));
        };
        let path = log_path(path).map_err(|reason| params.bad("path", reason))?;
        let Some(text) = params.get("pattern") else {
            return Err(params.bad("pattern", "is required"));
        };
        let pattern = pattern(text).map_err(|reason| params.bad("pattern", reason))?;
        let exclude = match params.get("exclude") {
            None => None,
            Some(text) => Some(self::pattern(text).map_err(|r| params.bad("exclude", r))?),
        };
        let dirs = local.map_or_else(
            || DEFAULT_DIRS.iter().map(PathBuf::from).collect(),
            |l| l.log_dirs.clone(),
        );
        if local.is_some() {
            if !dirs.iter().any(|d| path.starts_with(d)) {
                return Err(params.refused(format!(
                    "{} is not under the agent's log directories (sensors.log_dirs)",
                    path.display()
                )));
            }
            // A stat or two of a local path.
            resolve(&path, &dirs).map_err(|reason| params.refused(reason))?;
        }
        Ok(Self(Arc::new(Reader {
            path,
            pattern,
            exclude,
            dirs,
            state: Mutex::default(),
        })))
    }
}

impl Reader {
    fn matches(&self, line: &[u8]) -> bool {
        let line = line.strip_suffix(b"\r").unwrap_or(line);
        self.pattern.is_match(line) && !self.exclude.as_ref().is_some_and(|e| e.is_match(line))
    }

    /// One collection, on the blocking pool.
    fn read(&self, state: &mut State) -> Result<Round, SensorError> {
        let first = !std::mem::replace(&mut state.primed, true);
        let real = resolve(&self.path, &self.dirs).map_err(SensorError::Permanent)?;
        let io = |e: io::Error| SensorError::Transient(format!("{}: {e}", self.path.display()));
        let mut round = Round { matches: 0, lag: 0 };
        let mut budget = MAX_READ;

        // Finish the file we hold, even if it was renamed away.
        if let Some(tracked) = &mut state.tracked {
            let size = tracked.file.metadata().map_err(io)?.len();
            if size < tracked.offset {
                // Truncated in place (copytruncate).
                tracked.offset = 0;
                state.partial.clear();
            }
            round.matches += self
                .drain(tracked, &mut state.partial, &mut budget)
                .map_err(io)?;
            round.lag = size.saturating_sub(tracked.offset);
        }
        let Some(real) = real else {
            if state.tracked.is_some() {
                return Ok(round);
            }
            return Err(SensorError::Transient(format!(
                "{} does not exist",
                self.path.display()
            )));
        };
        let current = std::fs::metadata(&real).map_err(io)?;
        let same = state
            .tracked
            .as_ref()
            .is_some_and(|t| (t.dev, t.ino) == (current.dev(), current.ino()));
        if same || round.lag > 0 {
            // Still on this file, or the old one is not read to its end.
            return Ok(round);
        }
        if !current.is_file() {
            return Err(SensorError::Permanent(format!(
                "{} is not a regular file",
                real.display()
            )));
        }
        // A new file: the first one at its end, a later one from its start.
        if state.tracked.take().is_some() && !state.partial.is_empty() {
            // The old file ended without a newline.
            round.matches += u64::from(self.matches(&state.partial));
        }
        state.partial.clear();
        // O_NONBLOCK in case a FIFO replaced the file since the check;
        // O_NOFOLLOW in case a symlink did.
        let file = std::fs::OpenOptions::new()
            .read(true)
            .custom_flags(
                (rustix::fs::OFlags::NONBLOCK | rustix::fs::OFlags::NOFOLLOW).bits() as i32,
            )
            .open(&real)
            .map_err(io)?;
        let meta = file.metadata().map_err(io)?;
        if !meta.is_file() {
            return Err(SensorError::Permanent(format!(
                "{} is not a regular file",
                real.display()
            )));
        }
        let mut tracked = Tracked {
            file,
            dev: meta.dev(),
            ino: meta.ino(),
            offset: if first { meta.len() } else { 0 },
        };
        round.matches += self
            .drain(&mut tracked, &mut state.partial, &mut budget)
            .map_err(io)?;
        round.lag = meta.len().saturating_sub(tracked.offset);
        state.tracked = Some(tracked);
        Ok(round)
    }

    /// Reads `tracked` from its offset to its end or until `budget` is
    /// spent, counting complete matching lines.
    fn drain(
        &self,
        tracked: &mut Tracked,
        partial: &mut Vec<u8>,
        budget: &mut u64,
    ) -> io::Result<u64> {
        let mut matches = 0;
        let mut buf = vec![0; CHUNK];
        while *budget > 0 {
            let want = buf
                .len()
                .min(usize::try_from(*budget).unwrap_or(usize::MAX));
            let read = tracked.file.read_at(&mut buf[..want], tracked.offset)?;
            if read == 0 {
                break;
            }
            tracked.offset += read as u64;
            *budget -= read as u64;
            for piece in buf[..read].split_inclusive(|b| *b == b'\n') {
                let (body, complete) = match piece.strip_suffix(b"\n") {
                    Some(body) => (body, true),
                    None => (piece, false),
                };
                let room = MAX_LINE - partial.len();
                partial.extend_from_slice(&body[..room.min(body.len())]);
                if complete {
                    matches += u64::from(self.matches(partial));
                    partial.clear();
                }
            }
        }
        Ok(matches)
    }
}

#[async_trait::async_trait]
impl Sensor for LogSensor {
    async fn collect(&self, deadline: Instant) -> Result<Vec<Sample>, SensorError> {
        let now = Instant::now();
        let cutoff = deadline - MARGIN.min(deadline.saturating_duration_since(now) / 2);
        let reader = self.0.clone();
        // Bounded by MAX_READ; one that outlives the deadline keeps the
        // lock, so the next collection does not start another.
        let read = tokio::task::spawn_blocking(move || {
            let Ok(mut state) = reader.state.try_lock() else {
                return Err(SensorError::Transient(
                    "the previous read is still running".into(),
                ));
            };
            reader.read(&mut state)
        });
        let round = match timeout_at(cutoff, read).await {
            Ok(Ok(round)) => round?,
            Ok(Err(e)) => return Err(SensorError::Transient(format!("log read failed: {e}"))),
            Err(_) => {
                return Err(SensorError::Transient(
                    "log read did not finish in time".into(),
                ))
            }
        };
        let path = self.0.path.to_string_lossy();
        let sample = |metric: &str, value: f64| {
            Sample::metric(metric, value).with_label("path", path.as_ref())
        };
        Ok(vec![
            sample("matches", round.matches as f64),
            sample("lag_bytes", round.lag as f64),
        ])
    }
}

#[cfg(test)]
mod tests;
