//! Snaps (ADR-0140): the installed snaps, their refreshes and snapd's own
//! refresh schedule, read from snapd's REST API as the agent's own user.
//! The API is snapd's documented, versioned interface and answers JSON;
//! the `snap` command's tables are meant for people. Refreshes go through
//! the root helper ([`super::app_update`]).

use std::collections::HashMap;
use std::path::Path;

use monitorflow_common::proto::v1::{PackageManager, PackageScope, ThirdPartyPackage};
use monitorflow_common::third_party::valid_id;
use serde::Deserialize;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

use super::exec::{Tools, READ_TIMEOUT};
use super::PackageError;

/// Largest answer read from snapd; a host with many snaps answers tens of
/// kilobytes.
const MAX_RESPONSE: u64 = 8 * 1024 * 1024;

#[derive(Debug, Clone)]
pub struct Snap {
    tools: Tools,
}

/// snapd's version and automatic refreshes, as `/v2/system-info` tells
/// them: snapd refreshes on its own, and MonitorFlow leaves it so.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct System {
    pub version: String,
    /// `refresh.timer`, like `00:00~24:00/4`, or the legacy
    /// `refresh.schedule`.
    pub timer: String,
    /// Until when automatic refreshes are held, RFC 3339; empty when not.
    pub hold: String,
    /// When snapd last refreshed and next will, RFC 3339.
    pub last: String,
    pub next: String,
}

impl Snap {
    pub fn new(tools: Tools) -> Self {
        Self { tools }
    }

    /// Whether snapd is installed and listening.
    pub fn present(&self) -> bool {
        self.tools.snap.exists() && self.tools.snapd_socket.exists()
    }

    /// The installed snaps, as the root helper also reads them.
    pub async fn installed(&self) -> Result<Vec<Installed>, PackageError> {
        Ok(get(&self.tools.snapd_socket, "/v2/snaps").await?)
    }

    /// The installed snaps with the version their channel offers when a
    /// refresh is available. snapd asks the store for the refreshes.
    pub async fn inventory(&self) -> Result<Vec<ThirdPartyPackage>, PackageError> {
        let socket = &self.tools.snapd_socket;
        let installed = self.installed().await?;
        let refreshes = match get::<Vec<Refresh>>(socket, "/v2/find?select=refresh").await {
            Ok(refreshes) => refreshes,
            Err(SnapdError::Api { kind, .. }) if kind == "snap-not-found" => Vec::new(),
            Err(error) => return Err(error.into()),
        };
        Ok(merge(installed, refreshes))
    }

    /// snapd's version, automatic refresh schedule and hold.
    pub async fn system(&self) -> Result<System, PackageError> {
        let info: SystemInfo = get(&self.tools.snapd_socket, "/v2/system-info").await?;
        let mut refresh = info.refresh;
        if refresh.timer.is_empty() {
            refresh.timer = refresh.schedule.clone();
        }
        Ok(System {
            version: info.version,
            timer: refresh.timer,
            hold: refresh.hold,
            last: refresh.last,
            next: refresh.next,
        })
    }
}

/// A snap of `/v2/snaps`.
#[derive(Debug, Clone, Deserialize)]
pub struct Installed {
    /// The instance name, `name` or `name_key`.
    pub name: String,
    /// The name to show, empty on older snapd.
    #[serde(default)]
    pub title: String,
    #[serde(default)]
    pub version: String,
    #[serde(default, deserialize_with = "revision")]
    pub revision: String,
    /// The channel the snap follows; older snapd only has `channel`.
    #[serde(default, rename = "tracking-channel")]
    pub tracking_channel: String,
    #[serde(default)]
    pub channel: String,
}

/// A snap of `/v2/find?select=refresh`: the revision a refresh would
/// install.
#[derive(Debug, Clone, Deserialize)]
pub struct Refresh {
    pub name: String,
    #[serde(default)]
    pub version: String,
    #[serde(default, deserialize_with = "revision")]
    pub revision: String,
}

/// snapd writes revisions as strings; a number is taken too.
fn revision<'de, D: serde::Deserializer<'de>>(deserializer: D) -> Result<String, D::Error> {
    Ok(match serde_json::Value::deserialize(deserializer)? {
        serde_json::Value::String(text) => text,
        serde_json::Value::Number(number) => number.to_string(),
        _ => String::new(),
    })
}

#[derive(Debug, Deserialize)]
struct SystemInfo {
    #[serde(default)]
    version: String,
    #[serde(default)]
    refresh: RawRefresh,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct RawRefresh {
    timer: String,
    schedule: String,
    hold: String,
    last: String,
    next: String,
}

/// The installed snaps whose names snapd's grammar accepts, each with the
/// version a refresh would bring: the refresh's version, or its revision
/// when the version stays the same.
pub fn merge(installed: Vec<Installed>, refreshes: Vec<Refresh>) -> Vec<ThirdPartyPackage> {
    let refreshes: HashMap<String, Refresh> =
        refreshes.into_iter().map(|r| (r.name.clone(), r)).collect();
    installed
        .into_iter()
        .filter(|snap| valid_id(PackageManager::Snap, &snap.name))
        .map(|snap| {
            let available_version = match refreshes.get(&snap.name) {
                Some(r) if !r.version.is_empty() && r.version != snap.version => r.version.clone(),
                Some(r) => format!("{} (revision {})", r.version, r.revision)
                    .trim()
                    .to_owned(),
                None => String::new(),
            };
            // The channel the snap tracks is where its refreshes come from.
            let origin = if snap.tracking_channel.is_empty() {
                snap.channel
            } else {
                snap.tracking_channel
            };
            ThirdPartyPackage {
                manager: PackageManager::Snap.into(),
                id: snap.name,
                name: snap.title,
                version: snap.version,
                available_version,
                scope: PackageScope::Machine.into(),
                origin,
            }
        })
        .collect()
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum SnapdError {
    #[error("cannot reach snapd: {0}")]
    Io(String),
    #[error("snapd did not answer in time")]
    Timeout,
    #[error("snapd's answer is larger than {MAX_RESPONSE} bytes")]
    TooLarge,
    #[error("snapd's answer is malformed: {0}")]
    Malformed(String),
    #[error("snapd refused: {message} ({kind})")]
    Api { kind: String, message: String },
}

impl From<SnapdError> for PackageError {
    fn from(error: SnapdError) -> Self {
        PackageError::Spawn("snapd".into(), error.to_string())
    }
}

/// snapd's JSON envelope.
#[derive(Debug, Deserialize)]
struct Envelope {
    #[serde(rename = "type")]
    kind: String,
    #[serde(default)]
    result: serde_json::Value,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct ApiError {
    message: String,
    kind: String,
}

/// `GET path` on snapd's socket, within [`READ_TIMEOUT`]: the `result` of
/// a `sync` answer.
pub async fn get<T: serde::de::DeserializeOwned>(
    socket: &Path,
    path: &str,
) -> Result<T, SnapdError> {
    let body = tokio::time::timeout(READ_TIMEOUT, request(socket, path))
        .await
        .map_err(|_| SnapdError::Timeout)??;
    parse_body(&body)
}

/// Reads the `result` of an answer's JSON body.
pub fn parse_body<T: serde::de::DeserializeOwned>(body: &[u8]) -> Result<T, SnapdError> {
    let envelope: Envelope =
        serde_json::from_slice(body).map_err(|e| SnapdError::Malformed(e.to_string()))?;
    if envelope.kind == "error" {
        let error: ApiError = serde_json::from_value(envelope.result).unwrap_or_default();
        return Err(SnapdError::Api {
            kind: error.kind,
            message: error.message,
        });
    }
    if envelope.kind != "sync" {
        return Err(SnapdError::Malformed(format!(
            "unexpected answer type `{}`",
            envelope.kind
        )));
    }
    serde_json::from_value(envelope.result).map_err(|e| SnapdError::Malformed(e.to_string()))
}

/// One HTTP/1.0 request, so snapd closes the connection after a plain,
/// unchunked answer: its body.
async fn request(socket: &Path, path: &str) -> Result<Vec<u8>, SnapdError> {
    let io = |e: std::io::Error| SnapdError::Io(e.to_string());
    let mut stream = tokio::net::UnixStream::connect(socket).await.map_err(io)?;
    let head =
        format!("GET {path} HTTP/1.0\r\nHost: snapd\r\nUser-Agent: monitorflow-agent\r\n\r\n");
    stream.write_all(head.as_bytes()).await.map_err(io)?;
    let mut answer = Vec::new();
    (&mut stream)
        .take(MAX_RESPONSE + 1)
        .read_to_end(&mut answer)
        .await
        .map_err(io)?;
    if answer.len() as u64 > MAX_RESPONSE {
        return Err(SnapdError::TooLarge);
    }
    split_response(answer)
}

/// The body of an HTTP answer, whatever its status: snapd describes its
/// errors in the JSON body.
pub fn split_response(mut answer: Vec<u8>) -> Result<Vec<u8>, SnapdError> {
    let end = answer
        .windows(4)
        .position(|w| w == b"\r\n\r\n")
        .ok_or_else(|| SnapdError::Malformed("no end of headers".into()))?;
    let head = String::from_utf8_lossy(&answer[..end]).to_ascii_lowercase();
    if !head.starts_with("http/1.") {
        return Err(SnapdError::Malformed("not an HTTP answer".into()));
    }
    if head.contains("transfer-encoding: chunked") {
        return Err(SnapdError::Malformed("chunked answer".into()));
    }
    Ok(answer.split_off(end + 4))
}

#[cfg(test)]
mod tests;
