//! Reading and parsing files under `/proc`. Reads are capped so a huge or
//! endless file cannot exhaust memory; parsing is done by pure functions
//! in each sensor so tests use fixtures instead of the host.

use std::io::{ErrorKind, Read};
use std::path::Path;
use std::str::FromStr;

use crate::SensorError;

/// Reads `path` as text, at most `cap` bytes. A missing file is a
/// permanent error (the kernel lacks the interface), a larger one or any
/// other I/O error is transient.
///
/// Files under `/proc` are generated in memory, so reading them on the
/// runtime is fine (see [`crate::Sensor`]).
#[cfg_attr(not(unix), allow(dead_code))]
pub fn read_capped(path: impl AsRef<Path>, cap: usize) -> Result<String, SensorError> {
    let path = path.as_ref();
    let io = |e: std::io::Error| {
        let message = format!("cannot read {}: {e}", path.display());
        match e.kind() {
            ErrorKind::NotFound => SensorError::Permanent(message),
            _ => SensorError::Transient(message),
        }
    };
    let file = std::fs::File::open(path).map_err(io)?;
    let mut bytes = Vec::new();
    let limit = u64::try_from(cap).unwrap_or(u64::MAX).saturating_add(1);
    file.take(limit).read_to_end(&mut bytes).map_err(io)?;
    if bytes.len() > cap {
        return Err(SensorError::Transient(format!(
            "{} is larger than {cap} bytes",
            path.display()
        )));
    }
    String::from_utf8(bytes)
        .map_err(|_| SensorError::Transient(format!("{} is not UTF-8", path.display())))
}

/// A file that does not have the expected format. Transient: a kernel
/// with another format would fail on every tick and be logged, while a
/// one-off odd read recovers.
pub fn malformed(file: &str, what: impl std::fmt::Display) -> SensorError {
    SensorError::Transient(format!("{file}: {what}"))
}

/// Parses the `index`-th whitespace-separated field of `line`.
pub fn field<T: FromStr>(line: &str, index: usize) -> Option<T> {
    line.split_ascii_whitespace().nth(index)?.parse().ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_are_capped() {
        let dir = std::env::temp_dir().join(format!("mf-procfs-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("f");
        std::fs::write(&path, "0123456789").unwrap();
        assert_eq!(read_capped(&path, 10).unwrap(), "0123456789");
        assert!(matches!(
            read_capped(&path, 9),
            Err(SensorError::Transient(_))
        ));
        std::fs::write(&path, [0xff, 0xfe]).unwrap();
        assert!(matches!(
            read_capped(&path, 9),
            Err(SensorError::Transient(_))
        ));
        assert!(read_capped(dir.join("missing"), 9)
            .unwrap_err()
            .is_permanent());
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn fields() {
        assert_eq!(field::<u64>("cpu0  12 34", 2), Some(34));
        assert_eq!(field::<u64>("cpu0 12", 2), None);
        assert_eq!(field::<u64>("cpu0 x", 1), None);
        assert_eq!(field::<f64>("0.5 1", 0), Some(0.5));
    }
}
