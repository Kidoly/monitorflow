//! Downloads from the upstream's mirror listener over plain HTTP
//! (ADR-0034). Every byte is counted and hashed on the way to disk; a
//! file goes through `.<name>.part` and gets its final name only once it
//! matches what the metadata says.

use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::time::Duration;

use percent_encoding::{utf8_percent_encode, AsciiSet, NON_ALPHANUMERIC};
use reqwest::StatusCode;
use tokio::io::AsyncWriteExt;

use super::meta::{Algorithm, Checksum, Entry};

/// Characters kept as they are in a URL path segment.
const SEGMENT: &AsciiSet = &NON_ALPHANUMERIC
    .remove(b'-')
    .remove(b'.')
    .remove(b'_')
    .remove(b'~');

#[derive(Debug, thiserror::Error)]
pub enum FetchError {
    #[error("GET {url}: {source}")]
    Request {
        url: String,
        #[source]
        source: reqwest::Error,
    },
    #[error("GET {url}: {status}")]
    Status { url: String, status: StatusCode },
    #[error("GET {url}: no progress for {timeout:?}")]
    Timeout { url: String, timeout: Duration },
    #[error("GET {url}: larger than {limit} bytes")]
    TooLarge { url: String, limit: u64 },
    #[error("GET {url}: {got} does not match the metadata's {expected}")]
    Mismatch {
        url: String,
        expected: String,
        got: String,
    },
    #[error("{}: {source}", path.display())]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
}

fn io(path: &Path) -> impl FnOnce(std::io::Error) -> FetchError + '_ {
    move |source| FetchError::Io {
        path: path.to_owned(),
        source,
    }
}

/// What a download must be.
#[derive(Debug, Clone, Copy)]
pub enum Expect<'a> {
    /// Exactly this size and checksum.
    Entry(&'a Entry),
    /// Unknown content of at most this size, hashed with SHA-256.
    AtMost(u64),
}

#[derive(Debug, Clone)]
pub struct Fetcher {
    client: reqwest::Client,
    base: String,
    timeout: Duration,
}

impl Fetcher {
    /// `base` is the upstream's mirror listener, `http://host:7480`;
    /// `timeout` bounds connecting and every wait for more data.
    pub fn new(base: &str, timeout: Duration) -> Result<Self, reqwest::Error> {
        let client = reqwest::Client::builder()
            .connect_timeout(timeout)
            .user_agent(concat!("monitorflow-relay/", env!("CARGO_PKG_VERSION")))
            .build()?;
        Ok(Self {
            client,
            base: base.trim_end_matches('/').to_owned(),
            timeout,
        })
    }

    /// URL of `path` (`/`-separated), each segment percent-encoded.
    pub fn url(&self, path: &str) -> String {
        let mut url = self.base.clone();
        for segment in path.split('/') {
            url.push('/');
            url.extend(utf8_percent_encode(segment, SEGMENT));
        }
        url
    }

    /// The response to a GET of `path`; `None` for 404.
    async fn get(&self, path: &str) -> Result<Option<(String, reqwest::Response)>, FetchError> {
        let url = self.url(path);
        let sent = tokio::time::timeout(self.timeout, self.client.get(&url).send()).await;
        let response = match sent {
            Err(_) => {
                return Err(FetchError::Timeout {
                    url,
                    timeout: self.timeout,
                })
            }
            Ok(Err(source)) => return Err(FetchError::Request { url, source }),
            Ok(Ok(response)) => response,
        };
        match response.status() {
            StatusCode::OK => Ok(Some((url, response))),
            StatusCode::NOT_FOUND => Ok(None),
            status => Err(FetchError::Status { url, status }),
        }
    }

    async fn chunk(
        &self,
        url: &str,
        response: &mut reqwest::Response,
    ) -> Result<Option<impl std::ops::Deref<Target = [u8]>>, FetchError> {
        match tokio::time::timeout(self.timeout, response.chunk()).await {
            Err(_) => Err(FetchError::Timeout {
                url: url.to_owned(),
                timeout: self.timeout,
            }),
            Ok(Err(source)) => Err(FetchError::Request {
                url: url.to_owned(),
                source,
            }),
            Ok(Ok(chunk)) => Ok(chunk),
        }
    }

    /// The body of `path`, at most `limit` bytes; `None` for 404.
    pub async fn bytes(&self, path: &str, limit: u64) -> Result<Option<Vec<u8>>, FetchError> {
        let Some((url, mut response)) = self.get(path).await? else {
            return Ok(None);
        };
        let mut body = Vec::new();
        while let Some(chunk) = self.chunk(&url, &mut response).await? {
            if (body.len() + chunk.len()) as u64 > limit {
                return Err(FetchError::TooLarge { url, limit });
            }
            body.extend_from_slice(&chunk);
        }
        Ok(Some(body))
    }

    /// Downloads `path` to `dest` through `.<name>.part`, checked against
    /// `expect`, and makes it read-only. Returns the size and checksum, or
    /// `None` for 404 (and leaves `dest` alone).
    pub async fn file(
        &self,
        path: &str,
        dest: &Path,
        expect: Expect<'_>,
    ) -> Result<Option<Entry>, FetchError> {
        let Some((url, mut response)) = self.get(path).await? else {
            return Ok(None);
        };
        let (limit, mut hasher) = match expect {
            Expect::Entry(e) => (e.size, e.checksum.algorithm.hasher()),
            Expect::AtMost(limit) => (limit, Algorithm::Sha256.hasher()),
        };
        let part = part_path(dest);
        if let Some(parent) = dest.parent() {
            tokio::fs::create_dir_all(parent)
                .await
                .map_err(io(parent))?;
        }
        let mut file = tokio::fs::File::create(&part).await.map_err(io(&part))?;
        let mut size = 0u64;
        while let Some(chunk) = self.chunk(&url, &mut response).await? {
            size += chunk.len() as u64;
            if size > limit {
                return Err(FetchError::TooLarge { url, limit });
            }
            hasher.update(&chunk);
            file.write_all(&chunk).await.map_err(io(&part))?;
        }
        file.sync_all().await.map_err(io(&part))?;
        drop(file);
        let got = Entry {
            path: path.to_owned(),
            size,
            checksum: hasher.finish(),
        };
        if let Expect::Entry(e) = expect {
            if (got.size, &got.checksum) != (e.size, &e.checksum) {
                let _ = tokio::fs::remove_file(&part).await;
                return Err(FetchError::Mismatch {
                    url,
                    expected: format!("{} bytes {}", e.size, e.checksum),
                    got: format!("{} bytes {}", got.size, got.checksum),
                });
            }
        }
        tokio::fs::set_permissions(&part, std::fs::Permissions::from_mode(0o444))
            .await
            .map_err(io(&part))?;
        tokio::fs::rename(&part, dest).await.map_err(io(dest))?;
        Ok(Some(got))
    }
}

/// `<dir>/.<name>.part`: hidden, and never a name from the metadata,
/// whose segments cannot start with a dot.
pub fn part_path(dest: &Path) -> PathBuf {
    let name = dest.file_name().unwrap_or_default().to_string_lossy();
    dest.with_file_name(format!(".{name}.part"))
}

/// Size and checksum of the file at `path` with `algorithm`. Blocking.
pub fn hash_file(path: &Path, algorithm: Algorithm) -> std::io::Result<(u64, Checksum)> {
    use std::io::Read;
    let mut file = std::fs::File::open(path)?;
    let mut hasher = algorithm.hasher();
    let mut buf = vec![0; 64 * 1024];
    let mut size = 0;
    loop {
        let n = file.read(&mut buf)?;
        if n == 0 {
            return Ok((size, hasher.finish()));
        }
        size += n as u64;
        hasher.update(&buf[..n]);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn urls_encode_each_segment() {
        let f = Fetcher::new("http://up:7480/", Duration::from_secs(1)).unwrap();
        assert_eq!(
            f.url("apt/debian/s1/pool/main/a/a_1.0+b1~x_amd64.deb"),
            "http://up:7480/apt/debian/s1/pool/main/a/a_1.0%2Bb1~x_amd64.deb"
        );
        assert_eq!(
            f.url("dnf/r/s/P/a b#?.rpm"),
            "http://up:7480/dnf/r/s/P/a%20b%23%3F.rpm"
        );
    }

    #[test]
    fn part_files_are_hidden_and_hashing_matches() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("a.deb");
        assert_eq!(part_path(&path), dir.path().join(".a.deb.part"));
        std::fs::write(&path, "abc").unwrap();
        let (size, sum) = hash_file(&path, Algorithm::Sha256).unwrap();
        assert_eq!(size, 3);
        assert!(sum.to_string().starts_with("sha256:ba7816bf"));
    }
}
