//! What the repository metadata says a snapshot holds (ADR-0034): each
//! file's path, size and checksum, read from the signed apt `Release` or
//! dnf `repomd.xml` and the package lists they cover.

use std::fmt;
use std::str::FromStr;

use ring::digest;
use serde::{Deserialize, Serialize};

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum MetaError {
    #[error("{file}: {reason}")]
    Invalid { file: String, reason: String },
    #[error("{file}: unsafe path `{path}`")]
    UnsafePath { file: String, path: String },
}

impl MetaError {
    pub fn invalid(file: &str, reason: impl Into<String>) -> Self {
        Self::Invalid {
            file: file.to_owned(),
            reason: reason.into(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Algorithm {
    Sha256,
    Sha512,
}

impl Algorithm {
    /// The name dnf metadata uses (`sha256`); apt's `SHA256` field is
    /// parsed separately.
    pub fn from_name(name: &str) -> Option<Self> {
        match name {
            "sha256" => Some(Self::Sha256),
            "sha512" => Some(Self::Sha512),
            _ => None,
        }
    }

    fn name(self) -> &'static str {
        match self {
            Self::Sha256 => "sha256",
            Self::Sha512 => "sha512",
        }
    }

    pub fn hasher(self) -> Hasher {
        Hasher(digest::Context::new(match self {
            Self::Sha256 => &digest::SHA256,
            Self::Sha512 => &digest::SHA512,
        }))
    }
}

/// A file's digest, written `sha256:<lowercase hex>`.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct Checksum {
    pub algorithm: Algorithm,
    hex: String,
}

impl Checksum {
    /// `hex` must have the digest's length; case is ignored.
    pub fn new(algorithm: Algorithm, hex: &str) -> Option<Self> {
        let len = match algorithm {
            Algorithm::Sha256 => 64,
            Algorithm::Sha512 => 128,
        };
        (hex.len() == len && hex.bytes().all(|b| b.is_ascii_hexdigit())).then(|| Self {
            algorithm,
            hex: hex.to_ascii_lowercase(),
        })
    }
}

impl fmt::Display for Checksum {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}:{}", self.algorithm.name(), self.hex)
    }
}

impl FromStr for Checksum {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let (name, hex) = s.split_once(':').ok_or("missing algorithm")?;
        let algorithm = Algorithm::from_name(name).ok_or("unknown algorithm")?;
        Self::new(algorithm, hex).ok_or_else(|| "bad digest".into())
    }
}

impl TryFrom<String> for Checksum {
    type Error = String;

    fn try_from(s: String) -> Result<Self, Self::Error> {
        s.parse()
    }
}

impl From<Checksum> for String {
    fn from(c: Checksum) -> Self {
        c.to_string()
    }
}

/// Incremental digest of a download.
pub struct Hasher(digest::Context);

impl Hasher {
    pub fn update(&mut self, data: &[u8]) {
        self.0.update(data)
    }

    pub fn finish(self) -> Checksum {
        let algorithm = if self.0.algorithm() == &digest::SHA512 {
            Algorithm::Sha512
        } else {
            Algorithm::Sha256
        };
        let hex = self
            .0
            .finish()
            .as_ref()
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect::<String>();
        Checksum { algorithm, hex }
    }
}

/// One file of a snapshot, as its metadata describes it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Entry {
    /// Relative to the snapshot root, `/`-separated.
    pub path: String,
    pub size: u64,
    pub checksum: Checksum,
}

/// A relative path whose segments are all plain names: not empty, not
/// starting with a dot (which also rules out `..` and the staging and
/// `.part` names), without backslashes or NUL.
pub fn is_safe_path(path: &str) -> bool {
    !path.is_empty()
        && path
            .split('/')
            .all(|s| !s.is_empty() && !s.starts_with('.') && !s.contains(['\\', '\0']))
}

/// `prefix/path`, checked with [`is_safe_path`].
pub fn join(file: &str, prefix: &str, path: &str) -> Result<String, MetaError> {
    let joined = if prefix.is_empty() {
        path.to_owned()
    } else {
        format!("{prefix}/{path}")
    };
    if is_safe_path(&joined) {
        Ok(joined)
    } else {
        Err(MetaError::UnsafePath {
            file: file.to_owned(),
            path: joined,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn paths() {
        for ok in [
            "a",
            "pool/main/a/a_1.0+b1~x_amd64.deb",
            "repodata/x-primary.xml.gz",
        ] {
            assert!(is_safe_path(ok), "{ok}");
        }
        for bad in [
            "",
            "/etc/passwd",
            "a//b",
            "../x",
            "a/../b",
            "a/.part",
            "a\\b",
            "a/",
        ] {
            assert!(!is_safe_path(bad), "{bad}");
        }
        assert_eq!(join("f", "dists/x", "Release").unwrap(), "dists/x/Release");
        assert!(matches!(
            join("f", "dists/x", "../../etc"),
            Err(MetaError::UnsafePath { .. })
        ));
    }

    #[test]
    fn checksums_hash_and_round_trip() {
        let mut hasher = Algorithm::Sha256.hasher();
        hasher.update(b"ab");
        hasher.update(b"c");
        let sum = hasher.finish();
        assert_eq!(
            sum.to_string(),
            "sha256:ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
        assert_eq!(sum.to_string().parse::<Checksum>().unwrap(), sum);
        let upper = Checksum::new(Algorithm::Sha256, &sum.hex.to_uppercase()).unwrap();
        assert_eq!(upper, sum);
        let mut hasher = Algorithm::Sha512.hasher();
        hasher.update(b"abc");
        assert_eq!(hasher.finish().algorithm, Algorithm::Sha512);
        assert!(Checksum::new(Algorithm::Sha256, "abc").is_none());
        assert!(Checksum::new(Algorithm::Sha256, &"g".repeat(64)).is_none());
        assert!("md5:00".parse::<Checksum>().is_err());
    }
}
