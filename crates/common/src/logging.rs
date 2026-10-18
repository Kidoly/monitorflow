//! Tracing setup shared by the three binaries.

use std::ffi::OsStr;
use std::io::IsTerminal;

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
