//! Lines matching a pattern in a log file. See ADR-0024 and, for Windows,
//! encodings and saved positions, ADR-0085.
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
//! Parameters: `path` (required, `/var/log/app.log` or `C:\Logs\app.log`),
//! `pattern` (required, a regular expression of the `regex` crate, which
//! runs in linear time) and `exclude` (optional, lines matching it are not
//! counted).
//!
//! The first collection starts at the end of the file, or where the sensor
//! stopped if [`Local::log_positions`] holds a recent position: history is
//! not counted. A file created later is read from its start. Rotation by
//! rename is followed: the old file is read to its end first, and once
//! more at the next collection for a writer that finishes it late.
//! Truncation (copytruncate) is followed too. UTF-8 and, with their byte
//! order mark, UTF-16 files are read. Only the first [`MAX_LINE`]
//! characters of a line are matched.

use std::borrow::Cow;
use std::fs::File;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use regex::bytes::Regex;
use tokio::time::{timeout_at, Instant};

pub use crate::pattern::{pattern, MAX_PATTERN};
use crate::registry::{BuildError, Local, Params};
use crate::{validate, Sample, Sensor, SensorError};

mod position;
#[cfg(unix)]
mod unix;
#[cfg(unix)]
use unix as sys;
#[cfg(windows)]
mod windows;
#[cfg(windows)]
use windows as sys;

/// Default [`Local::log_dirs`]. Windows has no common folder of text
/// logs, so the administrator names them.
pub const DEFAULT_DIRS: &[&str] = if cfg!(windows) { &[] } else { &["/var/log"] };
/// Most bytes read per collection; the rest waits for the next one.
pub const MAX_READ: u64 = 1024 * 1024;
/// Characters (bytes in UTF-8) of a line that are matched; the rest of a
/// longer line is skipped.
pub const MAX_LINE: usize = 4096;
const CHUNK: usize = 64 * 1024;
/// Bytes at the start of a file that tell it from another file with the
/// same ID, or from itself rewritten (copytruncate).
const HEAD: u64 = 256;
/// Time kept before the deadline, so a slow read is reported as such.
const MARGIN: Duration = Duration::from_millis(200);

/// Which file, whatever its name: device and inode on Unix, volume serial
/// number and file index on Windows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct FileId {
    volume: u64,
    index: u128,
}

/// A hash of the first bytes of a file.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Head {
    len: u64,
    hash: u64,
}

/// `/var/log/app.log`: absolute, without `.` or `..` segments.
fn unix_path(path: &str) -> bool {
    path.starts_with('/')
        && path
            .split('/')
            .skip(1)
            .all(|s| !s.is_empty() && s != "." && s != "..")
}

/// `C:\Logs\app.log`: a drive letter, backslashes, no UNC or device path,
/// no stream (`:`), wildcard or name Windows would change or treat as a
/// device (`CON`, `x.` or `x `).
fn windows_path(path: &str) -> bool {
    let bytes = path.as_bytes();
    if bytes.len() < 4 || !bytes[0].is_ascii_alphabetic() || &bytes[1..3] != b":\\" {
        return false;
    }
    path[3..].split('\\').all(|s| {
        let stem = s.split('.').next().unwrap_or("").trim_end();
        let device = matches!(
            stem.to_ascii_uppercase().as_str(),
            "CON" | "PRN" | "AUX" | "NUL" | "CONIN$" | "CONOUT$"
        ) || {
            // Bytes, not `str` slices: the path comes from the server, and
            // byte 3 of `abé` is inside a character.
            let stem = stem.as_bytes();
            stem.len() == 4
                && [b"COM", b"LPT"]
                    .iter()
                    .any(|d| stem[..3].eq_ignore_ascii_case(*d))
                && stem[3].is_ascii_digit()
        };
        !s.is_empty()
            && !s.ends_with(['.', ' '])
            && !s.contains(['<', '>', ':', '"', '/', '|', '?', '*'])
            && !device
    })
}

/// A full path of either system, short enough to be a label value. The
/// server checks rows for every agent, so both are accepted everywhere;
/// the agent refuses the other system's.
pub fn log_path(path: &str) -> Result<PathBuf, String> {
    let ok = validate::instance(path).is_ok() && (unix_path(path) || windows_path(path));
    ok.then(|| PathBuf::from(path)).ok_or_else(|| {
        format!(
            "`{}` must be a full path like /var/log/app.log or C:\\Logs\\app.log, without `.` or `..`, at most {} bytes",
            path.chars().take(64).collect::<String>().escape_debug(),
            validate::MAX_VALUE_LEN
        )
    })
}

/// Whether `path` is `dir` or inside it; without case on Windows, as its
/// file systems compare names.
fn under(path: &Path, dir: &Path) -> bool {
    if cfg!(windows) {
        let fold = |p: &Path| -> Vec<String> {
            p.components()
                .map(|c| c.as_os_str().to_string_lossy().to_lowercase())
                .collect()
        };
        let (path, dir) = (fold(path), fold(dir));
        path.len() >= dir.len() && path[..dir.len()] == dir[..]
    } else {
        path.starts_with(dir)
    }
}

/// Whether the resolved `real` lies under one of `dirs`, resolved too.
fn allowed(real: &Path, dirs: &[PathBuf]) -> bool {
    dirs.iter()
        .filter_map(|d| std::fs::canonicalize(d).ok())
        .any(|d| under(real, &d))
}

fn denied(path: &Path) -> String {
    format!(
        "{}: access denied; give {} read access to it",
        path.display(),
        sys::ACCOUNT
    )
}

/// `path` resolved, if it lies under one of `dirs` once both are resolved.
/// `Ok(None)` when the file does not exist.
pub fn resolve(path: &Path, dirs: &[PathBuf]) -> Result<Option<PathBuf>, String> {
    let real = match std::fs::canonicalize(path) {
        Ok(real) => real,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(e) if e.kind() == io::ErrorKind::PermissionDenied => return Err(denied(path)),
        Err(e) => return Err(format!("{}: {e}", path.display())),
    };
    if !allowed(&real, dirs) {
        return Err(format!(
            "{} resolves to {}, outside the agent's log folders",
            path.display(),
            real.display()
        ));
    }
    Ok(Some(real))
}

/// How a file's lines are encoded, from its byte order mark.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Encoding {
    /// UTF-8, or any other encoding that writes `\n` as one byte (ASCII,
    /// Windows code pages): patterns match their bytes.
    Utf8,
    Utf16Le,
    Utf16Be,
}

impl Encoding {
    const UTF8_BOM: &[u8] = b"\xEF\xBB\xBF";
    const UTF16LE_BOM: &[u8] = b"\xFF\xFE";
    const UTF16BE_BOM: &[u8] = b"\xFE\xFF";

    /// The encoding the first bytes of a file announce and the length of
    /// its mark, `None` while they could still be the start of one.
    fn detect(start: &[u8]) -> Option<(Self, u64)> {
        let marks = [
            (Self::UTF8_BOM, Self::Utf8),
            (Self::UTF16LE_BOM, Self::Utf16Le),
            (Self::UTF16BE_BOM, Self::Utf16Be),
        ];
        for (mark, encoding) in marks {
            if start.starts_with(mark) {
                return Some((encoding, mark.len() as u64));
            }
            if mark.starts_with(start) && start.len() < mark.len() {
                return None;
            }
        }
        Some((Self::Utf8, 0))
    }

    /// Bytes per code unit.
    fn unit(self) -> usize {
        match self {
            Self::Utf8 => 1,
            Self::Utf16Le | Self::Utf16Be => 2,
        }
    }

    fn newline(self) -> &'static [u8] {
        match self {
            Self::Utf8 => b"\n",
            Self::Utf16Le => b"\n\0",
            Self::Utf16Be => b"\0\n",
        }
    }

    /// A line as UTF-8 for matching; invalid UTF-16 becomes U+FFFD.
    fn decode(self, line: &[u8]) -> Cow<'_, [u8]> {
        let (units, _) = line.as_chunks::<2>();
        let units: Vec<u16> = match self {
            Self::Utf8 => return Cow::Borrowed(line),
            Self::Utf16Le => units.iter().map(|u| u16::from_le_bytes(*u)).collect(),
            Self::Utf16Be => units.iter().map(|u| u16::from_be_bytes(*u)).collect(),
        };
        let text: String = char::decode_utf16(units)
            .map(|c| c.unwrap_or(char::REPLACEMENT_CHARACTER))
            .collect();
        Cow::Owned(text.into_bytes())
    }
}

/// A file being read.
struct Tracked {
    file: File,
    id: FileId,
    offset: u64,
    /// Where the line being read starts: what a restart resumes from.
    line_start: u64,
    /// Start of that line, cut at [`MAX_LINE`] characters.
    partial: Vec<u8>,
    /// `None` until the file has its first bytes.
    encoding: Option<Encoding>,
    head: Option<Head>,
}

impl Tracked {
    fn new(file: File, id: FileId, offset: u64) -> Self {
        Self {
            file,
            id,
            offset,
            line_start: offset,
            partial: Vec::new(),
            encoding: None,
            head: None,
        }
    }

    /// Read again from the start: the file was truncated or rewritten.
    fn restart(&mut self) {
        self.offset = 0;
        self.line_start = 0;
        self.partial.clear();
        self.encoding = None;
        self.head = None;
    }
}

/// `len` bytes from `offset`, fewer at the end of the file.
fn read_exact_at(file: &File, len: u64, offset: u64) -> io::Result<Vec<u8>> {
    let mut buf = vec![0; usize::try_from(len).unwrap_or(usize::MAX)];
    let mut filled = 0;
    while filled < buf.len() {
        match sys::read_at(file, &mut buf[filled..], offset + filled as u64)? {
            0 => break,
            n => filled += n,
        }
    }
    buf.truncate(filled);
    Ok(buf)
}

/// The hash of the first `len` bytes of `file`, `None` if it is shorter.
fn head(file: &File, len: u64) -> io::Result<Option<Head>> {
    let start = read_exact_at(file, len, 0)?;
    Ok((len > 0 && start.len() as u64 == len).then(|| Head {
        len,
        hash: position::fnv(&start),
    }))
}

#[derive(Default)]
struct State {
    /// The file the path names, or named last.
    current: Option<Tracked>,
    /// The file before the last rotation, read once more in case its
    /// writer finished it after the rename.
    previous: Option<Tracked>,
    /// A first collection ran; files found from now on are new.
    primed: bool,
    saved: position::Saved,
}

/// What one collection found.
#[derive(Default)]
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
    /// Where the position is saved, if anywhere.
    position: Option<PathBuf>,
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
        let Some(text) = params.get("path") else {
            return Err(params.bad("path", "is required"));
        };
        let path = log_path(text).map_err(|reason| params.bad("path", reason))?;
        let Some(pattern_text) = params.get("pattern") else {
            return Err(params.bad("pattern", "is required"));
        };
        let pattern = pattern(pattern_text).map_err(|reason| params.bad("pattern", reason))?;
        let exclude_text = params.get("exclude");
        let exclude = match exclude_text {
            None => None,
            Some(text) => Some(self::pattern(text).map_err(|r| params.bad("exclude", r))?),
        };
        let dirs = local.map_or_else(
            || DEFAULT_DIRS.iter().map(PathBuf::from).collect(),
            |l| l.log_dirs.clone(),
        );
        if local.is_some() {
            let (native, other) = if cfg!(windows) {
                (windows_path(text), "a Linux")
            } else {
                (unix_path(text), "a Windows")
            };
            if !native {
                return Err(params.refused(format!("{text} is {other} path")));
            }
            if dirs.is_empty() {
                return Err(
                    params.refused("this agent reads no log files: its sensors.log_dirs is empty")
                );
            }
            if !dirs.iter().any(|d| under(&path, d)) {
                return Err(params.refused(format!(
                    "{} is not in the agent's log folders (sensors.log_dirs)",
                    path.display()
                )));
            }
            // A stat or two of a local path.
            resolve(&path, &dirs).map_err(|reason| params.refused(reason))?;
        }
        let position = local
            .and_then(|l| l.log_positions.as_deref())
            .map(|dir| position::file(dir, text, pattern_text, exclude_text));
        Ok(Self(Arc::new(Reader {
            path,
            pattern,
            exclude,
            dirs,
            position,
            state: Mutex::default(),
        })))
    }
}

impl Reader {
    fn matches(&self, encoding: Encoding, line: &[u8]) -> bool {
        let line = encoding.decode(line);
        let line = line.strip_suffix(b"\r").unwrap_or(&line[..]);
        self.pattern.is_match(line) && !self.exclude.as_ref().is_some_and(|e| e.is_match(line))
    }

    /// How an I/O error is reported: access denied is permanent, as it
    /// takes the administrator to fix it.
    fn failure(&self, e: io::Error) -> SensorError {
        if e.kind() == io::ErrorKind::PermissionDenied {
            return SensorError::Permanent(denied(&self.path));
        }
        #[cfg(windows)]
        if e.raw_os_error() == Some(sys::SHARING_VIOLATION) {
            return SensorError::Transient(format!(
                "{} is locked by the program writing it, which lets nobody else read it",
                self.path.display()
            ));
        }
        SensorError::Transient(format!("{}: {e}", self.path.display()))
    }

    /// One collection, on the blocking pool, then the position saved.
    fn collect(&self, state: &mut State) -> Result<Round, SensorError> {
        let round = self.read(state);
        if let (Some(file), Some(t)) = (&self.position, &state.current) {
            let position = position::Position {
                id: t.id,
                offset: t.line_start,
                head: t.head,
            };
            state.saved.update(file, position);
        }
        round
    }

    fn read(&self, state: &mut State) -> Result<Round, SensorError> {
        let first = !std::mem::replace(&mut state.primed, true);
        let resume = if first {
            self.position.as_deref().and_then(position::load)
        } else {
            None
        };
        let real = resolve(&self.path, &self.dirs).map_err(SensorError::Permanent)?;
        let fail = |e| self.failure(e);
        let mut round = Round::default();
        let mut budget = MAX_READ;

        // The file renamed away at the last rotation: kept while its
        // writer still adds to it, then dropped.
        if let Some(mut old) = state.previous.take() {
            let read = self
                .drain(&mut old, &mut budget, &mut round)
                .map_err(fail)?;
            let lag = old
                .file
                .metadata()
                .map_err(fail)?
                .len()
                .saturating_sub(old.offset);
            round.lag += lag;
            if lag > 0 || (read > 0 && !sys::deleted(&old.file).map_err(fail)?) {
                state.previous = Some(old);
            } else {
                self.finish(old, &mut round);
            }
        }
        // Then the file we hold, even if it was renamed away.
        let mut lag = 0;
        if let Some(tracked) = &mut state.current {
            let size = tracked.file.metadata().map_err(fail)?.len();
            if self.replaced(tracked, size).map_err(fail)? {
                tracked.restart();
            }
            self.drain(tracked, &mut budget, &mut round).map_err(fail)?;
            lag = size.saturating_sub(tracked.offset);
            round.lag += lag;
        }
        let Some(real) = real else {
            let Some(tracked) = state.current.take() else {
                if state.previous.is_some() {
                    return Ok(round);
                }
                return Err(SensorError::Transient(format!(
                    "{} does not exist",
                    self.path.display()
                )));
            };
            // Renamed away and not created again yet: keep reading it,
            // unless it was deleted.
            if lag == 0 && sys::deleted(&tracked.file).map_err(fail)? {
                self.finish(tracked, &mut round);
            } else {
                state.current = Some(tracked);
            }
            return Ok(round);
        };
        let id = sys::path_id(&real).map_err(fail)?;
        if state.current.as_ref().is_some_and(|t| t.id == id) || lag > 0 {
            // Still on this file, or the old one is not read to its end.
            return Ok(round);
        }
        // A new file at the path: the one we held becomes the previous.
        if let Some(old) = state.current.take() {
            if let Some(older) = state.previous.replace(old) {
                self.finish(older, &mut round);
            }
        }
        let file = sys::open(&real).map_err(fail)?;
        if let Some(opened) = sys::final_path(&file).map_err(fail)? {
            if !allowed(&opened, &self.dirs) {
                return Err(SensorError::Permanent(format!(
                    "{} was replaced by a link to {}, outside the agent's log folders",
                    self.path.display(),
                    opened.display()
                )));
            }
        }
        let meta = file.metadata().map_err(fail)?;
        if !meta.is_file() {
            return Err(SensorError::Permanent(format!(
                "{} is not a regular file",
                real.display()
            )));
        }
        let id = sys::id(&file).map_err(fail)?;
        // A new file: the first one at its end or where the sensor
        // stopped, a later one from its start.
        let start = match (first, resume) {
            (false, _) => 0,
            (true, None) => meta.len(),
            (true, Some(saved)) => {
                let same = saved.id == id
                    && saved.offset <= meta.len()
                    && match saved.head {
                        Some(h) => head(&file, h.len).map_err(fail)? == Some(h),
                        None => true,
                    };
                // Otherwise rotated or rewritten while the agent was
                // stopped: all of it is new.
                if same {
                    saved.offset
                } else {
                    0
                }
            }
        };
        let mut tracked = Tracked::new(file, id, start);
        self.replaced(&mut tracked, meta.len()).map_err(fail)?;
        self.drain(&mut tracked, &mut budget, &mut round)
            .map_err(fail)?;
        round.lag += meta.len().saturating_sub(tracked.offset);
        state.current = Some(tracked);
        Ok(round)
    }

    /// Whether `tracked` was truncated or rewritten since it was last read
    /// (copytruncate); records the hash of its first bytes otherwise.
    fn replaced(&self, tracked: &mut Tracked, size: u64) -> io::Result<bool> {
        if size < tracked.offset {
            return Ok(true);
        }
        if let Some(known) = tracked.head {
            if head(&tracked.file, known.len)? != Some(known) {
                return Ok(true);
            }
        }
        if tracked.head.is_none_or(|h| h.len < HEAD && size > h.len) {
            tracked.head = head(&tracked.file, size.min(HEAD))?;
        }
        Ok(false)
    }

    /// Counts the last line of a file left for good, if it had no newline.
    fn finish(&self, tracked: Tracked, round: &mut Round) {
        if let (Some(encoding), false) = (tracked.encoding, tracked.partial.is_empty()) {
            round.matches += u64::from(self.matches(encoding, &tracked.partial));
        }
    }

    /// Reads `tracked` from its offset to its end or until `budget` is
    /// spent, counting complete matching lines. Returns the bytes read.
    fn drain(&self, tracked: &mut Tracked, budget: &mut u64, round: &mut Round) -> io::Result<u64> {
        let encoding = match tracked.encoding {
            Some(encoding) => encoding,
            None => {
                let start = read_exact_at(&tracked.file, 3, 0)?;
                let Some((encoding, mark)) = Encoding::detect(&start) else {
                    // Empty, or only part of a mark written yet.
                    return Ok(0);
                };
                tracked.encoding = Some(encoding);
                // Past the mark, on a code unit boundary.
                let unit = encoding.unit() as u64;
                let skew = tracked.offset.max(mark).saturating_sub(mark) % unit;
                tracked.offset = tracked.offset.max(mark) + (unit - skew) % unit;
                tracked.line_start = tracked.offset;
                encoding
            }
        };
        let unit = encoding.unit();
        let newline = encoding.newline();
        let max = MAX_LINE * unit;
        let mut total = 0;
        let mut buf = vec![0; CHUNK];
        while *budget > 0 {
            let want = buf
                .len()
                .min(usize::try_from(*budget).unwrap_or(usize::MAX));
            let read = sys::read_at(&tracked.file, &mut buf[..want], tracked.offset)?;
            // A code unit written in part waits for the rest.
            let whole = read - read % unit;
            if whole == 0 {
                break;
            }
            let base = tracked.offset;
            tracked.offset += whole as u64;
            *budget = budget.saturating_sub(read as u64);
            total += whole as u64;
            let mut line = 0;
            for (i, code) in buf[..whole].chunks_exact(unit).enumerate() {
                if code != newline {
                    continue;
                }
                let end = i * unit;
                let room = max - tracked.partial.len();
                let body = &buf[line..end];
                tracked
                    .partial
                    .extend_from_slice(&body[..room.min(body.len())]);
                round.matches += u64::from(self.matches(encoding, &tracked.partial));
                tracked.partial.clear();
                line = end + unit;
                tracked.line_start = base + line as u64;
            }
            let room = max - tracked.partial.len();
            let rest = &buf[line..whole];
            tracked
                .partial
                .extend_from_slice(&rest[..room.min(rest.len())]);
        }
        Ok(total)
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
            reader.collect(&mut state)
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
