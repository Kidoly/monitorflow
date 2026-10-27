//! Agent releases the server keeps for self-updates (ADR-0053): one
//! directory per version under `<mirror_dir>/agent/`, served by the mirror
//! listener at `/agent/<version>/`, and the URLs a `self_update` command
//! lists.
//!
//! Imports check the release signature with the keys built into the
//! server; agents check it again, so the cache only saves them from
//! downloading junk. Listing reads `VERSION` files only.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::str::FromStr;

use monitorflow_common::config::ServerConfig;
use monitorflow_common::proto::v1::SelfUpdate;
use monitorflow_common::release::{self, Platform, PublicKey, ReleaseError, Version};
use prost::Message;

/// Directory under `mirror_dir`, and the URL path of the mirror listener.
pub const DIR: &str = monitorflow_common::mirror::serve::AGENT_DIR;

/// The files a cached release keeps: what agents download.
pub fn kept_files() -> Vec<&'static str> {
    let mut files = vec![release::SUMS, release::SIGNATURE, release::VERSION_FILE];
    files.extend(
        [
            Platform::LinuxX86_64,
            Platform::LinuxAarch64,
            Platform::WindowsX86_64,
        ]
        .map(Platform::agent_asset),
    );
    files
}

#[derive(Debug, thiserror::Error)]
pub enum CacheError {
    #[error(transparent)]
    Release(#[from] ReleaseError),
    #[error("{0}: {1}")]
    Io(String, io::Error),
    #[error("the release has no agent binary")]
    NoBinary,
}

fn io_error(path: &Path) -> impl FnOnce(io::Error) -> CacheError + '_ {
    move |e| CacheError::Io(path.display().to_string(), e)
}

/// `<mirror_dir>/agent`.
#[derive(Debug, Clone)]
pub struct ReleaseCache {
    dir: PathBuf,
}

impl ReleaseCache {
    pub fn in_mirror_dir(mirror_dir: &Path) -> Self {
        Self {
            dir: mirror_dir.join(DIR),
        }
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }

    /// Cached versions, newest first: directories named as a version
    /// whose `VERSION` says the same. Missing directory, no versions.
    pub fn versions(&self) -> io::Result<Vec<Version>> {
        let entries = match fs::read_dir(&self.dir) {
            Ok(entries) => entries,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(e) => return Err(e),
        };
        let mut versions = Vec::new();
        for entry in entries {
            let entry = entry?;
            let Some(name) = entry.file_name().to_str().map(str::to_owned) else {
                continue;
            };
            let Ok(version) = Version::from_str(&name) else {
                continue;
            };
            let file = entry.path().join(release::VERSION_FILE);
            if fs::read_to_string(file).is_ok_and(|text| text == format!("{name}\n")) {
                versions.push(version);
            }
        }
        versions.sort_by(|a, b| b.cmp(a));
        Ok(versions)
    }

    pub fn has(&self, version: &Version) -> bool {
        self.versions().is_ok_and(|v| v.contains(version))
    }

    /// Copies the release in `source` into the cache once its signature
    /// verifies against `keys` and every file kept matches its checksum.
    /// Replaces a cached copy of the same version.
    pub fn import(&self, source: &Path, keys: &[PublicKey]) -> Result<Version, CacheError> {
        let read = |name: &str| {
            let path = source.join(name);
            fs::read(&path).map_err(io_error(&path))
        };
        let verified = release::verify(
            &read(release::SUMS)?,
            &read(release::SIGNATURE)?,
            &read(release::VERSION_FILE)?,
            keys,
        )?;
        let version = verified.version.to_string();
        fs::create_dir_all(&self.dir).map_err(io_error(&self.dir))?;
        // Hidden while incomplete: the listener serves no dot names.
        let staging = self.dir.join(format!(".{version}.import"));
        let _ = fs::remove_dir_all(&staging);
        fs::create_dir(&staging).map_err(io_error(&staging))?;
        let mut binaries = 0;
        for name in kept_files() {
            let path = source.join(name);
            let bytes = match fs::read(&path) {
                Ok(bytes) => bytes,
                // Binaries are optional, the release files are not.
                Err(e)
                    if e.kind() == io::ErrorKind::NotFound
                        && name.starts_with("monitorflow-agent-") =>
                {
                    continue
                }
                Err(e) => return Err(io_error(&path)(e)),
            };
            // The two signed files are checked by the signature itself.
            if name != release::SUMS && name != release::SIGNATURE {
                verified.check(name, &bytes)?;
            }
            if name.starts_with("monitorflow-agent-") {
                binaries += 1;
            }
            let target = staging.join(name);
            fs::write(&target, bytes).map_err(io_error(&target))?;
        }
        if binaries == 0 {
            let _ = fs::remove_dir_all(&staging);
            return Err(CacheError::NoBinary);
        }
        let target = self.dir.join(&version);
        let _ = fs::remove_dir_all(&target);
        fs::rename(&staging, &target).map_err(io_error(&target))?;
        Ok(verified.version)
    }
}

/// The newest of `versions` without a pre-release suffix, else the newest.
pub fn latest(versions: &[Version]) -> Option<&Version> {
    versions
        .iter()
        .filter(|v| v.pre.is_empty())
        .max()
        .or_else(|| versions.iter().max())
}

/// What a `self_update` command needs to say where releases are.
#[derive(Debug, Clone)]
pub struct Sources {
    pub cache: ReleaseCache,
    pub mirror_public_url: Option<String>,
    pub agent_download_url: String,
}

impl Sources {
    pub fn from_config(config: &ServerConfig) -> Self {
        Self {
            cache: ReleaseCache::in_mirror_dir(&config.mirror_dir()),
            mirror_public_url: config.mirror_public_url.clone(),
            agent_download_url: config.agent_download_url.clone(),
        }
    }

    /// The payload of a `self_update` to `version`; `health_timeout_seconds`
    /// is checked by the caller (0 for the agent's default).
    pub fn payload(
        &self,
        version: &Version,
        allow_downgrade: bool,
        health_timeout_seconds: u32,
    ) -> Vec<u8> {
        SelfUpdate {
            version: version.to_string(),
            base_urls: sources(
                version,
                self.cache.has(version),
                self.mirror_public_url.as_deref(),
                &self.agent_download_url,
            ),
            allow_downgrade,
            health_timeout_seconds,
        }
        .encode_to_vec()
    }
}

/// Where the default `agent_download_url` points: the latest release.
const GITHUB_LATEST: &str = "/releases/latest/download";

/// The base URLs a `self_update` to `version` lists, in the order agents
/// try them: the server's cache when it holds the version and agents know
/// the mirror listener's address, then `agent_download_url`. A GitHub URL
/// is pointed at that release; a self-hosted one is used as it is, and the
/// agent skips it if it serves another version.
pub fn sources(
    version: &Version,
    cached: bool,
    mirror_public_url: Option<&str>,
    agent_download_url: &str,
) -> Vec<String> {
    let mut urls = Vec::new();
    if let (true, Some(mirror)) = (cached, mirror_public_url) {
        urls.push(format!("{}/{DIR}/{version}/", mirror.trim_end_matches('/')));
    }
    let download = agent_download_url.trim_end_matches('/');
    urls.push(match download.strip_suffix(GITHUB_LATEST) {
        Some(repo) => format!("{repo}/releases/download/v{version}/"),
        None => format!("{download}/"),
    });
    urls
}

#[cfg(test)]
mod tests {
    use ring::rand::SystemRandom;
    use ring::signature::{Ed25519KeyPair, KeyPair};
    use tempfile::TempDir;

    use super::*;

    fn v(text: &str) -> Version {
        text.parse().unwrap()
    }

    fn hex(bytes: &[u8]) -> String {
        release::sha256(bytes)
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect()
    }

    /// A signed release of `version` in a directory, and its key.
    fn release_dir(version: &str) -> (TempDir, PublicKey) {
        let pkcs8 = Ed25519KeyPair::generate_pkcs8(&SystemRandom::new()).unwrap();
        let pair = Ed25519KeyPair::from_pkcs8(pkcs8.as_ref()).unwrap();
        let mut der = vec![
            0x30, 0x2a, 0x30, 0x05, 0x06, 0x03, 0x2b, 0x65, 0x70, 0x03, 0x21, 0x00,
        ];
        der.extend_from_slice(pair.public_key().as_ref());
        let dir = TempDir::new().unwrap();
        let files: [(&str, Vec<u8>); 3] = [
            ("VERSION", format!("{version}\n").into_bytes()),
            ("monitorflow-agent-linux-x86_64", b"agent".to_vec()),
            ("install.sh", b"#!/bin/sh\n".to_vec()),
        ];
        let mut sums = String::new();
        for (name, bytes) in &files {
            fs::write(dir.path().join(name), bytes).unwrap();
            sums.push_str(&format!("{}  {name}\n", hex(bytes)));
        }
        fs::write(dir.path().join("SHA256SUMS"), &sums).unwrap();
        fs::write(
            dir.path().join("SHA256SUMS.sig"),
            pair.sign(sums.as_bytes()).as_ref(),
        )
        .unwrap();
        (dir, PublicKey::from_spki(&der).unwrap())
    }

    #[test]
    fn imports_verified_releases_and_lists_them_newest_first() {
        let mirror = TempDir::new().unwrap();
        let cache = ReleaseCache::in_mirror_dir(mirror.path());
        assert!(cache.versions().unwrap().is_empty());
        for version in ["0.2.0", "0.10.0", "0.11.0-rc.1"] {
            let (dir, key) = release_dir(version);
            assert_eq!(cache.import(dir.path(), &[key]).unwrap(), v(version));
        }
        assert_eq!(
            cache.versions().unwrap(),
            [v("0.11.0-rc.1"), v("0.10.0"), v("0.2.0")]
        );
        assert_eq!(latest(&cache.versions().unwrap()), Some(&v("0.10.0")));
        assert!(cache.has(&v("0.2.0")));
        // Only what agents need is kept.
        let kept: Vec<_> = fs::read_dir(mirror.path().join("agent/0.2.0"))
            .unwrap()
            .map(|e| e.unwrap().file_name().into_string().unwrap())
            .collect();
        assert_eq!(kept.len(), 4, "{kept:?}");
        assert!(!kept.contains(&"install.sh".to_owned()));
    }

    #[test]
    fn refuses_releases_that_do_not_verify() {
        let mirror = TempDir::new().unwrap();
        let cache = ReleaseCache::in_mirror_dir(mirror.path());
        let (dir, _) = release_dir("0.2.0");
        let (_, other) = release_dir("0.2.0");
        assert!(matches!(
            cache.import(dir.path(), &[other]),
            Err(CacheError::Release(ReleaseError::BadSignature))
        ));
        let (dir, key) = release_dir("0.2.0");
        fs::write(dir.path().join("monitorflow-agent-linux-x86_64"), b"evil").unwrap();
        assert!(matches!(
            cache.import(dir.path(), &[key]),
            Err(CacheError::Release(ReleaseError::Mismatch(_)))
        ));
        assert!(cache.versions().unwrap().is_empty());
        // A directory whose VERSION disagrees with its name is not listed.
        fs::create_dir_all(mirror.path().join("agent/0.3.0")).unwrap();
        fs::write(mirror.path().join("agent/0.3.0/VERSION"), "0.4.0\n").unwrap();
        assert!(cache.versions().unwrap().is_empty());
    }

    #[test]
    fn sources_prefer_the_cache_and_point_github_at_the_version() {
        let github = "https://github.com/Kidoly/monitorflow/releases/latest/download";
        assert_eq!(
            sources(&v("0.3.0"), true, Some("http://mf.example:7480/"), github),
            [
                "http://mf.example:7480/agent/0.3.0/",
                "https://github.com/Kidoly/monitorflow/releases/download/v0.3.0/",
            ]
        );
        assert_eq!(
            sources(
                &v("0.3.0"),
                false,
                Some("http://mf.example:7480"),
                "https://files.example/mf/0.3.0/"
            ),
            ["https://files.example/mf/0.3.0/"]
        );
        assert_eq!(
            sources(&v("0.3.0"), true, None, github).len(),
            1,
            "no mirror URL, no cache URL"
        );
    }
}
