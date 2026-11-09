//! Turns an agent's `LogBatch` into VictoriaLogs records (ADR-0091).
//!
//! The `peer` and `site` stream fields come from the stream's identity,
//! never from the entry, as for samples (ADR-0013). Entries are bounded,
//! messages redacted with the rules of ADR-0040 before they are stored,
//! and lines the agent or this server dropped become a line of their own,
//! so the count shows where the gap is.

use std::sync::Arc;

use chrono::DateTime;
use monitorflow_common::proto::v1::{LogBatch, LogEntry};
use monitorflow_common::rate::TokenBucket;
use tokio::sync::{OwnedSemaphorePermit, Semaphore};

use super::logs::LogRecord;
use super::validate::{Identity, MAX_AGE, MAX_FUTURE};
use crate::remediation::redact::Redactor;

/// Entries in one `LogBatch`; a larger batch is dropped whole. Agents
/// send at most 500.
pub const MAX_ENTRIES_PER_BATCH: usize = 1_000;
/// Bytes of a message kept, as the agent cuts them.
pub const MAX_MESSAGE: usize = 4096;
/// Bytes of a message kept until it is redacted: twice [`MAX_MESSAGE`],
/// so a line that redaction shortens still fills what is stored, and a
/// huge one costs the redaction rules no more than that.
pub const MAX_UNREDACTED_MESSAGE: usize = 2 * MAX_MESSAGE;
/// Batches of one stream being redacted at once; the next one is dropped
/// and counted. Redaction keeps up with the rate limit many times over,
/// so only a stream sending oversized batches gets there.
pub const MAX_PENDING_BATCHES: usize = 4;
/// Bytes of a unit or identifier; a longer one is left out.
pub const MAX_FIELD: usize = 256;
/// Lines per second one stream may write after [`BURST`], by the clock.
/// The agent reads at most 200 a second.
pub const RATE_PER_SECOND: f64 = 500.0;
pub const BURST: f64 = 5_000.0;
/// The `source` field of agent lines.
pub const SOURCE: &str = "journal";
/// Rate of the agent's own limit, for the drop notice.
const AGENT_RATE: u64 = 10;

/// Why an entry was dropped.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum InvalidEntry {
    #[error("timestamp is more than {MAX_FUTURE:?} ahead of the server clock")]
    InFuture,
    #[error("timestamp is more than {MAX_AGE:?} old")]
    TooOld,
    #[error("priority is above 7")]
    Priority,
    #[error("source must be `journal`")]
    Source,
    #[error("batch has more than {MAX_ENTRIES_PER_BATCH} entries")]
    BatchTooLarge,
}

/// The records of one batch.
#[derive(Debug, Default)]
pub struct CheckedLogs {
    pub records: Vec<LogRecord>,
    pub invalid: u64,
    pub first_error: Option<InvalidEntry>,
    /// Valid lines over this server's rate limit.
    pub rate_limited: u64,
}

/// Per-stream rate limit, over the server clock.
#[derive(Debug)]
pub struct LogLimit(TokenBucket);

impl Default for LogLimit {
    fn default() -> Self {
        Self::new(RATE_PER_SECOND, BURST)
    }
}

impl LogLimit {
    pub fn new(rate_per_second: f64, burst: f64) -> Self {
        Self(TokenBucket::new(rate_per_second, burst))
    }
}

/// Checks `batch`, applies `limit` and builds unredacted records, oldest
/// first. Cheap; [`redact`] does the costly part.
pub fn check_logs(
    identity: &Identity,
    batch: LogBatch,
    now_ms: i64,
    limit: &mut LogLimit,
) -> CheckedLogs {
    let mut checked = CheckedLogs::default();
    if batch.entries.len() > MAX_ENTRIES_PER_BATCH {
        checked.invalid = batch.entries.len() as u64;
        checked.first_error = Some(InvalidEntry::BatchTooLarge);
        return checked;
    }
    if batch.rate_limited > 0 {
        let first = batch
            .entries
            .first()
            .map_or(now_ms, |e| e.timestamp_unix_ms);
        checked.records.push(notice(
            identity,
            first.clamp(now_ms - MAX_AGE.as_millis() as i64, now_ms),
            format!(
                "The agent dropped {}: they came faster than {AGENT_RATE} a second.",
                lines(batch.rate_limited)
            ),
        ));
    }
    for entry in batch.entries {
        match check_entry(&entry, now_ms) {
            Err(error) => {
                checked.invalid += 1;
                checked.first_error.get_or_insert(error);
            }
            Ok(()) if !limit.0.allow(now_ms) => checked.rate_limited += 1,
            Ok(()) => checked.records.push(record(identity, entry)),
        }
    }
    if checked.rate_limited > 0 {
        checked.records.push(notice(
            identity,
            now_ms,
            format!(
                "The server dropped {} of this agent: more than {RATE_PER_SECOND} a second.",
                lines(checked.rate_limited)
            ),
        ));
    }
    checked
}

/// "1 log line", "3 log lines".
fn lines(n: u64) -> String {
    if n == 1 {
        "1 log line".into()
    } else {
        format!("{n} log lines")
    }
}

fn check_entry(entry: &LogEntry, now_ms: i64) -> Result<(), InvalidEntry> {
    if entry.timestamp_unix_ms > now_ms + MAX_FUTURE.as_millis() as i64 {
        return Err(InvalidEntry::InFuture);
    }
    if entry.timestamp_unix_ms < now_ms - MAX_AGE.as_millis() as i64 {
        return Err(InvalidEntry::TooOld);
    }
    if entry.priority > 7 {
        return Err(InvalidEntry::Priority);
    }
    if entry.source != SOURCE {
        return Err(InvalidEntry::Source);
    }
    Ok(())
}

/// `value` if it is a short plain field, else empty (left out).
fn plain(value: String) -> String {
    if value.len() <= MAX_FIELD && !value.chars().any(char::is_control) {
        value
    } else {
        String::new()
    }
}

fn record(identity: &Identity, mut entry: LogEntry) -> LogRecord {
    // Cut before it is queued; [`redact`] still runs on all that is kept.
    entry
        .message
        .truncate(cut_at(&entry.message, MAX_UNREDACTED_MESSAGE));
    let time = DateTime::from_timestamp_millis(entry.timestamp_unix_ms).unwrap_or_default();
    let boot_id =
        if entry.boot_id.len() == 32 && entry.boot_id.bytes().all(|b| b.is_ascii_hexdigit()) {
            entry.boot_id
        } else {
            String::new()
        };
    let pid = if entry.pid > 0 {
        entry.pid.to_string()
    } else {
        String::new()
    };
    LogRecord {
        time,
        msg: entry.message,
        fields: Vec::new(),
    }
    .field("peer", &identity.peer)
    .field("site", &identity.site)
    .field("source", SOURCE)
    .field("priority", entry.priority.to_string())
    .field("unit", plain(entry.unit))
    .field("identifier", plain(entry.identifier))
    .field("boot_id", boot_id)
    .field("pid", pid)
}

/// A warning line of MonitorFlow's own about this agent's logs.
fn notice(identity: &Identity, at_ms: i64, msg: String) -> LogRecord {
    LogRecord {
        time: DateTime::from_timestamp_millis(at_ms).unwrap_or_default(),
        msg,
        fields: Vec::new(),
    }
    .field("peer", &identity.peer)
    .field("site", &identity.site)
    .field("source", SOURCE)
    .field("priority", "4")
    .field("identifier", "monitorflow")
}

/// The batches of one stream being redacted, at most
/// [`MAX_PENDING_BATCHES`] at once.
#[derive(Debug)]
pub struct Pending {
    permits: Arc<Semaphore>,
    /// Lines of batches dropped since the last one admitted.
    shed: u64,
}

impl Default for Pending {
    fn default() -> Self {
        Self {
            permits: Arc::new(Semaphore::new(MAX_PENDING_BATCHES)),
            shed: 0,
        }
    }
}

impl Pending {
    /// Admits a batch for redaction: hold the permit until it is done.
    /// With too many waiting, the batch is dropped instead and `Err` says
    /// how many lines; the next batch admitted gets a line saying so.
    pub fn admit(
        &mut self,
        identity: &Identity,
        records: &mut Vec<LogRecord>,
        now_ms: i64,
    ) -> Result<OwnedSemaphorePermit, u64> {
        let Ok(permit) = self.permits.clone().try_acquire_owned() else {
            let dropped = records.len() as u64;
            self.shed += dropped;
            return Err(dropped);
        };
        if self.shed > 0 {
            let dropped = std::mem::take(&mut self.shed);
            tracing::warn!(peer = %identity.peer, dropped, "dropped log lines: redaction was behind");
            records.push(notice(
                identity,
                now_ms,
                format!(
                    "The server dropped {} of this agent: they came faster than it could redact them.",
                    lines(dropped)
                ),
            ));
        }
        Ok(permit)
    }
}

/// Redacts secrets in every message, then cuts it to [`MAX_MESSAGE`]
/// bytes. Runs on the blocking pool: a batch of 500 lines takes a few
/// milliseconds.
pub fn redact(records: &mut [LogRecord]) {
    let mut redactor = Redactor::new();
    for record in records {
        let redacted = redactor.redact(&record.msg);
        record.msg = cut(redacted.as_str(), MAX_MESSAGE);
    }
}

fn cut(text: &str, max: usize) -> String {
    text[..cut_at(text, max)].to_owned()
}

/// The largest char boundary of `text` within `max` bytes.
fn cut_at(text: &str, max: usize) -> usize {
    let mut end = text.len().min(max);
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    end
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    const NOW: i64 = 1_800_000_000_000;

    fn identity() -> Identity {
        Identity {
            peer: "agent-1".into(),
            site: "paris".into(),
        }
    }

    fn entry(message: &str) -> LogEntry {
        LogEntry {
            timestamp_unix_ms: NOW - 1_000,
            message: message.into(),
            priority: 3,
            unit: "nginx.service".into(),
            identifier: "nginx".into(),
            boot_id: "0123456789abcdef0123456789abcdef".into(),
            pid: 42,
            source: "journal".into(),
        }
    }

    fn batch(entries: Vec<LogEntry>) -> LogBatch {
        LogBatch {
            entries,
            rate_limited: 0,
        }
    }

    #[test]
    fn entries_get_the_streams_identity() {
        let checked = check_logs(
            &identity(),
            batch(vec![entry("disk full")]),
            NOW,
            &mut LogLimit::default(),
        );
        assert_eq!(checked.invalid, 0);
        assert_eq!(
            checked.records[0].to_json(),
            json!({
                "_time": "2027-01-15T07:59:59.000Z",
                "_msg": "disk full",
                "peer": "agent-1",
                "site": "paris",
                "source": "journal",
                "priority": "3",
                "unit": "nginx.service",
                "identifier": "nginx",
                "boot_id": "0123456789abcdef0123456789abcdef",
                "pid": "42",
            })
        );
    }

    #[test]
    fn bad_entries_are_dropped_and_odd_fields_left_out() {
        let future = LogEntry {
            timestamp_unix_ms: NOW + 11 * 60 * 1000,
            ..entry("a")
        };
        let old = LogEntry {
            timestamp_unix_ms: NOW - 8 * 24 * 60 * 60 * 1000,
            ..entry("b")
        };
        let priority = LogEntry {
            priority: 8,
            ..entry("c")
        };
        let odd = LogEntry {
            unit: "a\nb".into(),
            identifier: "x".repeat(MAX_FIELD + 1),
            boot_id: "not-hex".into(),
            pid: 0,
            ..entry("d")
        };
        let checked = check_logs(
            &identity(),
            batch(vec![future, old, priority, odd]),
            NOW,
            &mut LogLimit::default(),
        );
        assert_eq!(checked.invalid, 3);
        assert_eq!(checked.first_error, Some(InvalidEntry::InFuture));
        let json = checked.records[0].to_json();
        assert_eq!(json["_msg"], "d");
        for field in ["unit", "identifier", "boot_id", "pid"] {
            assert!(json.get(field).is_none(), "{field}: {json}");
        }

        let large = batch(vec![entry("x"); MAX_ENTRIES_PER_BATCH + 1]);
        let checked = check_logs(&identity(), large, NOW, &mut LogLimit::default());
        assert!(checked.records.is_empty());
        assert_eq!(checked.first_error, Some(InvalidEntry::BatchTooLarge));
    }

    #[test]
    fn a_peer_cannot_write_another_peers_stream() {
        // The entry has no peer or site of its own; they come from the stream.
        let checked = check_logs(
            &identity(),
            batch(vec![entry("peer=other site=x")]),
            NOW,
            &mut LogLimit::default(),
        );
        let json = checked.records[0].to_json();
        assert_eq!(
            (json["peer"].as_str(), json["site"].as_str()),
            (Some("agent-1"), Some("paris"))
        );
    }

    #[test]
    fn the_rate_limit_drops_lines_and_says_how_many() {
        let mut limit = LogLimit::new(1.0, 2.0);
        let checked = check_logs(&identity(), batch(vec![entry("x"); 5]), NOW, &mut limit);
        assert_eq!(checked.rate_limited, 3);
        let messages: Vec<_> = checked.records.iter().map(|r| r.msg.as_str()).collect();
        assert_eq!(
            messages,
            [
                "x",
                "x",
                "The server dropped 3 log lines of this agent: more than 500 a second."
            ]
        );
        let checked = check_logs(
            &identity(),
            batch(vec![entry("y"); 2]),
            NOW + 1_000,
            &mut limit,
        );
        assert_eq!((checked.records.len(), checked.rate_limited), (2, 1));
    }

    #[test]
    fn the_agents_drops_become_a_line() {
        let checked = check_logs(
            &identity(),
            LogBatch {
                entries: vec![entry("after")],
                rate_limited: 1234,
            },
            NOW,
            &mut LogLimit::default(),
        );
        let first = checked.records[0].to_json();
        assert_eq!(
            first["_msg"],
            "The agent dropped 1234 log lines: they came faster than 10 a second."
        );
        assert_eq!(
            (first["priority"].as_str(), first["identifier"].as_str()),
            (Some("4"), Some("monitorflow"))
        );
        assert_eq!(first["_time"], checked.records[1].to_json()["_time"]);
        // Drops alone, with no line, still show.
        let alone = check_logs(
            &identity(),
            LogBatch {
                entries: vec![],
                rate_limited: 1,
            },
            NOW,
            &mut LogLimit::default(),
        );
        assert_eq!(alone.records.len(), 1);
        assert_eq!(
            alone.records[0].msg,
            "The agent dropped 1 log line: they came faster than 10 a second."
        );
    }

    #[test]
    fn secrets_are_redacted_and_messages_cut() {
        let mut records = check_logs(
            &identity(),
            batch(vec![
                entry("login failed for password=hunter2hunter2 from 10.0.0.1"),
                entry(&"é".repeat(MAX_MESSAGE)),
            ]),
            NOW,
            &mut LogLimit::default(),
        )
        .records;
        redact(&mut records);
        assert!(!records[0].msg.contains("hunter2"), "{}", records[0].msg);
        assert!(records[0].msg.contains("[REDACTED"), "{}", records[0].msg);
        assert!(records[0].msg.contains("10.0.0.1"));
        assert!(records[1].msg.len() <= MAX_MESSAGE);
    }

    #[test]
    fn a_huge_message_is_cut_before_it_is_queued_and_still_redacted() {
        // A secret early in the line, then megabytes of multi-byte text.
        let message = format!("password=hunter2hunter2 {}", "é".repeat(1 << 20));
        let mut checked = check_logs(
            &identity(),
            batch(vec![entry(&message), entry("short")]),
            NOW,
            &mut LogLimit::default(),
        );
        let queued = &checked.records[0].msg;
        assert!(queued.len() <= MAX_UNREDACTED_MESSAGE, "{}", queued.len());
        assert!(queued.len() > MAX_UNREDACTED_MESSAGE - 4);
        assert_eq!(checked.records[1].msg, "short");

        redact(&mut checked.records);
        let stored = &checked.records[0].msg;
        assert!(stored.len() <= MAX_MESSAGE);
        assert!(!stored.contains("hunter2"), "{stored:.60}");
    }

    #[test]
    fn a_stream_holds_a_bounded_number_of_batches_in_redaction() {
        let mut pending = Pending::default();
        let records = || {
            check_logs(
                &identity(),
                batch(vec![entry("a"), entry("b")]),
                NOW,
                &mut LogLimit::default(),
            )
            .records
        };
        let mut held: Vec<_> = (0..MAX_PENDING_BATCHES)
            .map(|_| pending.admit(&identity(), &mut records(), NOW).unwrap())
            .collect();
        // The next batches are dropped whole and counted.
        assert_eq!(
            pending.admit(&identity(), &mut records(), NOW).unwrap_err(),
            2
        );
        assert_eq!(
            pending.admit(&identity(), &mut records(), NOW).unwrap_err(),
            2
        );

        // One finishes: the next is admitted, with a line saying what was lost.
        held.pop();
        let mut next = records();
        let _permit = pending.admit(&identity(), &mut next, NOW).unwrap();
        assert_eq!(next.len(), 3);
        assert!(
            next[2].msg.contains("dropped 4 log lines"),
            "{}",
            next[2].msg
        );
        held.pop();
        let mut after = records();
        let _permit = pending.admit(&identity(), &mut after, NOW).unwrap();
        assert_eq!(after.len(), 2);
    }
}
