//! Agent releases the server keeps (ADR-0053, ADR-0054): one directory per
//! version under `<mirror_dir>/agent/` with every file of the release,
//! served by the mirror listener at `/agent/<version>/` for self-updates
//! and by the API listener, behind the web interface's HTTPS, for
//! installs; and the URLs a `self_update` command lists.
//!
//! Imports check the release signature with the keys built into the
//! server; agents and the installer check it again, so the cache only
//! saves them from downloading junk. Listing reads `VERSION` files only.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use monitorflow_common::config::ServerConfig;
use monitorflow_common::proto::v1::SelfUpdate;
use monitorflow_common::release::{self, Platform, PublicKey, ReleaseError, Version};
use prost::Message;

/// Directory under `mirror_dir`, and the URL path of the mirror listener.
pub const DIR: &str = monitorflow_common::mirror::serve::AGENT_DIR;

/// The Linux installer of a release (ADR-0052).
pub const INSTALLER: &str = "install.sh";
/// The Windows package of a release (ADR-0050).
pub const MSI: &str = "monitorflow-agent-windows-x86_64.msi";

/// The files a cached release keeps: all of a release's (ADR-0052), so
/// the server can serve installs as well as self-updates (ADR-0054).
pub fn kept_files() -> Vec<&'static str> {
    let mut files = vec![release::SUMS, release::SIGNATURE, release::VERSION_FILE];
    files.extend(BINARIES.map(Platform::agent_asset));
    files.extend([
        INSTALLER,
        "monitorflow-agent.service",
        "monitorflow-agent-sudoers",
        MSI,
    ]);
    files
}

const BINARIES: [Platform; 3] = [
    Platform::LinuxX86_64,
    Platform::LinuxAarch64,
    Platform::WindowsX86_64,
];

fn is_binary(name: &str) -> bool {
    BINARIES.iter().any(|p| p.agent_asset() == name)
}

/// A release [`ReleaseCache::import`] kept.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Imported {
    pub version: Version,
    /// Files of [`kept_files`] the source did not have.
    pub missing: Vec<&'static str>,
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

    /// Cached versions, newest first (see [`release::cached_versions`]).
    pub fn versions(&self) -> io::Result<Vec<Version>> {
        release::cached_versions(&self.dir)
    }

    pub fn has(&self, version: &Version) -> bool {
        self.versions().is_ok_and(|v| v.contains(version))
    }

    /// Copies the release in `source` into the cache once its signature
    /// verifies against `keys` and every file kept matches its checksum.
    /// Reads nothing but `source`, so it works offline. Files of the
    /// release may be missing, except the signed ones and at least one
    /// binary. Replaces a cached copy of the same version.
    pub fn import(&self, source: &Path, keys: &[PublicKey]) -> Result<Imported, CacheError> {
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
        let result = self.copy(source, &staging, &verified);
        if result.is_err() {
            let _ = fs::remove_dir_all(&staging);
        }
        let missing = result?;
        let target = self.dir.join(&version);
        let _ = fs::remove_dir_all(&target);
        fs::rename(&staging, &target).map_err(io_error(&target))?;
        Ok(Imported {
            version: verified.version,
            missing,
        })
    }

    /// Copies and checks the files of `verified` into `staging`; returns
    /// the ones `source` lacks.
    fn copy(
        &self,
        source: &Path,
        staging: &Path,
        verified: &release::Release,
    ) -> Result<Vec<&'static str>, CacheError> {
        let signed = [release::SUMS, release::SIGNATURE, release::VERSION_FILE];
        let mut missing = Vec::new();
        for name in kept_files() {
            let path = source.join(name);
            let bytes = match fs::read(&path) {
                Ok(bytes) => bytes,
                // Only the signed files are required.
                Err(e) if e.kind() == io::ErrorKind::NotFound && !signed.contains(&name) => {
                    missing.push(name);
                    continue;
                }
                Err(e) => return Err(io_error(&path)(e)),
            };
            // The two signed files are checked by the signature itself.
            if name != release::SUMS && name != release::SIGNATURE {
                verified.check(name, &bytes)?;
            }
            let target = staging.join(name);
            fs::write(&target, bytes).map_err(io_error(&target))?;
        }
        if missing.iter().filter(|name| is_binary(name)).count() == BINARIES.len() {
            return Err(CacheError::NoBinary);
        }
        Ok(missing)
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
    /// is checked by the caller (0 for the agent's default). `relay_url` is
    /// the mirror listener of the relay the agent connects through, if it
    /// reported one: tried first, since relays copy the server's releases.
    pub fn payload(
        &self,
        version: &Version,
        relay_url: Option<&str>,
        allow_downgrade: bool,
        health_timeout_seconds: u32,
    ) -> Vec<u8> {
        let mut base_urls: Vec<String> = relay_url
            .map(|url| format!("{}/{DIR}/{version}/", url.trim_end_matches('/')))
            .into_iter()
            .collect();
        base_urls.extend(sources(
            version,
            self.cache.has(version),
            self.mirror_public_url.as_deref(),
            &self.agent_download_url,
        ));
        SelfUpdate {
            version: version.to_string(),
            base_urls,
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

    /// A signed release of `version` in a directory, with every file, and
    /// its key.
    fn release_dir(version: &str) -> (TempDir, PublicKey) {
        let pkcs8 = Ed25519KeyPair::generate_pkcs8(&SystemRandom::new()).unwrap();
        let pair = Ed25519KeyPair::from_pkcs8(pkcs8.as_ref()).unwrap();
        let mut der = vec![
            0x30, 0x2a, 0x30, 0x05, 0x06, 0x03, 0x2b, 0x65, 0x70, 0x03, 0x21, 0x00,
        ];
        der.extend_from_slice(pair.public_key().as_ref());
        let dir = TempDir::new().unwrap();
        let mut sums = String::new();
        for name in kept_files() {
            let bytes = match name {
                release::SUMS | release::SIGNATURE => continue,
                release::VERSION_FILE => format!("{version}\n").into_bytes(),
                name => format!("{name} {version}").into_bytes(),
            };
            fs::write(dir.path().join(name), &bytes).unwrap();
            sums.push_str(&format!("{}  {name}\n", hex(&bytes)));
        }
        fs::write(dir.path().join("SHA256SUMS"), &sums).unwrap();
        fs::write(
            dir.path().join("SHA256SUMS.sig"),
            pair.sign(sums.as_bytes()).as_ref(),
        )
        .unwrap();
        (dir, PublicKey::from_spki(&der).unwrap())
    }

    fn names_in(dir: &Path) -> Vec<String> {
        let mut names: Vec<_> = fs::read_dir(dir)
            .unwrap()
            .map(|e| e.unwrap().file_name().into_string().unwrap())
            .collect();
        names.sort();
        names
    }

    #[test]
    fn imports_verified_releases_and_lists_them_newest_first() {
        let mirror = TempDir::new().unwrap();
        let cache = ReleaseCache::in_mirror_dir(mirror.path());
        assert!(cache.versions().unwrap().is_empty());
        for version in ["0.2.0", "0.10.0", "0.11.0-rc.1"] {
            let (dir, key) = release_dir(version);
            let imported = cache.import(dir.path(), &[key]).unwrap();
            assert_eq!(imported.version, v(version));
            assert!(imported.missing.is_empty());
        }
        assert_eq!(
            cache.versions().unwrap(),
            [v("0.11.0-rc.1"), v("0.10.0"), v("0.2.0")]
        );
        assert_eq!(latest(&cache.versions().unwrap()), Some(&v("0.10.0")));
        assert!(cache.has(&v("0.2.0")));
    }

    #[test]
    fn keeps_every_file_of_the_release_and_nothing_else() {
        let mirror = TempDir::new().unwrap();
        let cache = ReleaseCache::in_mirror_dir(mirror.path());
        let (dir, key) = release_dir("0.2.0");
        fs::write(dir.path().join("notes.txt"), "not in the release").unwrap();
        cache.import(dir.path(), &[key]).unwrap();
        let kept = mirror.path().join("agent/0.2.0");
        let mut expected: Vec<_> = kept_files().iter().map(|n| n.to_string()).collect();
        expected.sort();
        assert_eq!(names_in(&kept), expected);
        for name in [INSTALLER, MSI, "monitorflow-agent-sudoers"] {
            assert_eq!(
                fs::read(kept.join(name)).unwrap(),
                fs::read(dir.path().join(name)).unwrap()
            );
        }
        // No staging directory is left behind.
        assert_eq!(names_in(&mirror.path().join("agent")), ["0.2.0"]);
    }

    #[test]
    fn partial_releases_say_what_they_lack() {
        let mirror = TempDir::new().unwrap();
        let cache = ReleaseCache::in_mirror_dir(mirror.path());
        let (dir, key) = release_dir("0.2.0");
        for name in [MSI, INSTALLER] {
            fs::remove_file(dir.path().join(name)).unwrap();
        }
        let imported = cache.import(dir.path(), &[key]).unwrap();
        assert_eq!(imported.missing, [INSTALLER, MSI]);
        // Without any binary there is nothing to serve.
        let (dir, key) = release_dir("0.3.0");
        for platform in BINARIES {
            fs::remove_file(dir.path().join(platform.agent_asset())).unwrap();
        }
        assert!(matches!(
            cache.import(dir.path(), &[key]),
            Err(CacheError::NoBinary)
        ));
        assert_eq!(cache.versions().unwrap(), [v("0.2.0")]);
        assert_eq!(names_in(&mirror.path().join("agent")), ["0.2.0"]);
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
        // Unsigned.
        let (dir, key) = release_dir("0.2.0");
        fs::remove_file(dir.path().join("SHA256SUMS.sig")).unwrap();
        assert!(matches!(
            cache.import(dir.path(), &[key]),
            Err(CacheError::Io(_, e)) if e.kind() == io::ErrorKind::NotFound
        ));
        // Any tampered file, the installer included.
        for name in ["monitorflow-agent-linux-x86_64", INSTALLER, MSI] {
            let (dir, key) = release_dir("0.2.0");
            fs::write(dir.path().join(name), b"evil").unwrap();
            assert!(
                matches!(
                    cache.import(dir.path(), &[key]),
                    Err(CacheError::Release(ReleaseError::Mismatch(n))) if n == name
                ),
                "{name}"
            );
        }
        assert!(cache.versions().unwrap().is_empty());
        assert!(names_in(&mirror.path().join("agent")).is_empty());
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

    #[test]
    fn agents_behind_a_relay_try_it_first() {
        let sources = Sources {
            cache: ReleaseCache::in_mirror_dir(Path::new("/nonexistent")),
            mirror_public_url: Some("http://mf.example:7480".into()),
            agent_download_url: "https://files.example/mf/".into(),
        };
        let payload = sources.payload(&v("0.3.0"), Some("http://relay1:7480/"), true, 60);
        let update = SelfUpdate::decode(payload.as_slice()).unwrap();
        assert_eq!(
            update.base_urls,
            [
                "http://relay1:7480/agent/0.3.0/",
                "https://files.example/mf/"
            ]
        );
        assert!(update.allow_downgrade);
        assert_eq!(update.health_timeout_seconds, 60);
    }
}
