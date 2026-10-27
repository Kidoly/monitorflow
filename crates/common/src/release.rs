//! Signed release files (ADR-0053): the checks every MonitorFlow binary
//! makes before it trusts files of a release, whatever served them.
//!
//! A release directory holds `SHA256SUMS` (`sha256sum` format, one line
//! per file), `SHA256SUMS.sig` (the raw 64-byte Ed25519 signature of
//! `SHA256SUMS`, as `openssl pkeyutl -sign -rawin` writes it) and `VERSION`
//! (the version and a newline, listed in `SHA256SUMS`). [`verify`] checks
//! the signature against the keys built in from `deploy/release-keys.pem`,
//! then reads the version; [`Release::check`] checks a file against its
//! line, and [`check_executable`] that a binary is for this machine.

use std::cmp::Ordering;
use std::collections::BTreeMap;
use std::fmt;
use std::str::FromStr;
use std::sync::OnceLock;

use base64::Engine;
use ring::signature::{UnparsedPublicKey, ED25519};

/// File names in a release directory.
pub const SUMS: &str = "SHA256SUMS";
pub const SIGNATURE: &str = "SHA256SUMS.sig";
pub const VERSION_FILE: &str = "VERSION";

/// Longest `SHA256SUMS` accepted.
pub const MAX_SUMS: usize = 64 * 1024;

/// DER prefix of an Ed25519 SubjectPublicKeyInfo (RFC 8410); the key's 32
/// bytes follow.
const SPKI_PREFIX: [u8; 12] = [
    0x30, 0x2a, 0x30, 0x05, 0x06, 0x03, 0x2b, 0x65, 0x70, 0x03, 0x21, 0x00,
];

/// The keys file this build was made with.
const BUILT_IN_KEYS: &str = include_str!(concat!(env!("OUT_DIR"), "/release-keys.pem"));

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ReleaseError {
    #[error("this build trusts no release signing key")]
    NoKeys,
    #[error("invalid release key: {0}")]
    BadKey(String),
    #[error("{SIGNATURE} does not verify against any release key this build trusts")]
    BadSignature,
    #[error("invalid {SUMS}: {0}")]
    BadSums(String),
    #[error("{0} is not listed in {SUMS}")]
    NotListed(String),
    #[error("checksum mismatch for {0}")]
    Mismatch(String),
    #[error("invalid {VERSION_FILE}: {0}")]
    BadVersion(String),
    #[error("{0}")]
    WrongBinary(String),
    #[error("{target} is older than the running {current}; downgrades must be allowed explicitly")]
    Downgrade { current: Version, target: Version },
}

/// A version `major.minor.patch`, with an optional pre-release suffix
/// (`0.2.0-rc.1`), ordered as in Semantic Versioning 2.0. Build metadata
/// (`+…`) is refused: releases never carry it.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Version {
    pub major: u64,
    pub minor: u64,
    pub patch: u64,
    pub pre: Vec<String>,
}

impl FromStr for Version {
    type Err = ReleaseError;

    fn from_str(text: &str) -> Result<Self, Self::Err> {
        let bad =
            || ReleaseError::BadVersion(format!("`{}` is not a version", text.escape_debug()));
        if text.is_empty() || text.len() > 64 {
            return Err(bad());
        }
        let (core, pre) = match text.split_once('-') {
            Some((core, pre)) => (core, Some(pre)),
            None => (text, None),
        };
        let number = |part: &str| -> Result<u64, ReleaseError> {
            let digits = !part.is_empty() && part.bytes().all(|b| b.is_ascii_digit());
            if !digits || (part.len() > 1 && part.starts_with('0')) {
                return Err(bad());
            }
            part.parse().map_err(|_| bad())
        };
        let parts: Vec<&str> = core.split('.').collect();
        let [major, minor, patch] = parts[..] else {
            return Err(bad());
        };
        let pre = match pre {
            None => Vec::new(),
            Some(pre) => {
                let ids: Vec<String> = pre.split('.').map(str::to_owned).collect();
                let valid = ids.iter().all(|id| {
                    !id.is_empty()
                        && id.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-')
                        && !(id.len() > 1 && id.starts_with('0') && is_numeric(id))
                });
                if !valid {
                    return Err(bad());
                }
                ids
            }
        };
        Ok(Self {
            major: number(major)?,
            minor: number(minor)?,
            patch: number(patch)?,
            pre,
        })
    }
}

fn is_numeric(id: &str) -> bool {
    id.bytes().all(|b| b.is_ascii_digit())
}

impl Ord for Version {
    fn cmp(&self, other: &Self) -> Ordering {
        let core =
            (self.major, self.minor, self.patch).cmp(&(other.major, other.minor, other.patch));
        if core != Ordering::Equal {
            return core;
        }
        // A pre-release sorts before its release.
        match (self.pre.is_empty(), other.pre.is_empty()) {
            (true, true) => return Ordering::Equal,
            (true, false) => return Ordering::Greater,
            (false, true) => return Ordering::Less,
            (false, false) => {}
        }
        for (a, b) in self.pre.iter().zip(&other.pre) {
            let order = match (is_numeric(a), is_numeric(b)) {
                (true, true) => a.len().cmp(&b.len()).then_with(|| a.cmp(b)),
                (true, false) => Ordering::Less,
                (false, true) => Ordering::Greater,
                (false, false) => a.cmp(b),
            };
            if order != Ordering::Equal {
                return order;
            }
        }
        self.pre.len().cmp(&other.pre.len())
    }
}

impl PartialOrd for Version {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl fmt::Display for Version {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}.{}.{}", self.major, self.minor, self.patch)?;
        if !self.pre.is_empty() {
            write!(f, "-{}", self.pre.join("."))?;
        }
        Ok(())
    }
}

/// An Ed25519 public key that signs releases.
#[derive(Clone, PartialEq, Eq)]
pub struct PublicKey([u8; 32]);

impl fmt::Debug for PublicKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let b64 = base64::engine::general_purpose::STANDARD.encode(self.0);
        f.debug_tuple("PublicKey").field(&b64).finish()
    }
}

impl PublicKey {
    /// From the DER of a SubjectPublicKeyInfo.
    pub fn from_spki(der: &[u8]) -> Result<Self, ReleaseError> {
        der.strip_prefix(&SPKI_PREFIX[..])
            .and_then(|key| <[u8; 32]>::try_from(key).ok())
            .map(Self)
            .ok_or_else(|| ReleaseError::BadKey("not an Ed25519 public key".into()))
    }

    fn verifies(&self, message: &[u8], signature: &[u8]) -> bool {
        UnparsedPublicKey::new(&ED25519, self.0)
            .verify(message, signature)
            .is_ok()
    }
}

/// Every `PUBLIC KEY` PEM block of `text`; other text is ignored.
pub fn parse_keys(text: &str) -> Result<Vec<PublicKey>, ReleaseError> {
    const BEGIN: &str = "-----BEGIN PUBLIC KEY-----";
    const END: &str = "-----END PUBLIC KEY-----";
    let mut keys = Vec::new();
    let mut body: Option<String> = None;
    for line in text.lines().map(str::trim) {
        match (&mut body, line) {
            (None, BEGIN) => body = Some(String::new()),
            (None, _) => {}
            (Some(b64), END) => {
                let der = base64::engine::general_purpose::STANDARD
                    .decode(b64.as_bytes())
                    .map_err(|e| ReleaseError::BadKey(e.to_string()))?;
                keys.push(PublicKey::from_spki(&der)?);
                body = None;
            }
            (Some(b64), line) => b64.push_str(line),
        }
    }
    if body.is_some() {
        return Err(ReleaseError::BadKey("unterminated PEM block".into()));
    }
    Ok(keys)
}

/// The keys this build trusts, from `deploy/release-keys.pem` (or the file
/// `MONITORFLOW_RELEASE_KEYS` named at build time).
pub fn trusted_keys() -> Result<&'static [PublicKey], ReleaseError> {
    static KEYS: OnceLock<Result<Vec<PublicKey>, ReleaseError>> = OnceLock::new();
    match KEYS.get_or_init(|| parse_keys(BUILT_IN_KEYS)) {
        Ok(keys) if keys.is_empty() => Err(ReleaseError::NoKeys),
        Ok(keys) => Ok(keys),
        Err(error) => Err(error.clone()),
    }
}

/// The SHA-256 of `bytes`.
pub fn sha256(bytes: &[u8]) -> [u8; 32] {
    let digest = ring::digest::digest(&ring::digest::SHA256, bytes);
    digest.as_ref().try_into().expect("SHA-256 is 32 bytes")
}

/// A release whose `SHA256SUMS` verified: its version and the checksum of
/// every file it lists.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Release {
    pub version: Version,
    sums: BTreeMap<String, [u8; 32]>,
}

impl Release {
    /// The checksum `SHA256SUMS` lists for `name`.
    pub fn digest(&self, name: &str) -> Result<[u8; 32], ReleaseError> {
        self.sums
            .get(name)
            .copied()
            .ok_or_else(|| ReleaseError::NotListed(name.to_owned()))
    }

    /// The names `SHA256SUMS` lists, in order.
    pub fn files(&self) -> impl Iterator<Item = &str> {
        self.sums.keys().map(String::as_str)
    }

    /// Fails unless `actual` is the checksum listed for `name`.
    pub fn check_digest(&self, name: &str, actual: &[u8; 32]) -> Result<(), ReleaseError> {
        if &self.digest(name)? != actual {
            return Err(ReleaseError::Mismatch(name.to_owned()));
        }
        Ok(())
    }

    /// Fails unless `bytes` is the file `name` of this release.
    pub fn check(&self, name: &str, bytes: &[u8]) -> Result<(), ReleaseError> {
        self.check_digest(name, &sha256(bytes))
    }
}

/// Checks `signature` over `sums` against `keys`, then `version_file`
/// against its line, and returns the release. Nothing in `sums` is read
/// before the signature verifies.
pub fn verify(
    sums: &[u8],
    signature: &[u8],
    version_file: &[u8],
    keys: &[PublicKey],
) -> Result<Release, ReleaseError> {
    if keys.is_empty() {
        return Err(ReleaseError::NoKeys);
    }
    if sums.len() > MAX_SUMS || !keys.iter().any(|k| k.verifies(sums, signature)) {
        return Err(ReleaseError::BadSignature);
    }
    let sums = parse_sums(sums)?;
    let listed = sums
        .get(VERSION_FILE)
        .ok_or_else(|| ReleaseError::NotListed(VERSION_FILE.to_owned()))?;
    if listed != &sha256(version_file) {
        return Err(ReleaseError::Mismatch(VERSION_FILE.to_owned()));
    }
    let text = std::str::from_utf8(version_file)
        .ok()
        .and_then(|t| t.strip_suffix('\n'))
        .filter(|t| !t.contains('\n'))
        .ok_or_else(|| ReleaseError::BadVersion("not one line".into()))?;
    Ok(Release {
        version: text.parse()?,
        sums,
    })
}

/// `sha256sum` lines: 64 lower-case hex digits, a space, a space or `*`,
/// and a plain file name.
fn parse_sums(sums: &[u8]) -> Result<BTreeMap<String, [u8; 32]>, ReleaseError> {
    let bad = |why: &str| ReleaseError::BadSums(why.to_owned());
    let text = std::str::from_utf8(sums).map_err(|_| bad("not UTF-8"))?;
    let mut map = BTreeMap::new();
    for line in text.lines().filter(|l| !l.is_empty()) {
        let (hex, rest) = line.split_at_checked(64).ok_or_else(|| bad("short line"))?;
        let name = rest
            .strip_prefix("  ")
            .or_else(|| rest.strip_prefix(" *"))
            .ok_or_else(|| bad("no file name"))?;
        let plain = !name.is_empty()
            && name != "."
            && name != ".."
            && !name.contains(['/', '\\'])
            && !name.chars().any(char::is_control);
        if !plain {
            return Err(bad("file names must be plain names"));
        }
        let mut digest = [0u8; 32];
        for (i, byte) in digest.iter_mut().enumerate() {
            let pair = hex
                .get(2 * i..2 * i + 2)
                .ok_or_else(|| bad("bad checksum"))?;
            let valid = pair.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'));
            *byte = u8::from_str_radix(pair, 16)
                .ok()
                .filter(|_| valid)
                .ok_or_else(|| bad("bad checksum"))?;
        }
        if map.insert(name.to_owned(), digest).is_some() {
            return Err(bad("a file is listed twice"));
        }
    }
    Ok(map)
}

/// Whether moving from `current` to `target` is allowed: never to an older
/// version unless `allow_downgrade`. `Ok(false)` when they are equal,
/// nothing to do.
pub fn check_upgrade(
    current: &Version,
    target: &Version,
    allow_downgrade: bool,
) -> Result<bool, ReleaseError> {
    match target.cmp(current) {
        Ordering::Greater => Ok(true),
        Ordering::Equal => Ok(false),
        Ordering::Less if allow_downgrade => Ok(true),
        Ordering::Less => Err(ReleaseError::Downgrade {
            current: current.clone(),
            target: target.clone(),
        }),
    }
}

/// Releases kept in `dir`, one directory per version (ADR-0053), newest
/// first: directories named as a version whose `VERSION` says the same.
/// Hidden (dot) names, like a copy in progress, are skipped. A missing
/// directory holds none.
pub fn cached_versions(dir: &std::path::Path) -> std::io::Result<Vec<Version>> {
    let entries = match std::fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
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
        let file = entry.path().join(VERSION_FILE);
        if std::fs::read_to_string(file).is_ok_and(|text| text == format!("{name}\n")) {
            versions.push(version);
        }
    }
    versions.sort_by(|a, b| b.cmp(a));
    Ok(versions)
}

/// Platforms releases build for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Platform {
    LinuxX86_64,
    LinuxAarch64,
    WindowsX86_64,
}

impl Platform {
    /// The platform of this build, if releases include it.
    pub fn current() -> Option<Self> {
        if cfg!(all(target_os = "linux", target_arch = "x86_64")) {
            Some(Self::LinuxX86_64)
        } else if cfg!(all(target_os = "linux", target_arch = "aarch64")) {
            Some(Self::LinuxAarch64)
        } else if cfg!(all(windows, target_arch = "x86_64")) {
            Some(Self::WindowsX86_64)
        } else {
            None
        }
    }

    /// The agent's file name in a release (ADR-0052).
    pub fn agent_asset(self) -> &'static str {
        match self {
            Self::LinuxX86_64 => "monitorflow-agent-linux-x86_64",
            Self::LinuxAarch64 => "monitorflow-agent-linux-aarch64",
            Self::WindowsX86_64 => "monitorflow-agent-windows-x86_64.exe",
        }
    }
}

/// Bytes [`check_executable`] needs from the start of a binary.
pub const HEADER_LEN: usize = 4096;

/// Fails unless `head`, the start of a file, is an executable for
/// `platform`: a 64-bit little-endian ELF for its machine, or a PE image
/// for x86-64. Catches a release whose files were swapped between
/// architectures before it reaches the service.
pub fn check_executable(platform: Platform, head: &[u8]) -> Result<(), ReleaseError> {
    let wrong = |what: &str| ReleaseError::WrongBinary(format!("not {what}"));
    match platform {
        Platform::LinuxX86_64 | Platform::LinuxAarch64 => {
            const EM_X86_64: u16 = 62;
            const EM_AARCH64: u16 = 183;
            let (machine, name) = match platform {
                Platform::LinuxX86_64 => (EM_X86_64, "an x86_64 Linux executable"),
                _ => (EM_AARCH64, "an aarch64 Linux executable"),
            };
            let elf64_le = head.len() >= 20 && head.starts_with(b"\x7fELF\x02\x01");
            let e_type = elf64_le.then(|| u16::from_le_bytes([head[16], head[17]]));
            let e_machine = elf64_le.then(|| u16::from_le_bytes([head[18], head[19]]));
            // ET_EXEC or ET_DYN (static PIE).
            if !matches!(e_type, Some(2 | 3)) || e_machine != Some(machine) {
                return Err(wrong(name));
            }
        }
        Platform::WindowsX86_64 => {
            const MACHINE_AMD64: u16 = 0x8664;
            let name = "an x86-64 Windows executable";
            if !head.starts_with(b"MZ") || head.len() < 0x40 {
                return Err(wrong(name));
            }
            let pe = u32::from_le_bytes(head[0x3c..0x40].try_into().expect("4 bytes")) as usize;
            let machine = head
                .get(pe..pe + 6)
                .filter(|h| h.starts_with(b"PE\0\0"))
                .map(|h| u16::from_le_bytes([h[4], h[5]]));
            if machine != Some(MACHINE_AMD64) {
                return Err(wrong(name));
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests;
