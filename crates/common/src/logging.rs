//! Tracing setup shared by the three binaries.

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
pub fn init(log: &LogConfig) {
    let builder = tracing_subscriber::fmt().with_max_level(tracing::Level::from(log.level));
    match log.format {
        LogFormat::Text => builder.init(),
        LogFormat::Json => builder.json().init(),
    }
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
}
