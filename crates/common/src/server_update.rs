//! Server updates (ADR-0056): what the server and the updater container
//! share. `images.json` names the images of a release by digest and is
//! covered by its signature; [`fetch`] downloads and verifies the release
//! files a check or an update needs; [`Request`] and [`Status`] are the
//! files the two exchange in their shared volume, written by
//! [`write_atomic`] and read by [`read_small`].

use std::io::{self, Read, Write};
use std::os::unix::fs::OpenOptionsExt;
use std::path::Path;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::release::{self, PublicKey, Release, ReleaseError, Version};

pub mod backup;

/// The release file naming the images, listed in `SHA256SUMS`.
pub const IMAGES_FILE: &str = "images.json";
/// Files of the shared volume.
pub const REQUEST_FILE: &str = "request.json";
pub const STATUS_FILE: &str = "status.json";

/// Longest `images.json`, `request.json` and `status.json` accepted.
pub const MAX_IMAGES: usize = 16 * 1024;
pub const MAX_STATE_FILE: usize = 256 * 1024;
/// Longest `VERSION` and `SHA256SUMS.sig` downloaded.
const MAX_VERSION_FILE: usize = 128;
const MAX_SIGNATURE: usize = 1024;

/// Where the default `agent_download_url` points: the latest release.
const GITHUB_LATEST: &str = "/releases/latest/download";

#[derive(Debug, thiserror::Error)]
pub enum UpdateError {
    #[error(transparent)]
    Release(#[from] ReleaseError),
    #[error("invalid {IMAGES_FILE}: {0}")]
    BadImages(String),
    #[error("{url}: {reason}")]
    Download { url: String, reason: String },
    #[error("{base} serves {found}, not {expected}")]
    WrongVersion {
        base: String,
        expected: Version,
        found: Version,
    },
}

/// An image reference by digest: `name@sha256:<64 lower-case hex>`, where
/// `name` is a registry path like `ghcr.io/kidoly/monitorflow-server` or
/// `localhost:5000/monitorflow-web`. Tags are refused: a registry can move
/// them, a digest names the content.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(transparent)]
pub struct ImageRef(String);

impl ImageRef {
    pub fn parse(text: &str) -> Result<Self, UpdateError> {
        let bad =
            || UpdateError::BadImages(format!("`{}` is not name@sha256:…", text.escape_debug()));
        let (name, digest) = text.split_once("@sha256:").ok_or_else(bad)?;
        let name_ok = !name.is_empty()
            && name.len() <= 255
            && name
                .bytes()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b"._-/:".contains(&b))
            && name
                .bytes()
                .next()
                .is_some_and(|b| b.is_ascii_alphanumeric())
            && !name.contains("//");
        let digest_ok = digest.len() == 64
            && digest
                .bytes()
                .all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'));
        if !(name_ok && digest_ok) {
            return Err(bad());
        }
        Ok(Self(text.to_owned()))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for ImageRef {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

/// The images of a release.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Images {
    pub server: ImageRef,
    pub web: ImageRef,
    pub updater: ImageRef,
    /// The mirror tools (ADR-0067); not in older releases.
    pub mirror: Option<ImageRef>,
}

/// `images.json` as written by the release workflow. Unknown keys are
/// ignored, so a later release can add an image.
#[derive(Deserialize)]
struct ImagesFile {
    version: String,
    images: ImagesFileImages,
}

#[derive(Deserialize)]
struct ImagesFileImages {
    server: String,
    web: String,
    updater: String,
    mirror: Option<String>,
}

/// Checks `bytes` against its line in the verified `release`, then its
/// version and every reference.
pub fn verify_images(release: &Release, bytes: &[u8]) -> Result<Images, UpdateError> {
    if bytes.len() > MAX_IMAGES {
        return Err(UpdateError::BadImages("too large".into()));
    }
    release.check(IMAGES_FILE, bytes)?;
    let file: ImagesFile =
        serde_json::from_slice(bytes).map_err(|e| UpdateError::BadImages(e.to_string()))?;
    if file.version != release.version.to_string() {
        return Err(UpdateError::BadImages(format!(
            "version {} in a release of {}",
            file.version.escape_debug(),
            release.version
        )));
    }
    Ok(Images {
        server: ImageRef::parse(&file.images.server)?,
        web: ImageRef::parse(&file.images.web)?,
        updater: ImageRef::parse(&file.images.updater)?,
        mirror: file
            .images
            .mirror
            .as_deref()
            .map(ImageRef::parse)
            .transpose()?,
    })
}

/// The source of server releases: `release_url` when set, else
/// `agent_download_url` (ADR-0056).
pub fn source<'a>(release_url: Option<&'a str>, agent_download_url: &'a str) -> &'a str {
    release_url
        .filter(|url| !url.is_empty())
        .unwrap_or(agent_download_url)
}

/// The directory of release files in `source`, with a trailing slash: for
/// GitHub's latest-release URL, that release, or the release of `version`;
/// a self-hosted directory as it is, whatever version it holds.
pub fn release_base(source: &str, version: Option<&Version>) -> String {
    let source = source.trim_end_matches('/');
    match (source.strip_suffix(GITHUB_LATEST), version) {
        (Some(repo), Some(version)) => format!("{repo}/releases/download/v{version}/"),
        _ => format!("{source}/"),
    }
}

/// The page of `version`'s release notes, when `source` is GitHub's.
pub fn release_notes_url(source: &str, version: &Version) -> Option<String> {
    let repo = source.trim_end_matches('/').strip_suffix(GITHUB_LATEST)?;
    Some(format!("{repo}/releases/tag/v{version}"))
}

/// Release files that verified: what a check shows and an update installs.
#[derive(Debug, Clone)]
pub struct Candidate {
    /// Where they came from, with a trailing slash.
    pub base: String,
    pub release: Release,
    pub images: Images,
    pub sums: Vec<u8>,
    pub signature: Vec<u8>,
    pub version_file: Vec<u8>,
    pub images_file: Vec<u8>,
}

impl Candidate {
    /// Verifies files already read, as [`fetch`] does after downloading
    /// them. `expected`, when given, must be their version.
    pub fn verify(
        base: String,
        files: [Vec<u8>; 4],
        keys: &[PublicKey],
        expected: Option<&Version>,
    ) -> Result<Self, UpdateError> {
        let [sums, signature, version_file, images_file] = files;
        let release = release::verify(&sums, &signature, &version_file, keys)?;
        if let Some(expected) = expected.filter(|v| **v != release.version) {
            return Err(UpdateError::WrongVersion {
                base,
                expected: expected.clone(),
                found: release.version,
            });
        }
        let images = verify_images(&release, &images_file)?;
        Ok(Self {
            base,
            release,
            images,
            sums,
            signature,
            version_file,
            images_file,
        })
    }
}

/// Downloads `SHA256SUMS`, its signature, `VERSION` and `images.json`
/// from `base` and verifies them against `keys`.
pub async fn fetch(
    client: &reqwest::Client,
    base: &str,
    keys: &[PublicKey],
    expected: Option<&Version>,
) -> Result<Candidate, UpdateError> {
    let get = |name: &'static str, limit: usize| download(client, format!("{base}{name}"), limit);
    let files = [
        get(release::SUMS, release::MAX_SUMS).await?,
        get(release::SIGNATURE, MAX_SIGNATURE).await?,
        get(release::VERSION_FILE, MAX_VERSION_FILE).await?,
        get(IMAGES_FILE, MAX_IMAGES).await?,
    ];
    Candidate::verify(base.to_owned(), files, keys, expected)
}

/// The body of `url`, refused past `limit` bytes.
pub async fn download(
    client: &reqwest::Client,
    url: String,
    limit: usize,
) -> Result<Vec<u8>, UpdateError> {
    let fail = |reason: String| UpdateError::Download {
        url: url.clone(),
        reason,
    };
    let mut response = client
        .get(&url)
        .send()
        .await
        .map_err(|e| fail(error_chain(&e)))?;
    if !response.status().is_success() {
        return Err(fail(format!("HTTP {}", response.status())));
    }
    let mut body = Vec::new();
    while let Some(chunk) = response.chunk().await.map_err(|e| fail(error_chain(&e)))? {
        if body.len() + chunk.len() > limit {
            return Err(fail(format!("larger than {limit} bytes")));
        }
        body.extend_from_slice(&chunk);
    }
    Ok(body)
}

/// An error and its sources on one line: reqwest's own message rarely
/// says why a connection failed.
pub fn error_chain(error: &dyn std::error::Error) -> String {
    let mut text = error.to_string();
    let mut source = error.source();
    while let Some(cause) = source {
        text.push_str(": ");
        text.push_str(&cause.to_string());
        source = cause.source();
    }
    text
}

/// `request.json`: the server asks the updater to move to `version`.
/// Nothing else in it decides what runs (ADR-0056).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Request {
    /// 1 to 64 of a-z, A-Z, 0-9 and `-`.
    pub id: String,
    pub version: String,
    /// The user who asked, shown in the status.
    pub requested_by: String,
    pub requested_at: DateTime<Utc>,
}

impl Request {
    /// Parses and checks a request; its version must still be verified
    /// against a release.
    pub fn parse(bytes: &[u8]) -> Result<(Self, Version), String> {
        let request: Self = serde_json::from_slice(bytes).map_err(|e| e.to_string())?;
        let id_ok = (1..=64).contains(&request.id.len())
            && request
                .id
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'-');
        if !id_ok {
            return Err("invalid request id".into());
        }
        if request.requested_by.len() > 128 || request.requested_by.chars().any(char::is_control) {
            return Err("invalid requested_by".into());
        }
        let version = request
            .version
            .parse()
            .map_err(|e: ReleaseError| e.to_string())?;
        Ok((request, version))
    }
}

/// `status.json`, written by the updater only.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Status {
    /// The updater's own version.
    pub updater_version: String,
    /// Rewritten every [`HEARTBEAT`] while the updater runs.
    pub heartbeat_at: DateTime<Utc>,
    /// The server running now, when the updater could ask it.
    pub running: Option<Running>,
    /// The last update, running or finished.
    pub update: Option<UpdateRun>,
    /// Backups (ADR-0058).
    #[serde(default)]
    pub backups: backup::Backups,
}

/// How often the updater rewrites its status.
pub const HEARTBEAT: std::time::Duration = std::time::Duration::from_secs(30);

/// The running server: its version, and the images pinned in `.env`
/// (`None`: a local build).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Running {
    pub version: String,
    pub server_image: Option<String>,
    pub web_image: Option<String>,
    pub updater_image: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UpdateRun {
    pub id: String,
    pub from: Option<String>,
    pub to: String,
    pub requested_by: String,
    pub state: UpdateState,
    /// What happened so far, oldest first.
    pub steps: Vec<Step>,
    pub error: Option<String>,
    /// File name of the backup taken first, in the backups volume.
    pub backup: Option<String>,
    /// Whether the new server applied migrations: a rollback then needs the
    /// backup restored.
    pub migrated: bool,
    pub started_at: DateTime<Utc>,
    pub finished_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum UpdateState {
    Verifying,
    Downloading,
    BackingUp,
    Updating,
    Checking,
    CachingAgents,
    RollingBack,
    /// The new version runs.
    Succeeded,
    /// Nothing changed: refused or failed before the containers were
    /// touched.
    Failed,
    /// The previous images run again; see `migrated`.
    RolledBack,
}

impl UpdateState {
    pub fn is_final(self) -> bool {
        matches!(self, Self::Succeeded | Self::Failed | Self::RolledBack)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Step {
    pub at: DateTime<Utc>,
    pub message: String,
}

/// Writes `bytes` to `dir/name` through a new file of a random name,
/// created exclusively with `mode`, synced and renamed over the target:
/// readers see the old or the new content, and a link planted at either
/// name is replaced, never followed.
pub fn write_atomic(dir: &Path, name: &str, bytes: &[u8], mode: u32) -> io::Result<()> {
    let mut random = [0u8; 8];
    ring::rand::SecureRandom::fill(&ring::rand::SystemRandom::new(), &mut random)
        .map_err(|_| io::Error::other("no randomness"))?;
    let suffix: String = random.iter().map(|b| format!("{b:02x}")).collect();
    let temp = dir.join(format!(".{name}.{suffix}.tmp"));
    let result = (|| {
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(mode)
            .open(&temp)?;
        file.write_all(bytes)?;
        file.sync_all()?;
        std::fs::rename(&temp, dir.join(name))?;
        std::fs::File::open(dir)?.sync_all()
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&temp);
    }
    result
}

/// The file at `path`, `None` when missing; never through a link, and
/// refused past `limit` bytes.
pub fn read_small(path: &Path, limit: usize) -> io::Result<Option<Vec<u8>>> {
    let file = match std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(rustix::fs::OFlags::NOFOLLOW.bits() as i32)
        .open(path)
    {
        Ok(file) => file,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e),
    };
    if !file.metadata()?.is_file() {
        return Err(io::Error::other(format!(
            "{} is not a file",
            path.display()
        )));
    }
    let mut bytes = Vec::new();
    file.take(limit as u64 + 1).read_to_end(&mut bytes)?;
    if bytes.len() > limit {
        return Err(io::Error::other(format!("{} is too large", path.display())));
    }
    Ok(Some(bytes))
}

#[cfg(test)]
mod tests;
