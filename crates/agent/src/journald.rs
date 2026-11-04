//! Forwards the system journal's warnings and errors upstream (ADR-0091).
//!
//! `journalctl --output=json --follow --priority=warning` runs as a child
//! process; each line becomes a [`LogEntry`] in its own [`Uplink`], which
//! the stream sends in `LogBatch` envelopes. Reading the journal API
//! directly would need libsystemd, which the agent does not link.
//!
//! - **Where it resumes.** The cursor of the last line handled is saved
//!   in `<data_dir>/journal.cursor` every few seconds and at shutdown; a
//!   restart reads on from it, so lines written while the agent was
//!   stopped are forwarded. Without a cursor, or with one saved more than
//!   [`CURSOR_MAX_AGE`] ago, it starts at the end, like the `log` sensor
//!   (ADR-0085). Lines queued but not sent yet when the agent stops are
//!   lost.
//! - **Back-pressure.** When the queue is full (the upstream is down), the
//!   forwarder stops reading: `journalctl` blocks and the journal keeps the
//!   lines, which are read once there is room again.
//! - **Rate limit.** [`rate_limit`] lets [`BURST`] lines through, then
//!   [`RATE_PER_SECOND`], measured on the lines' own timestamps, so a
//!   backlog written slowly passes whole while a flood is cut. Dropped
//!   lines are counted with [`Uplink::skip`] and reported in the batch.
//!   Reading is also paced to [`READ_PER_SECOND`] lines, waiting rather
//!   than dropping, so a backlog reaches the server under its own limit.
//!
//! Messages are cut to [`MAX_MESSAGE`] bytes. Nothing else is filtered
//! or redacted here; the server redacts secrets it recognises.

use std::io;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use monitorflow_common::proto::v1::LogEntry;
use monitorflow_common::rate::TokenBucket;
use serde_json::Value;
use tokio::io::{AsyncBufRead, AsyncBufReadExt, BufReader};
use tokio::process::Command;
use tokio::time::{sleep, Instant};

use crate::shutdown::Shutdown;
use crate::upload::{Uplink, UploadSettings};

/// Least important priority forwarded: 4, warning.
pub const PRIORITY: &str = "warning";
/// Lines let through at once before the rate applies.
pub const BURST: f64 = 1_000.0;
/// Lines per second let through after the burst.
pub const RATE_PER_SECOND: f64 = 10.0;
/// Lines read per second at most, by the clock; the server drops lines
/// above 500 a second per agent.
pub const READ_PER_SECOND: f64 = 200.0;
/// Bytes of a message kept; the rest is cut at a character boundary.
pub const MAX_MESSAGE: usize = 4096;
/// Bytes of one JSON line of `journalctl`; a longer line is skipped.
const MAX_LINE: usize = 1024 * 1024;
/// File of the saved cursor, in the data directory.
pub const CURSOR_FILE: &str = "journal.cursor";
/// A cursor saved longer ago is ignored: the agent starts at the end.
pub const CURSOR_MAX_AGE: Duration = Duration::from_secs(24 * 60 * 60);
/// Longest cursor accepted from the file.
const MAX_CURSOR: usize = 512;
/// How often a changed cursor is saved.
const SAVE_EVERY: Duration = Duration::from_secs(5);
/// Wait before running `journalctl` again after it exited.
const RESTART_AFTER: Duration = Duration::from_secs(30);
/// How often a full queue is checked for room.
const ROOM_POLL: Duration = Duration::from_millis(500);
/// Wait when reading is ahead of [`READ_PER_SECOND`].
const PACE_WAIT: Duration = Duration::from_millis(50);
/// What the source field says.
pub const SOURCE: &str = "journal";

/// Queue settings: a full queue pauses reading, so it can stay small.
pub fn upload_settings() -> UploadSettings {
    UploadSettings {
        batch_size: 500,
        flush_interval: Duration::from_secs(10),
        // At most 8 MiB of messages.
        capacity: 2_000,
    }
}

/// The forwarder's limit: [`BURST`] lines, then [`RATE_PER_SECOND`], over
/// the lines' timestamps.
pub fn rate_limit() -> TokenBucket {
    TokenBucket::new(RATE_PER_SECOND, BURST)
}

/// The saved position in the journal.
#[derive(Debug, Clone)]
pub struct Cursor {
    path: PathBuf,
}

impl Cursor {
    pub fn in_dir(data_dir: &Path) -> Self {
        Self {
            path: data_dir.join(CURSOR_FILE),
        }
    }

    /// The saved cursor, unless missing, unreadable, malformed or saved
    /// more than [`CURSOR_MAX_AGE`] before `now`.
    pub async fn load(&self, now: SystemTime) -> Option<String> {
        let modified = tokio::fs::metadata(&self.path)
            .await
            .ok()?
            .modified()
            .ok()?;
        if now.duration_since(modified).unwrap_or_default() > CURSOR_MAX_AGE {
            return None;
        }
        let text = tokio::fs::read_to_string(&self.path).await.ok()?;
        let cursor = text.trim();
        let valid = !cursor.is_empty()
            && cursor.len() <= MAX_CURSOR
            && cursor.bytes().all(|b| b.is_ascii_graphic());
        valid.then(|| cursor.to_owned())
    }

    /// Writes through a temporary file and a rename.
    pub async fn save(&self, cursor: &str) -> io::Result<()> {
        let tmp = self.path.with_extension("tmp");
        tokio::fs::write(&tmp, format!("{cursor}\n")).await?;
        tokio::fs::rename(&tmp, &self.path).await
    }

    pub async fn remove(&self) {
        let _ = tokio::fs::remove_file(&self.path).await;
    }
}

/// The arguments of `journalctl`, from `cursor` or from the end.
pub fn args(cursor: Option<&str>) -> Vec<String> {
    let mut args = vec![
        "--output=json".to_owned(),
        "--follow".to_owned(),
        "--all".to_owned(),
        "--no-pager".to_owned(),
        format!("--priority={PRIORITY}"),
    ];
    match cursor {
        Some(cursor) => args.push(format!("--after-cursor={cursor}")),
        None => args.push("--lines=0".to_owned()),
    }
    args
}

/// One `journalctl` JSON line: its cursor and the entry. `None` without a
/// cursor or a timestamp.
pub fn parse(line: &[u8]) -> Option<(String, LogEntry)> {
    let object: serde_json::Map<String, Value> = serde_json::from_slice(line).ok()?;
    let text = |name: &str| match object.get(name) {
        Some(Value::String(s)) => s.clone(),
        // Binary or not UTF-8: an array of bytes.
        Some(Value::Array(bytes)) => {
            let bytes: Vec<u8> = bytes
                .iter()
                .filter_map(|b| b.as_u64().and_then(|b| u8::try_from(b).ok()))
                .collect();
            String::from_utf8_lossy(&bytes).into_owned()
        }
        _ => String::new(),
    };
    let number = |name: &str| text(name).parse::<u64>().ok();
    let cursor = text("__CURSOR");
    let micros = number("__REALTIME_TIMESTAMP")?;
    if cursor.is_empty() {
        return None;
    }
    let mut identifier = text("SYSLOG_IDENTIFIER");
    if identifier.is_empty() {
        identifier = text("_COMM");
    }
    let entry = LogEntry {
        timestamp_unix_ms: i64::try_from(micros / 1000).ok()?,
        message: truncate(text("MESSAGE"), MAX_MESSAGE),
        // Lines without one are logged at the default, info; but this
        // reads only up to warning, so the filter matched a priority.
        priority: number("PRIORITY").map_or(6, |p| p.min(7) as u32),
        unit: text("_SYSTEMD_UNIT"),
        identifier,
        boot_id: text("_BOOT_ID"),
        pid: number("_PID")
            .and_then(|p| u32::try_from(p).ok())
            .unwrap_or(0),
        source: SOURCE.to_owned(),
    };
    Some((cursor, entry))
}

/// `text` cut to at most `max` bytes at a character boundary.
pub fn truncate(mut text: String, max: usize) -> String {
    if text.len() > max {
        let mut end = max;
        while !text.is_char_boundary(end) {
            end -= 1;
        }
        text.truncate(end);
    }
    text
}

/// Reads one line into `buf`, without its newline. `Ok(None)` at the end;
/// `Ok(Some(false))` when the line was longer than `max` and skipped.
async fn read_line(
    reader: &mut (impl AsyncBufRead + Unpin),
    buf: &mut Vec<u8>,
    max: usize,
) -> io::Result<Option<bool>> {
    buf.clear();
    let mut fits = true;
    loop {
        let available = reader.fill_buf().await?;
        if available.is_empty() {
            return Ok((!buf.is_empty() || !fits).then_some(fits));
        }
        let (chunk, done) = match available.iter().position(|&b| b == b'\n') {
            Some(i) => (&available[..i], Some(i + 1)),
            None => (available, None),
        };
        if fits && buf.len() + chunk.len() <= max {
            buf.extend_from_slice(chunk);
        } else {
            fits = false;
            buf.clear();
        }
        let used = done.unwrap_or(available.len());
        reader.consume(used);
        if done.is_some() {
            return Ok(Some(fits));
        }
    }
}

/// Whether the agent can read the system's journal files, not only its
/// own: `None` when there is no journal directory at all.
pub fn system_journal_readable(roots: &[&Path]) -> Option<bool> {
    let mut found = false;
    for root in roots {
        let Ok(machines) = std::fs::read_dir(root) else {
            continue;
        };
        found = true;
        for machine in machines.flatten() {
            let Ok(files) = std::fs::read_dir(machine.path()) else {
                continue;
            };
            for file in files.flatten() {
                let name = file.file_name();
                let name = name.to_string_lossy();
                if name.starts_with("system")
                    && name.ends_with(".journal")
                    && std::fs::File::open(file.path()).is_ok()
                {
                    return Some(true);
                }
            }
        }
    }
    found.then_some(false)
}

/// The forwarder's notice when it cannot read the system journal.
pub fn no_access_notice(now_ms: i64) -> LogEntry {
    LogEntry {
        timestamp_unix_ms: now_ms,
        message: "The MonitorFlow agent cannot read the system journal, only its own lines. \
                  Add its user to the systemd-journal group (usermod -aG systemd-journal monitorflow) \
                  and restart it, or run the installer again."
            .to_owned(),
        priority: 4,
        unit: "monitorflow-agent.service".to_owned(),
        identifier: "monitorflow-agent".to_owned(),
        boot_id: String::new(),
        pid: std::process::id(),
        source: SOURCE.to_owned(),
    }
}

/// Counters since the forwarder started.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Stats {
    pub forwarded: u64,
    pub rate_limited: u64,
    /// Lines that were not JSON with a cursor, or too long.
    pub unreadable: u64,
}

/// Runs `journalctl` and queues its lines.
pub struct Forwarder {
    program: PathBuf,
    /// Arguments before [`args`]; tests run a script through `sh`.
    prefix: Vec<String>,
    cursor: Cursor,
    uplink: Uplink<LogEntry>,
    limit: TokenBucket,
    /// [`READ_PER_SECOND`], over the time since the forwarder started.
    pace: TokenBucket,
    started: Instant,
    stats: Stats,
}

impl Forwarder {
    pub fn new(data_dir: &Path, uplink: Uplink<LogEntry>) -> Self {
        Self::with_program("journalctl", Vec::new(), data_dir, uplink)
    }

    pub fn with_program(
        program: impl Into<PathBuf>,
        prefix: Vec<String>,
        data_dir: &Path,
        uplink: Uplink<LogEntry>,
    ) -> Self {
        Self {
            program: program.into(),
            prefix,
            cursor: Cursor::in_dir(data_dir),
            uplink,
            limit: rate_limit(),
            pace: TokenBucket::new(READ_PER_SECOND, READ_PER_SECOND),
            started: Instant::now(),
            stats: Stats::default(),
        }
    }

    pub fn with_limit(mut self, limit: TokenBucket) -> Self {
        self.limit = limit;
        self
    }

    pub fn stats(&self) -> Stats {
        self.stats
    }

    /// Forwards until `stop` fires, running `journalctl` again
    /// [`RESTART_AFTER`] it exits. Returns at once when it is missing.
    pub async fn run(mut self, mut stop: Shutdown) -> Stats {
        loop {
            let started = Instant::now();
            match self.follow(&mut stop).await {
                Ok(true) => break,
                Ok(false) => {}
                Err(error) if error.kind() == io::ErrorKind::NotFound => {
                    tracing::info!("journalctl is not installed: system logs are not forwarded");
                    break;
                }
                Err(error) => tracing::warn!(%error, "cannot read the journal"),
            }
            if started.elapsed() < RESTART_AFTER {
                tokio::select! {
                    () = stop.wait() => break,
                    () = sleep(RESTART_AFTER) => {}
                }
            }
        }
        self.stats
    }

    /// One run of `journalctl`; `Ok(true)` when `stop` fired.
    pub(crate) async fn follow(&mut self, stop: &mut Shutdown) -> io::Result<bool> {
        let saved = self.cursor.load(SystemTime::now()).await;
        let mut child = Command::new(&self.program)
            .args(&self.prefix)
            .args(args(saved.as_deref()))
            .env("LC_ALL", "C")
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .kill_on_drop(true)
            .spawn()?;
        let stdout = child.stdout.take().expect("piped");
        let mut reader = BufReader::with_capacity(64 * 1024, stdout);
        let mut line = Vec::new();
        let mut last: Option<String> = None;
        let mut save_at = Instant::now() + SAVE_EVERY;
        let mut read_any = false;
        let stopped = loop {
            if !self.uplink.has_room() {
                tokio::select! {
                    () = stop.wait() => break true,
                    () = sleep(ROOM_POLL) => continue,
                }
            }
            let elapsed_ms = i64::try_from(self.started.elapsed().as_millis()).unwrap_or(i64::MAX);
            if !self.pace.allow(elapsed_ms) {
                tokio::select! {
                    () = stop.wait() => break true,
                    () = sleep(PACE_WAIT) => continue,
                }
            }
            let next = tokio::select! {
                () = stop.wait() => break true,
                next = read_line(&mut reader, &mut line, MAX_LINE) => next?,
            };
            match next {
                None => break false,
                Some(false) => self.stats.unreadable += 1,
                Some(true) => {
                    read_any = true;
                    match parse(&line) {
                        Some((cursor, entry)) => {
                            if self.limit.allow(entry.timestamp_unix_ms) {
                                self.uplink.push(entry);
                                self.stats.forwarded += 1;
                            } else {
                                self.uplink.skip(1);
                                self.stats.rate_limited += 1;
                            }
                            last = Some(cursor);
                        }
                        None => self.stats.unreadable += 1,
                    }
                }
            }
            if Instant::now() >= save_at {
                save_at = Instant::now() + SAVE_EVERY;
                if let Some(cursor) = last.take() {
                    self.save(&cursor).await;
                }
            }
        };
        if let Some(cursor) = last {
            self.save(&cursor).await;
        }
        let _ = child.start_kill();
        let status = child.wait().await?;
        if !stopped && saved.is_some() && !read_any && !status.success() {
            // A cursor of a journal that no longer exists: start at the end.
            tracing::warn!(%status, "journalctl refused the saved cursor; starting at the end of the journal");
            self.cursor.remove().await;
        } else if !stopped {
            tracing::warn!(%status, "journalctl exited");
        }
        Ok(stopped)
    }

    async fn save(&self, cursor: &str) {
        if let Err(error) = self.cursor.save(cursor).await {
            tracing::warn!(%error, "cannot save the journal cursor");
        }
    }
}

/// Starts forwarding unless this host has no journal; queues a notice
/// when the agent cannot read the system's.
pub fn start(data_dir: &Path, uplink: Uplink<LogEntry>, stop: Shutdown) {
    let roots = [Path::new("/var/log/journal"), Path::new("/run/log/journal")];
    let readable = if rustix::process::geteuid().is_root() {
        Some(true)
    } else {
        system_journal_readable(&roots)
    };
    match readable {
        None => {
            tracing::info!("no systemd journal on this host: system logs are not forwarded");
            return;
        }
        Some(false) => {
            tracing::warn!(
                "cannot read the system journal: add the agent's user to the systemd-journal group"
            );
            let now = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map_or(0, |d| d.as_millis() as i64);
            uplink.push(no_access_notice(now));
        }
        Some(true) => {}
    }
    let forwarder = Forwarder::new(data_dir, uplink);
    tokio::spawn(async move {
        let stats = forwarder.run(stop).await;
        tracing::info!(
            forwarded = stats.forwarded,
            rate_limited = stats.rate_limited,
            unreadable = stats.unreadable,
            "journal forwarding stopped"
        );
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn journal_lines_become_entries() {
        let line = br#"{"__CURSOR":"s=abc;i=1","__REALTIME_TIMESTAMP":"1800000000123456","PRIORITY":"3","MESSAGE":"disk full","_SYSTEMD_UNIT":"nginx.service","SYSLOG_IDENTIFIER":"nginx","_BOOT_ID":"0123456789abcdef0123456789abcdef","_PID":"42"}"#;
        let (cursor, entry) = parse(line).unwrap();
        assert_eq!(cursor, "s=abc;i=1");
        assert_eq!(
            entry,
            LogEntry {
                timestamp_unix_ms: 1_800_000_000_123,
                message: "disk full".into(),
                priority: 3,
                unit: "nginx.service".into(),
                identifier: "nginx".into(),
                boot_id: "0123456789abcdef0123456789abcdef".into(),
                pid: 42,
                source: "journal".into(),
            }
        );
    }

    #[test]
    fn binary_messages_missing_fields_and_long_messages() {
        let line = br#"{"__CURSOR":"c","__REALTIME_TIMESTAMP":"1000","MESSAGE":[104,105,255],"_COMM":"kworker"}"#;
        let (_, entry) = parse(line).unwrap();
        assert_eq!(entry.message, "hi\u{fffd}");
        assert_eq!(
            (entry.identifier.as_str(), entry.unit.as_str()),
            ("kworker", "")
        );
        assert_eq!(
            (entry.priority, entry.pid, entry.timestamp_unix_ms),
            (6, 0, 1)
        );
        assert!(parse(br#"{"__REALTIME_TIMESTAMP":"1000"}"#).is_none());
        assert!(parse(br#"{"__CURSOR":"c"}"#).is_none());
        assert!(parse(b"not json").is_none());
        let long = format!("{}é", "a".repeat(MAX_MESSAGE - 1));
        assert_eq!(truncate(long, MAX_MESSAGE).len(), MAX_MESSAGE - 1);
    }

    #[test]
    fn arguments_resume_from_the_cursor_or_start_at_the_end() {
        let from_end = args(None);
        assert!(from_end.contains(&"--lines=0".to_owned()));
        assert!(from_end.contains(&"--priority=warning".to_owned()));
        let resumed = args(Some("s=1;i=2"));
        assert!(resumed.contains(&"--after-cursor=s=1;i=2".to_owned()));
        assert!(!resumed.iter().any(|a| a.starts_with("--lines")));
    }

    #[tokio::test]
    async fn long_lines_are_skipped_and_the_next_one_read() {
        let input = format!("short\n{}\nnext\nlast", "x".repeat(100));
        let mut reader = BufReader::with_capacity(8, input.as_bytes());
        let mut buf = Vec::new();
        assert_eq!(
            read_line(&mut reader, &mut buf, 10).await.unwrap(),
            Some(true)
        );
        assert_eq!(buf, b"short");
        assert_eq!(
            read_line(&mut reader, &mut buf, 10).await.unwrap(),
            Some(false)
        );
        assert_eq!(
            read_line(&mut reader, &mut buf, 10).await.unwrap(),
            Some(true)
        );
        assert_eq!(buf, b"next");
        assert_eq!(
            read_line(&mut reader, &mut buf, 10).await.unwrap(),
            Some(true)
        );
        assert_eq!(buf, b"last");
        assert_eq!(read_line(&mut reader, &mut buf, 10).await.unwrap(), None);
    }

    #[tokio::test]
    async fn old_or_malformed_cursors_are_ignored() {
        let dir = tempfile::tempdir().unwrap();
        let cursor = Cursor::in_dir(dir.path());
        assert_eq!(cursor.load(SystemTime::now()).await, None);
        cursor.save("s=1;i=2").await.unwrap();
        assert_eq!(
            cursor.load(SystemTime::now()).await.as_deref(),
            Some("s=1;i=2")
        );
        let later = SystemTime::now() + CURSOR_MAX_AGE + Duration::from_secs(60);
        assert_eq!(cursor.load(later).await, None);
        cursor.save("has space").await.unwrap();
        assert_eq!(cursor.load(SystemTime::now()).await, None);
    }

    /// A `journalctl` stand-in run through `sh`: it records its arguments,
    /// prints two lines from the end, one more after cursor `c2`, and
    /// refuses any other cursor.
    fn fake_journalctl(dir: &Path) -> Vec<String> {
        let line = |cursor: &str, ms: u64| {
            format!(
                r#"{{"__CURSOR":"{cursor}","__REALTIME_TIMESTAMP":"{}","PRIORITY":"3","MESSAGE":"line {cursor}"}}"#,
                ms * 1000
            )
        };
        let script = format!(
            "echo \"$*\" >> '{args}'\n\
             case \"$*\" in\n\
             *--after-cursor=c2*) echo '{c3}' ;;\n\
             *--after-cursor=*) exit 1 ;;\n\
             *) echo '{c1}'; echo 'not json'; echo '{c2}' ;;\n\
             esac\n",
            args = dir.join("args").display(),
            c1 = line("c1", 1_000),
            c2 = line("c2", 2_000),
            c3 = line("c3", 3_000),
        );
        let path = dir.join("journalctl.sh");
        std::fs::write(&path, script).unwrap();
        vec![path.display().to_string()]
    }

    #[tokio::test]
    async fn the_cursor_carries_across_restarts() {
        let dir = tempfile::tempdir().unwrap();
        let prefix = fake_journalctl(dir.path());
        let uplink = Uplink::new(upload_settings());
        let (_trigger, mut stop) = crate::shutdown::channel();
        let forwarder =
            || Forwarder::with_program("/bin/sh", prefix.clone(), dir.path(), uplink.clone());

        // First start: from the end, then the cursor of the last line.
        let mut first = forwarder();
        assert!(!first.follow(&mut stop).await.unwrap());
        assert_eq!(first.stats().forwarded, 2);
        assert_eq!(first.stats().unreadable, 1);
        let messages =
            |batch: Vec<LogEntry>| batch.into_iter().map(|e| e.message).collect::<Vec<_>>();
        assert_eq!(messages(uplink.take()), ["line c1", "line c2"]);
        let saved = std::fs::read_to_string(dir.path().join(CURSOR_FILE)).unwrap();
        assert_eq!(saved.trim(), "c2");

        // A new forwarder, as after a restart, reads on after it.
        let mut second = forwarder();
        assert!(!second.follow(&mut stop).await.unwrap());
        assert_eq!(messages(uplink.take()), ["line c3"]);
        let args = std::fs::read_to_string(dir.path().join("args")).unwrap();
        let args: Vec<_> = args.lines().collect();
        assert!(args[0].contains("--lines=0"), "{args:?}");
        assert!(args[1].contains("--after-cursor=c2"), "{args:?}");

        // A cursor journalctl refuses is dropped: the next start is from the end.
        Cursor::in_dir(dir.path()).save("gone").await.unwrap();
        let mut third = forwarder();
        assert!(!third.follow(&mut stop).await.unwrap());
        assert!(!dir.path().join(CURSOR_FILE).exists());
    }

    #[tokio::test]
    async fn lines_over_the_rate_are_counted_not_queued() {
        let dir = tempfile::tempdir().unwrap();
        let prefix = fake_journalctl(dir.path());
        let uplink = Uplink::new(upload_settings());
        let (_trigger, mut stop) = crate::shutdown::channel();
        // One line at once, then one a second: c1 passes, c2 a second
        // later passes too only with a rate of at least one a second.
        let mut forwarder = Forwarder::with_program("/bin/sh", prefix, dir.path(), uplink.clone())
            .with_limit(TokenBucket::new(0.5, 1.0));
        forwarder.follow(&mut stop).await.unwrap();
        assert_eq!(
            (forwarder.stats().forwarded, forwarder.stats().rate_limited),
            (1, 1)
        );
        assert_eq!(uplink.take().len(), 1);
        assert_eq!(uplink.take_skipped(), 1);
        assert!(!uplink.has_pending());
    }

    #[tokio::test]
    async fn a_full_queue_pauses_reading_until_stopped() {
        let dir = tempfile::tempdir().unwrap();
        let prefix = fake_journalctl(dir.path());
        let uplink = Uplink::new(UploadSettings {
            capacity: 1,
            batch_size: 1,
            ..upload_settings()
        });
        let (trigger, mut stop) = crate::shutdown::channel();
        let mut forwarder = Forwarder::with_program("/bin/sh", prefix, dir.path(), uplink.clone());
        let task = tokio::spawn(async move {
            let stopped = forwarder.follow(&mut stop).await.unwrap();
            (stopped, forwarder.stats())
        });
        tokio::time::sleep(Duration::from_millis(300)).await;
        assert_eq!(uplink.len(), 1);
        trigger.fire();
        let (stopped, stats) = task.await.unwrap();
        assert!(stopped);
        assert_eq!(stats.forwarded, 1);
        // The cursor of the line queued, not of the ones left unread.
        let saved = std::fs::read_to_string(dir.path().join(CURSOR_FILE)).unwrap();
        assert_eq!(saved.trim(), "c1");
    }

    #[test]
    fn journal_access_is_checked_on_the_system_files() {
        let dir = tempfile::tempdir().unwrap();
        let missing = dir.path().join("missing");
        assert_eq!(system_journal_readable(&[&missing]), None);
        let machine = dir.path().join("0123");
        std::fs::create_dir(&machine).unwrap();
        assert_eq!(system_journal_readable(&[dir.path()]), Some(false));
        std::fs::write(machine.join("user-1000.journal"), b"").unwrap();
        assert_eq!(system_journal_readable(&[dir.path()]), Some(false));
        std::fs::write(machine.join("system.journal"), b"").unwrap();
        assert_eq!(system_journal_readable(&[&missing, dir.path()]), Some(true));
    }
}
