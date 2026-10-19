//! The segment files of the store-and-forward buffer (ADR-0027).
//!
//! `<dir>/agent-<id>/` holds the agent's last `Hello` in `hello.pb` and its
//! records in `<n>.seg`, `n` from one counter for the whole store, so the
//! smallest segment anywhere is the oldest. A record is a 16-byte header
//! (payload length, CRC-32C of the rest, time written) and the payload.
//!
//! This is synchronous file I/O: callers on the runtime go through
//! [`super::Buffer`], which runs it on the blocking pool.

use std::collections::{BTreeMap, VecDeque};
use std::fs::{self, File, OpenOptions};
use std::io::{self, BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::time::Duration;

use crc::{Crc, CRC_32_ISCSI};
use monitorflow_common::tls;

/// Length, checksum and time written.
pub const HEADER_LEN: u64 = 16;
/// Larger lengths are corruption: a batch fits in tonic's 4 MiB limit.
pub const MAX_RECORD: u64 = 16 * 1024 * 1024;
/// Upper bound of one segment's size.
pub const MAX_SEGMENT: u64 = 16 * 1024 * 1024;
const CRC: Crc<u32> = Crc::<u32>::new(&CRC_32_ISCSI);
const AGENT_PREFIX: &str = "agent-";
const HELLO_FILE: &str = "hello.pb";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Limits {
    /// Total size of all segments.
    pub max_bytes: u64,
    /// Records written longer ago than this are dropped.
    pub max_age: Duration,
    /// A segment is closed once it reaches this size.
    pub segment_bytes: u64,
}

impl Limits {
    /// Segments of an eighth of `max_bytes`, at most [`MAX_SEGMENT`], so
    /// dropping the oldest never drops much more than an eighth.
    pub fn new(max_bytes: u64, max_age: Duration) -> Self {
        Self {
            max_bytes,
            max_age,
            segment_bytes: (max_bytes / 8).clamp(1, MAX_SEGMENT),
        }
    }
}

/// Counters since the store was opened, and its current size.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Stats {
    pub bytes: u64,
    pub segments: u64,
    pub appended: u64,
    /// Records dropped to make room, or larger than the whole buffer.
    pub dropped_full: u64,
    /// Records older than the age cap.
    pub dropped_expired: u64,
    /// Records of agents the upstream refused.
    pub dropped_refused: u64,
}

impl Stats {
    pub fn dropped(&self) -> u64 {
        self.dropped_full + self.dropped_expired + self.dropped_refused
    }
}

#[derive(Debug)]
struct Segment {
    id: u64,
    bytes: u64,
    records: u64,
    newest_ms: i64,
}

/// Where the next read starts in an agent's records.
struct Cursor {
    segment: u64,
    offset: u64,
    read: u64,
    file: BufReader<File>,
}

#[derive(Default)]
struct Queue {
    segments: VecDeque<Segment>,
    /// Open for appending, always on the last segment.
    writer: Option<File>,
    dirty: bool,
    cursor: Option<Cursor>,
    hello: Option<Vec<u8>>,
}

pub struct Store {
    dir: PathBuf,
    limits: Limits,
    agents: BTreeMap<String, Queue>,
    next_id: u64,
    stats: Stats,
}

impl Store {
    /// Opens or creates the store in `dir`, truncating each segment after
    /// its last valid record.
    pub fn open(dir: &Path, limits: Limits) -> io::Result<Self> {
        fs::create_dir_all(dir)?;
        let mut store = Self {
            dir: dir.to_owned(),
            limits,
            agents: BTreeMap::new(),
            next_id: 1,
            stats: Stats::default(),
        };
        for entry in fs::read_dir(dir)? {
            let entry = entry?;
            let name = entry.file_name();
            let Some(id) = name.to_str().and_then(|n| n.strip_prefix(AGENT_PREFIX)) else {
                continue;
            };
            if tls::validate_id(id).is_err() || !entry.file_type()?.is_dir() {
                tracing::warn!(path = %entry.path().display(), "ignoring unexpected entry in the buffer");
                continue;
            }
            let queue = store.load_queue(&entry.path())?;
            store.agents.insert(id.to_owned(), queue);
        }
        let stats = store.stats;
        tracing::info!(
            bytes = stats.bytes,
            segments = stats.segments,
            agents = store.pending().len(),
            "buffer opened"
        );
        Ok(store)
    }

    fn load_queue(&mut self, dir: &Path) -> io::Result<Queue> {
        let mut queue = Queue {
            hello: read_optional(&dir.join(HELLO_FILE))?,
            ..Queue::default()
        };
        let mut ids = Vec::new();
        for entry in fs::read_dir(dir)? {
            let path = entry?.path();
            match path.extension().and_then(|e| e.to_str()) {
                Some("seg") => match segment_id(&path) {
                    Some(id) => ids.push(id),
                    None => {
                        tracing::warn!(path = %path.display(), "ignoring unexpected segment name")
                    }
                },
                // A Hello write that did not finish.
                Some("tmp") => fs::remove_file(&path)?,
                _ => {}
            }
        }
        ids.sort_unstable();
        for id in ids {
            let path = segment_path(dir, id);
            self.next_id = self.next_id.max(id + 1);
            match recover(&path)? {
                Some(segment) => {
                    self.stats.bytes += segment.bytes;
                    self.stats.segments += 1;
                    queue.segments.push_back(segment);
                }
                None => fs::remove_file(&path)?,
            }
        }
        Ok(queue)
    }

    pub fn limits(&self) -> Limits {
        self.limits
    }

    pub fn stats(&self) -> Stats {
        self.stats
    }

    /// Agents with records to send, in ID order.
    pub fn pending(&self) -> Vec<String> {
        self.agents
            .iter()
            .filter(|(_, q)| !q.segments.is_empty())
            .map(|(id, _)| id.clone())
            .collect()
    }

    pub fn has_pending(&self, agent: &str) -> bool {
        self.agents
            .get(agent)
            .is_some_and(|q| !q.segments.is_empty())
    }

    /// Stores the agent's latest `Hello`, sent first when its records are
    /// replayed.
    pub fn set_hello(&mut self, agent: &str, hello: &[u8]) -> io::Result<()> {
        let dir = self.agent_dir(agent)?;
        let queue = self.agents.entry(agent.to_owned()).or_default();
        if queue.hello.as_deref() == Some(hello) {
            return Ok(());
        }
        let tmp = dir.join(format!("{HELLO_FILE}.tmp"));
        let mut file = File::create(&tmp)?;
        file.write_all(hello)?;
        file.sync_all()?;
        fs::rename(&tmp, dir.join(HELLO_FILE))?;
        queue.hello = Some(hello.to_vec());
        Ok(())
    }

    pub fn hello(&self, agent: &str) -> Option<&[u8]> {
        self.agents.get(agent)?.hello.as_deref()
    }

    /// Appends a record written at `now_ms`, dropping the oldest segments
    /// of the store until it fits. Returns false if the record alone is
    /// larger than the buffer, and is dropped.
    pub fn append(&mut self, agent: &str, now_ms: i64, payload: &[u8]) -> io::Result<bool> {
        let len = HEADER_LEN + payload.len() as u64;
        if payload.len() as u64 > MAX_RECORD || len > self.limits.max_bytes {
            self.stats.dropped_full += 1;
            return Ok(false);
        }
        while self.stats.bytes + len > self.limits.max_bytes {
            self.drop_oldest()?;
        }
        let dir = self.agent_dir(agent)?;
        let segment_bytes = self.limits.segment_bytes;
        let queue = self.agents.entry(agent.to_owned()).or_default();
        let full = queue
            .segments
            .back()
            .is_some_and(|s| s.bytes > 0 && s.bytes + len > segment_bytes);
        if queue.writer.is_none() || full {
            if let Some(mut old) = queue.writer.take() {
                if queue.dirty {
                    old.flush()?;
                    old.sync_data()?;
                }
            }
            let id = self.next_id;
            self.next_id += 1;
            let file = OpenOptions::new()
                .append(true)
                .create_new(true)
                .open(segment_path(&dir, id))?;
            queue.writer = Some(file);
            queue.segments.push_back(Segment {
                id,
                bytes: 0,
                records: 0,
                newest_ms: now_ms,
            });
            self.stats.segments += 1;
        }
        let mut record = Vec::with_capacity(len as usize);
        record.extend_from_slice(&(payload.len() as u32).to_le_bytes());
        record.extend_from_slice(&checksum(now_ms, payload).to_le_bytes());
        record.extend_from_slice(&now_ms.to_le_bytes());
        record.extend_from_slice(payload);
        let writer = queue.writer.as_mut().expect("opened above");
        if let Err(error) = writer.write_all(&record) {
            // A partial write would be a torn record in the middle of the
            // segment: close it, so the next append starts a new one.
            queue.writer = None;
            return Err(error);
        }
        let segment = queue.segments.back_mut().expect("pushed above");
        segment.bytes += len;
        segment.records += 1;
        segment.newest_ms = segment.newest_ms.max(now_ms);
        queue.dirty = true;
        self.stats.bytes += len;
        self.stats.appended += 1;
        Ok(true)
    }

    /// Flushes every segment written since the last call to disk.
    pub fn sync(&mut self) -> io::Result<()> {
        for queue in self.agents.values_mut().filter(|q| q.dirty) {
            if let Some(writer) = &mut queue.writer {
                writer.sync_data()?;
            }
            queue.dirty = false;
        }
        Ok(())
    }

    /// Up to `max` records of `agent` after the cursor, oldest first,
    /// skipping those older than the age cap at `now_ms`. Records stay on
    /// disk until [`Self::commit`].
    pub fn read(&mut self, agent: &str, now_ms: i64, max: usize) -> io::Result<Vec<Vec<u8>>> {
        let cutoff = now_ms.saturating_sub(self.limits.max_age.as_millis() as i64);
        let dir = self.dir.join(format!("{AGENT_PREFIX}{agent}"));
        let mut out = Vec::new();
        let Some(queue) = self.agents.get_mut(agent) else {
            return Ok(out);
        };
        while out.len() < max {
            let from = queue.cursor.as_ref().map_or(0, |c| c.segment);
            let Some(index) = queue.segments.iter().position(|s| s.id >= from) else {
                break;
            };
            let (id, bytes) = (queue.segments[index].id, queue.segments[index].bytes);
            if queue.cursor.as_ref().is_none_or(|c| c.segment != id) {
                let file = File::open(segment_path(&dir, id))?;
                queue.cursor = Some(Cursor {
                    segment: id,
                    offset: 0,
                    read: 0,
                    file: BufReader::new(file),
                });
            }
            let cursor = queue.cursor.as_mut().expect("set above");
            if cursor.offset >= bytes {
                // Stay on the last segment: it may still grow.
                let Some(next) = queue.segments.get(index + 1) else {
                    break;
                };
                *cursor = Cursor {
                    segment: next.id,
                    offset: 0,
                    read: 0,
                    file: BufReader::new(File::open(segment_path(&dir, next.id))?),
                };
                continue;
            }
            match read_record(&mut cursor.file)? {
                Some((written_ms, payload)) => {
                    cursor.offset += HEADER_LEN + payload.len() as u64;
                    cursor.read += 1;
                    if written_ms < cutoff {
                        self.stats.dropped_expired += 1;
                    } else {
                        out.push(payload);
                    }
                }
                None => {
                    // Checked when written or at start: the disk changed
                    // under us. Skip the rest of the segment.
                    tracing::warn!(
                        agent_id = agent,
                        segment = id,
                        "unreadable record in the buffer; skipping the rest of its segment"
                    );
                    cursor.offset = bytes;
                }
            }
        }
        Ok(out)
    }

    /// Deletes the segments of `agent` the cursor has read to the end:
    /// call once what [`Self::read`] returned was handed upstream.
    pub fn commit(&mut self, agent: &str) -> io::Result<()> {
        let Some(queue) = self.agents.get_mut(agent) else {
            return Ok(());
        };
        let Some(cursor) = &queue.cursor else {
            return Ok(());
        };
        let (at, offset) = (cursor.segment, cursor.offset);
        let done = queue
            .segments
            .iter()
            .take_while(|s| s.id < at || (s.id == at && offset >= s.bytes))
            .count();
        for _ in 0..done {
            self.pop_front(agent)?;
        }
        Ok(())
    }

    /// Reads `agent` from its oldest record again, after an upstream
    /// stream failed before its records were committed.
    pub fn rewind(&mut self, agent: &str) {
        if let Some(queue) = self.agents.get_mut(agent) {
            queue.cursor = None;
        }
    }

    /// Deletes every record of `agent`, which the upstream refused.
    pub fn discard(&mut self, agent: &str) -> io::Result<u64> {
        let Some(queue) = self.agents.get_mut(agent) else {
            return Ok(0);
        };
        queue.writer = None;
        queue.dirty = false;
        queue.cursor = None;
        let segments: Vec<_> = queue.segments.drain(..).collect();
        let mut records = 0;
        for segment in segments {
            records += segment.records;
            self.remove(agent, &segment)?;
        }
        self.stats.dropped_refused += records;
        Ok(records)
    }

    /// Deletes the segments whose newest record is older than the age cap.
    pub fn expire(&mut self, now_ms: i64) -> io::Result<u64> {
        let cutoff = now_ms.saturating_sub(self.limits.max_age.as_millis() as i64);
        let mut dropped = 0;
        let agents: Vec<_> = self.agents.keys().cloned().collect();
        for agent in agents {
            while let Some(segment) = self.agents[&agent].segments.front() {
                if segment.newest_ms >= cutoff {
                    break;
                }
                let unread = self.unread(&agent, segment);
                self.stats.dropped_expired += unread;
                dropped += unread;
                self.pop_front(&agent)?;
            }
        }
        Ok(dropped)
    }

    /// Deletes the oldest segment of the whole store.
    fn drop_oldest(&mut self) -> io::Result<()> {
        let oldest = self
            .agents
            .iter()
            .filter_map(|(agent, q)| Some((q.segments.front()?.id, agent.clone())))
            .min();
        let Some((_, agent)) = oldest else {
            return Err(io::Error::other(
                "buffer accounting is wrong: full but empty",
            ));
        };
        let segment = self.agents[&agent].segments.front().expect("found above");
        let unread = self.unread(&agent, segment);
        self.stats.dropped_full += unread;
        self.pop_front(&agent)
    }

    fn unread(&self, agent: &str, segment: &Segment) -> u64 {
        match &self.agents[agent].cursor {
            Some(c) if c.segment == segment.id => segment.records - c.read,
            Some(c) if c.segment > segment.id => 0,
            _ => segment.records,
        }
    }

    fn pop_front(&mut self, agent: &str) -> io::Result<()> {
        let queue = self.agents.get_mut(agent).expect("caller checked");
        let segment = queue.segments.pop_front().expect("caller checked");
        if queue.segments.is_empty() {
            queue.writer = None;
            queue.dirty = false;
        }
        if queue
            .cursor
            .as_ref()
            .is_some_and(|c| c.segment <= segment.id)
        {
            queue.cursor = None;
        }
        self.remove(agent, &segment)
    }

    fn remove(&mut self, agent: &str, segment: &Segment) -> io::Result<()> {
        self.stats.bytes -= segment.bytes;
        self.stats.segments -= 1;
        let path = segment_path(&self.dir.join(format!("{AGENT_PREFIX}{agent}")), segment.id);
        match fs::remove_file(path) {
            Err(error) if error.kind() != io::ErrorKind::NotFound => Err(error),
            _ => Ok(()),
        }
    }

    fn agent_dir(&self, agent: &str) -> io::Result<PathBuf> {
        tls::validate_id(agent).map_err(io::Error::other)?;
        let dir = self.dir.join(format!("{AGENT_PREFIX}{agent}"));
        if !self.agents.contains_key(agent) {
            fs::create_dir_all(&dir)?;
        }
        Ok(dir)
    }
}

fn segment_path(dir: &Path, id: u64) -> PathBuf {
    dir.join(format!("{id:020}.seg"))
}

fn segment_id(path: &Path) -> Option<u64> {
    path.file_stem()?.to_str()?.parse().ok()
}

fn checksum(written_ms: i64, payload: &[u8]) -> u32 {
    let mut digest = CRC.digest();
    digest.update(&written_ms.to_le_bytes());
    digest.update(payload);
    digest.finalize()
}

fn read_optional(path: &Path) -> io::Result<Option<Vec<u8>>> {
    match fs::read(path) {
        Ok(bytes) => Ok(Some(bytes)),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error),
    }
}

/// The next record, or `None` at the end of the file or at a torn or
/// corrupt record.
fn read_record(file: &mut impl Read) -> io::Result<Option<(i64, Vec<u8>)>> {
    let mut header = [0u8; HEADER_LEN as usize];
    if !read_full(file, &mut header)? {
        return Ok(None);
    }
    let len = u32::from_le_bytes(header[0..4].try_into().expect("4 bytes"));
    let crc = u32::from_le_bytes(header[4..8].try_into().expect("4 bytes"));
    let written_ms = i64::from_le_bytes(header[8..16].try_into().expect("8 bytes"));
    if u64::from(len) > MAX_RECORD {
        return Ok(None);
    }
    let mut payload = vec![0u8; len as usize];
    if !read_full(file, &mut payload)? || checksum(written_ms, &payload) != crc {
        return Ok(None);
    }
    Ok(Some((written_ms, payload)))
}

/// Fills `buf`; false if the file ended first.
fn read_full(file: &mut impl Read, buf: &mut [u8]) -> io::Result<bool> {
    let mut filled = 0;
    while filled < buf.len() {
        match file.read(&mut buf[filled..]) {
            Ok(0) => return Ok(false),
            Ok(n) => filled += n,
            Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
            Err(error) => return Err(error),
        }
    }
    Ok(true)
}

/// Scans a segment, truncating it after its last valid record; `None` if
/// none is.
fn recover(path: &Path) -> io::Result<Option<Segment>> {
    let file = OpenOptions::new().read(true).write(true).open(path)?;
    let size = file.metadata()?.len();
    let mut reader = BufReader::new(file);
    let mut segment = Segment {
        id: segment_id(path).expect("listed by id"),
        bytes: 0,
        records: 0,
        newest_ms: i64::MIN,
    };
    while let Some((written_ms, payload)) = read_record(&mut reader)? {
        segment.bytes += HEADER_LEN + payload.len() as u64;
        segment.records += 1;
        segment.newest_ms = segment.newest_ms.max(written_ms);
    }
    if segment.bytes < size {
        tracing::warn!(
            path = %path.display(),
            kept = segment.bytes,
            truncated = size - segment.bytes,
            "truncating a torn or corrupt end of a buffer segment"
        );
        let file = reader.into_inner();
        file.set_len(segment.bytes)?;
        file.sync_all()?;
    }
    Ok((segment.records > 0).then_some(segment))
}

#[cfg(test)]
mod tests;
