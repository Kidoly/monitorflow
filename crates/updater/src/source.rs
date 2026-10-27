//! Where the updater gets a release (ADR-0056): the configured URL, or a
//! directory for the offline path. Either way the files are verified with
//! the keys built in before anything uses them.

use std::path::{Path, PathBuf};

use async_trait::async_trait;
use monitorflow_common::release::{self, Platform, PublicKey, Release, Version};
use monitorflow_common::server_update::{self, error_chain, Candidate, UpdateError};
use tokio::io::AsyncWriteExt;

/// Largest agent binary downloaded.
const MAX_BINARY: u64 = 256 * 1024 * 1024;

/// The agent executables of a release, for the server's cache.
pub fn agent_assets() -> [&'static str; 3] {
    [
        Platform::LinuxX86_64,
        Platform::LinuxAarch64,
        Platform::WindowsX86_64,
    ]
    .map(Platform::agent_asset)
}

#[async_trait]
pub trait Source: Send + Sync {
    /// The verified release files of `version`.
    async fn candidate(&self, version: &Version) -> Result<Candidate, UpdateError>;
    /// Copies the agent executables of `candidate` that the source has into
    /// `dest`, each checked against `SHA256SUMS`, and returns their names.
    async fn agent_files(
        &self,
        candidate: &Candidate,
        dest: &Path,
    ) -> Result<Vec<&'static str>, UpdateError>;
}

/// The release on `release_url` or `agent_download_url`.
pub struct Http {
    pub client: reqwest::Client,
    pub source: String,
    pub keys: Vec<PublicKey>,
}

#[async_trait]
impl Source for Http {
    async fn candidate(&self, version: &Version) -> Result<Candidate, UpdateError> {
        let base = server_update::release_base(&self.source, Some(version));
        server_update::fetch(&self.client, &base, &self.keys, Some(version)).await
    }

    async fn agent_files(
        &self,
        candidate: &Candidate,
        dest: &Path,
    ) -> Result<Vec<&'static str>, UpdateError> {
        let mut copied = Vec::new();
        for name in agent_assets() {
            if candidate.release.digest(name).is_err() {
                continue;
            }
            let url = format!("{}{name}", candidate.base);
            download_checked(
                &self.client,
                &url,
                &candidate.release,
                name,
                &dest.join(name),
            )
            .await?;
            copied.push(name);
        }
        Ok(copied)
    }
}

/// Streams `url` into `path`, hashing as it writes, and removes the file
/// unless it is `name` of `release`.
async fn download_checked(
    client: &reqwest::Client,
    url: &str,
    release: &Release,
    name: &str,
    path: &Path,
) -> Result<(), UpdateError> {
    let fail = |reason: String| UpdateError::Download {
        url: url.to_owned(),
        reason,
    };
    let result = async {
        let mut response = client
            .get(url)
            .send()
            .await
            .map_err(|e| fail(error_chain(&e)))?;
        if !response.status().is_success() {
            return Err(fail(format!("HTTP {}", response.status())));
        }
        let mut file = tokio::fs::File::create(path)
            .await
            .map_err(|e| fail(e.to_string()))?;
        let mut digest = ring::digest::Context::new(&ring::digest::SHA256);
        let mut size = 0u64;
        while let Some(chunk) = response.chunk().await.map_err(|e| fail(error_chain(&e)))? {
            size += chunk.len() as u64;
            if size > MAX_BINARY {
                return Err(fail(format!("larger than {MAX_BINARY} bytes")));
            }
            digest.update(&chunk);
            file.write_all(&chunk)
                .await
                .map_err(|e| fail(e.to_string()))?;
        }
        file.sync_all().await.map_err(|e| fail(e.to_string()))?;
        let actual: [u8; 32] = digest.finish().as_ref().try_into().expect("32 bytes");
        release.check_digest(name, &actual)?;
        Ok(())
    }
    .await;
    if result.is_err() {
        let _ = tokio::fs::remove_file(path).await;
    }
    result
}

/// A directory of release files, for servers without Internet access.
pub struct Dir {
    pub dir: PathBuf,
    pub keys: Vec<PublicKey>,
}

impl Dir {
    /// The verified release in the directory, whatever its version.
    pub fn read(&self) -> Result<Candidate, UpdateError> {
        let read = |name: &str| {
            let path = self.dir.join(name);
            std::fs::read(&path).map_err(|e| UpdateError::Download {
                url: path.display().to_string(),
                reason: e.to_string(),
            })
        };
        let files = [
            read(release::SUMS)?,
            read(release::SIGNATURE)?,
            read(release::VERSION_FILE)?,
            read(server_update::IMAGES_FILE)?,
        ];
        Candidate::verify(format!("{}/", self.dir.display()), files, &self.keys, None)
    }
}

#[async_trait]
impl Source for Dir {
    async fn candidate(&self, version: &Version) -> Result<Candidate, UpdateError> {
        let candidate = self.read()?;
        if candidate.release.version != *version {
            return Err(UpdateError::WrongVersion {
                base: candidate.base,
                expected: version.clone(),
                found: candidate.release.version,
            });
        }
        Ok(candidate)
    }

    async fn agent_files(
        &self,
        candidate: &Candidate,
        dest: &Path,
    ) -> Result<Vec<&'static str>, UpdateError> {
        let mut copied = Vec::new();
        for name in agent_assets() {
            let Ok(bytes) = std::fs::read(self.dir.join(name)) else {
                continue;
            };
            candidate.release.check(name, &bytes)?;
            std::fs::write(dest.join(name), bytes).map_err(|e| UpdateError::Download {
                url: dest.join(name).display().to_string(),
                reason: e.to_string(),
            })?;
            copied.push(name);
        }
        Ok(copied)
    }
}
