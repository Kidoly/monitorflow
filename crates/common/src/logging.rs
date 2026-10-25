//! Tracing setup shared by the three binaries.

use std::ffi::OsStr;
use std::fs::{File, OpenOptions};
use std::io::{self, IsTerminal, Write};
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use crate::config::{LogConfig, LogFormat, LogLevel};

impl From<LogLevel> for tracing::Level {
    fn from(level: LogLevel) -> Self {
        match level {
            LogLevel::Trace => Self::TRACE,
            LogLevel::Debug => Self::DEBUG,
            LogLevel::Info => Self::INFO,
            LogLevel::Warn => Self::WARN,
            LogLevel::Error => Self::ERROR,
        }
    }
}

/// Installs the global subscriber with the configured level and format.
///
/// Logs go to stdout. Text logs are colored only when stdout is a terminal
/// and `NO_COLOR` is unset or empty (<https://no-color.org>), so files and
/// journald never get escape codes.
pub fn init(log: &LogConfig) {
    let no_color = std::env::var_os("NO_COLOR");
    let ansi = ansi_enabled(std::io::stdout().is_terminal(), no_color.as_deref());
    let builder = tracing_subscriber::fmt()
        .with_max_level(tracing::Level::from(log.level))
        .with_ansi(ansi);
    match log.format {
        LogFormat::Text => builder.init(),
        LogFormat::Json => builder.json().init(),
    }
}

/// Like [`init`], into `path` through a [`RotatingFile`], never colored:
/// for a Windows service, which has no console and no journald.
pub fn init_file(log: &LogConfig, path: &Path) -> io::Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let file = RotatingFile::open(path, MAX_LOG_FILE)?;
    let builder = tracing_subscriber::fmt()
        .with_max_level(tracing::Level::from(log.level))
        .with_ansi(false)
        .with_writer(file);
    match log.format {
        LogFormat::Text => builder.init(),
        LogFormat::Json => builder.json().init(),
    }
    Ok(())
}

/// Size of a log file before [`RotatingFile`] moves it aside.
pub const MAX_LOG_FILE: u64 = 16 * 1024 * 1024;

/// A log file that appends to `path` and, once it would pass `max_bytes`,
/// replaces `<path>.1` with it and starts again: at most twice the size on
/// disk, the newest lines kept.
#[derive(Debug)]
pub struct RotatingFile {
    path: PathBuf,
    max_bytes: u64,
    /// The open file and its size; `None` after a failed reopen, retried
    /// on the next write.
    state: Mutex<Option<(File, u64)>>,
}

impl RotatingFile {
    pub fn open(path: &Path, max_bytes: u64) -> io::Result<Self> {
        let file = append(path)?;
        let size = file.metadata()?.len();
        Ok(Self {
            path: path.to_owned(),
            max_bytes,
            state: Mutex::new(Some((file, size))),
        })
    }

    fn rotated(&self) -> PathBuf {
        let mut name = self.path.as_os_str().to_owned();
        name.push(".1");
        PathBuf::from(name)
    }
}

fn append(path: &Path) -> io::Result<File> {
    OpenOptions::new().create(true).append(true).open(path)
}

impl Write for &RotatingFile {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        let full = state
            .as_ref()
            .is_some_and(|(_, size)| *size > 0 && size + buf.len() as u64 > self.max_bytes);
        if full || state.is_none() {
            // Closed first: Windows cannot rename an open file.
            if state.take().is_some() {
                std::fs::rename(&self.path, self.rotated())?;
            }
            let file = append(&self.path)?;
            let size = file.metadata()?.len();
            *state = Some((file, size));
        }
        let (file, size) = state.as_mut().expect("reopened above");
        let written = file.write(buf)?;
        *size += written as u64;
        Ok(written)
    }

    fn flush(&mut self) -> io::Result<()> {
        match self
            .state
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .as_mut()
        {
            Some((file, _)) => file.flush(),
            None => Ok(()),
        }
    }
}

impl<'a> tracing_subscriber::fmt::MakeWriter<'a> for RotatingFile {
    type Writer = &'a RotatingFile;

    fn make_writer(&'a self) -> Self::Writer {
        self
    }
}

/// Whether to emit ANSI colors, given whether the log output is a terminal
/// and the value of `NO_COLOR`.
fn ansi_enabled(is_terminal: bool, no_color: Option<&OsStr>) -> bool {
    is_terminal && no_color.is_none_or(OsStr::is_empty)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn log_levels_map_to_tracing_levels() {
        assert_eq!(tracing::Level::from(LogLevel::Trace), tracing::Level::TRACE);
        assert_eq!(tracing::Level::from(LogLevel::Info), tracing::Level::INFO);
        assert_eq!(tracing::Level::from(LogLevel::Error), tracing::Level::ERROR);
    }

    #[test]
    fn colors_only_on_a_terminal_without_no_color() {
        let set = Some(OsStr::new("1"));
        let empty = Some(OsStr::new(""));
        assert!(ansi_enabled(true, None));
        assert!(ansi_enabled(true, empty), "an empty NO_COLOR is ignored");
        assert!(!ansi_enabled(true, set));
        assert!(!ansi_enabled(false, None));
        assert!(!ansi_enabled(false, set));
    }

    #[test]
    fn log_files_rotate_at_their_size() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("agent.log");
        std::fs::write(&path, "old\n").unwrap();
        let file = RotatingFile::open(&path, 10).unwrap();
        (&file).write_all(b"12345\n").unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "old\n12345\n");
        // Past 10 bytes: the file moves to `.1`, replacing an older one.
        (&file).write_all(b"abc\n").unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "abc\n");
        let rotated = dir.path().join("agent.log.1");
        assert_eq!(std::fs::read_to_string(&rotated).unwrap(), "old\n12345\n");
        (&file).write_all(b"0123456789xyz\n").unwrap();
        (&file).write_all(b"next\n").unwrap();
        assert_eq!(
            std::fs::read_to_string(&rotated).unwrap(),
            "0123456789xyz\n"
        );
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "next\n");
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 2);
    }

    #[test]
    fn subscriber_writes_no_escape_codes_without_ansi() {
        use std::sync::{Arc, Mutex};

        #[derive(Clone, Default)]
        struct Buffer(Arc<Mutex<Vec<u8>>>);
        impl std::io::Write for Buffer {
            fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
                self.0.lock().unwrap().extend_from_slice(buf);
                Ok(buf.len())
            }
            fn flush(&mut self) -> std::io::Result<()> {
                Ok(())
            }
        }

        let render = |ansi: bool| {
            let buffer = Buffer::default();
            let writer = buffer.clone();
            let subscriber = tracing_subscriber::fmt()
                .with_ansi(ansi)
                .with_writer(move || writer.clone())
                .finish();
            tracing::subscriber::with_default(subscriber, || tracing::warn!(key = 1, "hello"));
            let bytes = buffer.0.lock().unwrap().clone();
            String::from_utf8(bytes).unwrap()
        };
        assert!(render(true).contains('\x1b'), "sanity: colors when asked");
        let plain = render(false);
        assert!(
            plain.contains("hello") && !plain.contains('\x1b'),
            "{plain}"
        );
    }
}
