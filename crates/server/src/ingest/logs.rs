//! VictoriaLogs JSON line ingestion: one JSON object per line, POST to
//! `/insert/jsonline`, with `peer` and `site` as stream fields.
//!
//! Records hold identifiers and short messages only. Never put a
//! certificate, token, payload or credential-bearing URL in one.

use chrono::{DateTime, SecondsFormat, Utc};
use serde_json::{Map, Value};

use super::http;
use super::writer::{Sink, WriteError};

/// Fields that identify a log stream in VictoriaLogs.
pub const STREAM_FIELDS: &str = "peer,site";

/// One log line.
#[derive(Debug, Clone, PartialEq)]
pub struct LogRecord {
    pub time: DateTime<Utc>,
    pub msg: String,
    /// Extra fields; empty values are left out.
    pub fields: Vec<(&'static str, String)>,
}

impl LogRecord {
    pub fn new(msg: impl Into<String>) -> Self {
        Self {
            time: Utc::now(),
            msg: msg.into(),
            fields: Vec::new(),
        }
    }

    pub fn field(mut self, name: &'static str, value: impl Into<String>) -> Self {
        self.fields.push((name, value.into()));
        self
    }

    /// The JSON object VictoriaLogs ingests, `_time` and `_msg` included.
    pub fn to_json(&self) -> Value {
        let mut object = Map::new();
        object.insert(
            "_time".into(),
            self.time
                .to_rfc3339_opts(SecondsFormat::Millis, true)
                .into(),
        );
        object.insert("_msg".into(), self.msg.clone().into());
        for (name, value) in &self.fields {
            if !value.is_empty() {
                object.insert((*name).into(), value.clone().into());
            }
        }
        Value::Object(object)
    }
}

/// The VictoriaLogs [`Sink`].
#[derive(Debug, Clone)]
pub struct JsonLines {
    client: reqwest::Client,
    url: String,
}

impl JsonLines {
    /// `base_url` is `victorialogs_url`, like `http://localhost:9428`.
    pub fn new(base_url: &str) -> Result<Self, reqwest::Error> {
        Ok(Self {
            client: http::client()?,
            url: format!(
                "{}/insert/jsonline?_stream_fields={STREAM_FIELDS}",
                base_url.trim_end_matches('/')
            ),
        })
    }
}

/// Newline-delimited JSON for `batch`.
pub fn encode(batch: &[LogRecord]) -> String {
    let mut body = String::new();
    for record in batch {
        body.push_str(&record.to_json().to_string());
        body.push('\n');
    }
    body
}

impl Sink for JsonLines {
    type Item = LogRecord;

    fn name(&self) -> &'static str {
        "victorialogs"
    }

    async fn write(&self, batch: &[LogRecord]) -> Result<(), WriteError> {
        let request = self
            .client
            .post(&self.url)
            .header("Content-Type", "application/stream+json")
            .body(encode(batch));
        http::send(request).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn records_become_one_json_object_per_line() {
        let time = DateTime::parse_from_rfc3339("2026-10-19T08:00:00.250Z")
            .unwrap()
            .to_utc();
        let record = LogRecord {
            time,
            ..LogRecord::new("stream opened")
                .field("peer", "web-01")
                .field("relay", "")
        };
        let body = encode(&[record.clone(), record]);
        let lines: Vec<_> = body.lines().collect();
        assert_eq!(lines.len(), 2);
        let value: Value = serde_json::from_str(lines[0]).unwrap();
        assert_eq!(
            value,
            serde_json::json!({
                "_time": "2026-10-19T08:00:00.250Z",
                "_msg": "stream opened",
                "peer": "web-01",
            })
        );
    }

    #[test]
    fn stream_fields_are_in_the_url() {
        let sink = JsonLines::new("http://vl:9428/").unwrap();
        assert_eq!(
            sink.url,
            "http://vl:9428/insert/jsonline?_stream_fields=peer,site"
        );
    }
}
