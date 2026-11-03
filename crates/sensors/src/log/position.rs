//! Where a log sensor stopped reading, saved under the agent's data
//! directory so a restart or a configuration change resumes there
//! (ADR-0085).

use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use super::{FileId, Head};

/// A position saved longer ago than this is ignored: the sensor starts at
/// the end of the file, as a new one does, so a sensor removed and added
/// back much later does not count old lines.
pub const MAX_AGE: Duration = Duration::from_secs(24 * 60 * 60);
/// A position that did not change is saved again after this, so it stays
/// younger than [`MAX_AGE`] while the log is quiet.
pub const REFRESH: Duration = Duration::from_secs(60 * 60);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Position {
    pub id: FileId,
    /// Start of the first line not counted yet.
    pub offset: u64,
    pub head: Option<Head>,
}

/// FNV-1a: stable across Rust versions, unlike `DefaultHasher`.
pub fn fnv(bytes: &[u8]) -> u64 {
    bytes.iter().fold(0xcbf2_9ce4_8422_2325, |hash, b| {
        (hash ^ u64::from(*b)).wrapping_mul(0x0100_0000_01b3)
    })
}

/// The file of a sensor's position in `dir`: one per path and patterns, so
/// two sensors on the same file do not share one.
pub fn file(dir: &Path, path: &str, pattern: &str, exclude: Option<&str>) -> PathBuf {
    let key = [path, pattern, exclude.unwrap_or("")].join("\0");
    dir.join(format!("{:016x}.pos", fnv(key.as_bytes())))
}

fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

/// The position saved in `file`, unless missing, unreadable or too old.
pub fn load(file: &Path) -> Option<Position> {
    let text = std::fs::read_to_string(file).ok()?;
    let (position, saved) = parse(&text)?;
    (now().saturating_sub(saved) <= MAX_AGE.as_secs()).then_some(position)
}

fn parse(text: &str) -> Option<(Position, u64)> {
    let mut fields = text.split_whitespace();
    let mut hex = || u128::from_str_radix(fields.next()?, 16).ok();
    let (volume, index, offset, head_len, head_hash, saved) =
        (hex()?, hex()?, hex()?, hex()?, hex()?, hex()?);
    let head = (head_len > 0).then_some(Head {
        len: u64::try_from(head_len).ok()?,
        hash: u64::try_from(head_hash).ok()?,
    });
    let position = Position {
        id: FileId {
            volume: u64::try_from(volume).ok()?,
            index,
        },
        offset: u64::try_from(offset).ok()?,
        head,
    };
    Some((position, u64::try_from(saved).ok()?))
}

fn format(position: &Position, saved: u64) -> String {
    let head = position.head.unwrap_or(Head { len: 0, hash: 0 });
    format!(
        "{:x} {:x} {:x} {:x} {:x} {:x}\n",
        position.id.volume, position.id.index, position.offset, head.len, head.hash, saved
    )
}

/// Writes `position` to `file` atomically: a crash leaves the old one.
pub fn save(file: &Path, position: &Position) -> io::Result<()> {
    static TEMP: AtomicU64 = AtomicU64::new(0);
    if let Some(dir) = file.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let temp = file.with_extension(format!("{}.tmp", TEMP.fetch_add(1, Ordering::Relaxed)));
    std::fs::write(&temp, format(position, now()))?;
    std::fs::rename(&temp, file).inspect_err(|_| {
        let _ = std::fs::remove_file(&temp);
    })
}

/// What was last written, to save only changes and refreshes.
#[derive(Debug, Default)]
pub struct Saved {
    last: Option<(Position, SystemTime)>,
    failing: bool,
}

impl Saved {
    /// Saves `position` if it changed or is due for a refresh. A failure is
    /// logged once until a save works again.
    pub fn update(&mut self, file: &Path, position: Position) {
        let due = match self.last {
            Some((last, at)) => last != position || at.elapsed().map_or(true, |e| e >= REFRESH),
            None => true,
        };
        if !due {
            return;
        }
        match save(file, &position) {
            Ok(()) => {
                self.last = Some((position, SystemTime::now()));
                self.failing = false;
            }
            Err(error) => {
                if !std::mem::replace(&mut self.failing, true) {
                    tracing::warn!(
                        file = %file.display(),
                        %error,
                        "cannot save where the log sensor stopped; a restart will skip what was written meanwhile"
                    );
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn positions_round_trip_and_expire() {
        let dir = tempfile::tempdir().unwrap();
        let path = file(
            dir.path().join("positions").as_path(),
            "/var/log/a",
            "x",
            None,
        );
        assert_ne!(path, file(dir.path(), "/var/log/a", "x", Some("y")));
        let position = Position {
            id: FileId {
                volume: u64::MAX,
                index: u128::MAX - 1,
            },
            offset: 42,
            head: Some(Head { len: 256, hash: 7 }),
        };
        assert_eq!(load(&path), None);
        save(&path, &position).unwrap();
        assert_eq!(load(&path), Some(position));
        let none = Position {
            head: None,
            ..position
        };
        save(&path, &none).unwrap();
        assert_eq!(load(&path), Some(none));

        let old = now() - MAX_AGE.as_secs() - 1;
        std::fs::write(&path, format(&position, old)).unwrap();
        assert_eq!(load(&path), None);
        for bad in ["", "1 2 3", "x y z 1 2 3", "1 2 3 4 5 1ffffffffffffffff"] {
            assert!(parse(bad).is_none(), "{bad}");
        }
        assert_eq!(fnv(b""), 0xcbf2_9ce4_8422_2325);
    }
}
