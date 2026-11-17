//! What a reader of another tool's configuration implements, and the list
//! of readers the server offers (ADR-0146).

use std::collections::BTreeMap;
use std::fmt;
use std::time::Duration;

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use serde::Serialize;

use crate::model::Inventory;

/// Largest file a reader is given, in bytes.
pub const MAX_FILE_BYTES: usize = 32 * 1024 * 1024;
/// Most files in one upload.
pub const MAX_FILES: usize = 64;
/// Most hosts in one import: a guard against a runaway reader, far above a
/// real fleet of one central server.
pub const MAX_HOSTS: usize = 20_000;

/// An uploaded file.
#[derive(Clone)]
pub struct File {
    /// Its name as uploaded; readers use the extension and match file
    /// names a configuration refers to.
    pub name: String,
    pub content: Vec<u8>,
}

impl fmt::Debug for File {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "File({}, {} bytes)", self.name, self.content.len())
    }
}

/// Values of a connection's fields, by [`Field::key`]. Its `Debug` shows
/// the keys only: a token never reaches a log.
#[derive(Clone, Default)]
pub struct Connection(pub BTreeMap<String, String>);

impl Connection {
    /// The trimmed value of `key`, if not empty.
    pub fn get(&self, key: &str) -> Option<&str> {
        self.0.get(key).map(|v| v.trim()).filter(|v| !v.is_empty())
    }
}

impl fmt::Debug for Connection {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_set().entries(self.0.keys()).finish()
    }
}

/// What a reader reads.
#[derive(Debug, Clone)]
pub enum Input {
    Files(Vec<File>),
    Connect(Connection),
}

/// One field of a connection form.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct Field {
    /// The key in [`Connection`]; the web interface labels it with
    /// `imports.field.<key>` from its catalog.
    pub key: &'static str,
    /// A token or password: typed in a password field, never stored,
    /// never logged.
    pub secret: bool,
    pub optional: bool,
    /// An example value.
    pub placeholder: &'static str,
}

/// How the web interface and the API present a reader.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct SourceInfo {
    /// Stable ID in the API: `zabbix`, `prometheus`.
    pub id: &'static str,
    /// The product's name, shown in the source picker.
    pub name: &'static str,
    /// File extensions an upload accepts, without the dot; empty when it
    /// reads no files.
    pub extensions: &'static [&'static str],
    /// Whether an upload takes several files at once.
    pub many_files: bool,
    /// The connection form; empty when it cannot connect.
    pub connect: Vec<Field>,
}

/// What readers get besides their input.
#[derive(Debug, Clone)]
pub struct Context {
    /// Now, for the next window of a recurring maintenance.
    pub now: DateTime<Utc>,
    /// Timeout of each request of a connection.
    pub timeout: Duration,
}

impl Context {
    pub fn new(now: DateTime<Utc>) -> Self {
        Self {
            now,
            timeout: Duration::from_secs(30),
        }
    }
}

/// Why a reader read nothing. Messages are sentences for the person who
/// uploaded or connected.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ReadError {
    /// A file is not what the reader expected.
    #[error("{file}: {reason}")]
    File { file: String, reason: String },
    /// The input lacks something.
    #[error("{0}")]
    Input(String),
    /// The other tool could not be reached or answered with an error.
    #[error("{0}")]
    Connect(String),
    /// The other tool refused the credentials.
    #[error("{0}")]
    Refused(String),
}

impl ReadError {
    pub fn file(file: &str, reason: impl fmt::Display) -> Self {
        Self::File {
            file: file.to_owned(),
            reason: reason.to_string(),
        }
    }
}

/// A reader of one tool's configuration.
#[async_trait]
pub trait Source: Send + Sync {
    fn info(&self) -> SourceInfo;

    /// Everything the input describes. A reader never writes anything to
    /// the other tool.
    async fn read(&self, input: Input, context: &Context) -> Result<Inventory, ReadError>;
}

/// Every reader, in the order the source picker shows them.
pub fn sources() -> Vec<Box<dyn Source>> {
    vec![
        Box::new(crate::zabbix::Zabbix),
        Box::new(crate::prometheus::Prometheus),
        Box::new(crate::prtg::Prtg),
    ]
}

/// The reader of `id`.
pub fn source(id: &str) -> Option<Box<dyn Source>> {
    sources().into_iter().find(|s| s.info().id == id)
}

/// Checks that apply to every reader's input, before it runs.
pub fn check_input(info: &SourceInfo, input: &Input) -> Result<(), ReadError> {
    match input {
        Input::Files(files) => {
            if info.extensions.is_empty() {
                return Err(ReadError::Input(format!(
                    "{} reads no files: connect to it instead.",
                    info.name
                )));
            }
            if files.is_empty() {
                return Err(ReadError::Input("Choose a file to read.".into()));
            }
            if files.len() > MAX_FILES {
                return Err(ReadError::Input(format!(
                    "At most {MAX_FILES} files at once."
                )));
            }
            if files.len() > 1 && !info.many_files {
                return Err(ReadError::Input(format!(
                    "{} reads one file at a time.",
                    info.name
                )));
            }
            if let Some(file) = files.iter().find(|f| f.content.len() > MAX_FILE_BYTES) {
                return Err(ReadError::file(
                    &file.name,
                    format!("larger than {} MB", MAX_FILE_BYTES / (1024 * 1024)),
                ));
            }
            Ok(())
        }
        Input::Connect(connection) => {
            if info.connect.is_empty() {
                return Err(ReadError::Input(format!(
                    "{} has no API to connect to: upload its files instead.",
                    info.name
                )));
            }
            for field in &info.connect {
                if !field.optional && connection.get(field.key).is_none() {
                    return Err(ReadError::Input(format!("Fill in {}.", field.key)));
                }
            }
            if let Some(key) = connection
                .0
                .keys()
                .find(|k| !info.connect.iter().any(|f| f.key == k.as_str()))
            {
                return Err(ReadError::Input(format!("Unknown field {key}.")));
            }
            Ok(())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_reader_has_a_unique_plain_id() {
        let infos: Vec<_> = sources().iter().map(|s| s.info()).collect();
        let mut ids: Vec<_> = infos.iter().map(|i| i.id).collect();
        ids.sort_unstable();
        ids.dedup();
        assert_eq!(ids.len(), infos.len());
        for info in &infos {
            assert!(info.id.chars().all(|c| c.is_ascii_lowercase()));
            assert!(!info.extensions.is_empty() || !info.connect.is_empty());
            assert_eq!(source(info.id).unwrap().info(), *info);
        }
    }

    #[test]
    fn connections_never_show_their_values() {
        let connection = Connection(BTreeMap::from([("token".into(), "s3cret".into())]));
        assert_eq!(format!("{connection:?}"), "{\"token\"}");
    }

    #[test]
    fn inputs_are_checked_against_the_reader() {
        let info = SourceInfo {
            id: "x",
            name: "X",
            extensions: &["yaml"],
            many_files: false,
            connect: vec![Field {
                key: "url",
                secret: false,
                optional: false,
                placeholder: "",
            }],
        };
        let file = |n: &str| File {
            name: n.into(),
            content: vec![],
        };
        assert!(check_input(&info, &Input::Files(vec![file("a.yaml")])).is_ok());
        assert!(check_input(&info, &Input::Files(vec![])).is_err());
        assert!(check_input(&info, &Input::Files(vec![file("a"), file("b")])).is_err());
        let connect = |pairs: &[(&str, &str)]| {
            Input::Connect(Connection(
                pairs
                    .iter()
                    .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
                    .collect(),
            ))
        };
        assert!(check_input(&info, &connect(&[("url", "http://x")])).is_ok());
        assert!(check_input(&info, &connect(&[("url", " ")])).is_err());
        assert!(check_input(&info, &connect(&[("url", "u"), ("other", "v")])).is_err());
    }
}
