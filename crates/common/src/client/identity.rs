//! The mTLS identity of an agent or relay on disk: load it, or store the
//! one enrollment returned.
//!
//! Each file is written atomically (temporary file, fsync, rename, fsync of
//! the directory), key and CA first and the certificate last. The
//! certificate is the commit marker: without it the peer counts as not
//! enrolled, so a crash in the middle never leaves a mismatched set that
//! looks valid.
//!
//! A renewal (ADR-0010) replaces a key and certificate that are in use, so
//! it writes both next to the live files first, `cert.pem.next` last as its
//! commit marker, then renames them over. [`load`] finishes a renewal that
//! reached its marker and discards one that did not.
//!
//! On Windows the modes become ACLs (ADR-0047): private files and the
//! directories created for them get a protected DACL from
//! [`crate::winacl`], public files inherit their directory's. Windows
//! cannot sync a directory; NTFS journals the rename itself.

use std::fs::{self, DirBuilder, File, OpenOptions};
use std::io::{self, Write};
#[cfg(unix)]
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use crate::config::TlsPaths;
use crate::redact::Secret;
use crate::tls::{self, Fingerprint, Identity, PeerIdentity, PeerRole, TlsError};
use x509_parser::prelude::{FromDer, X509Certificate};

const KEY_MODE: u32 = 0o600;
const PUBLIC_MODE: u32 = 0o644;
#[cfg(unix)]
const DIR_MODE: u32 = 0o700;

#[derive(Debug, thiserror::Error)]
pub enum IdentityError {
    #[error(
        "{}: permission denied; run the service as the user that owns its data directory",
        path.display()
    )]
    PermissionDenied { path: PathBuf },
    #[error("{}: {error}", path.display())]
    Io { path: PathBuf, error: io::Error },
    #[error("invalid identity: {0}; remove the files in the identity directory and enroll again with a new token")]
    Invalid(String),
}

impl IdentityError {
    fn io(path: &Path, error: io::Error) -> Self {
        match error.kind() {
            io::ErrorKind::PermissionDenied => Self::PermissionDenied {
                path: path.to_owned(),
            },
            _ => Self::Io {
                path: path.to_owned(),
                error,
            },
        }
    }
}

impl From<TlsError> for IdentityError {
    fn from(error: TlsError) -> Self {
        match error {
            TlsError::Read { path, error } => Self::io(&path, error),
            other => Self::Invalid(other.to_string()),
        }
    }
}

/// A loaded identity whose certificate names the expected role.
#[derive(Debug, Clone)]
pub struct StoredIdentity {
    pub tls: Identity,
    /// From the certificate's URI SAN; what an agent's `Hello.agent_id`
    /// must say.
    pub id: PeerIdentity,
    /// SHA-256 of the stored CA certificate; a renewal must return it.
    pub ca_fingerprint: Fingerprint,
    pub not_before: SystemTime,
    pub not_after: SystemTime,
}

impl StoredIdentity {
    pub fn peer_id(&self) -> &str {
        self.id.id()
    }

    /// When two thirds of the validity have passed.
    pub fn renewal_at(&self) -> SystemTime {
        let lifetime = self
            .not_after
            .duration_since(self.not_before)
            .unwrap_or_default();
        self.not_before + lifetime.mul_f64(2.0 / 3.0)
    }

    pub fn renewal_due(&self, now: SystemTime) -> bool {
        now >= self.renewal_at()
    }

    pub fn is_expired(&self, now: SystemTime) -> bool {
        now >= self.not_after
    }
}

/// Loads the identity, or `None` when the certificate file does not exist
/// yet (never enrolled, or enrollment interrupted before it finished).
/// Finishes or discards an interrupted renewal first. A certificate for
/// another role than `role` is refused.
pub fn load(paths: &TlsPaths, role: PeerRole) -> Result<Option<StoredIdentity>, IdentityError> {
    recover_renewal(paths)?;
    match paths.cert.try_exists() {
        Ok(true) => {}
        Ok(false) => return Ok(None),
        Err(error) => return Err(IdentityError::io(&paths.cert, error)),
    }
    let tls = Identity::load(paths)?;
    let leaf = tls::load_certs(&paths.cert)?.swap_remove(0);
    let id = PeerIdentity::from_cert(&leaf)?;
    if id.role() != role {
        return Err(IdentityError::Invalid(format!(
            "{} is a {} certificate, not {} one",
            paths.cert.display(),
            id.role(),
            article(role)
        )));
    }
    let (not_before, not_after) = validity(leaf.as_ref())?;
    let ca_fingerprint = tls::fingerprint(&tls::load_certs(&paths.ca_cert)?[0]);
    warn_if_key_is_readable(&paths.key);
    Ok(Some(StoredIdentity {
        tls,
        id,
        ca_fingerprint,
        not_before,
        not_after,
    }))
}

/// "an agent", "a relay", "a server".
pub(crate) fn article(role: PeerRole) -> String {
    match role {
        PeerRole::Agent => format!("an {role}"),
        _ => format!("a {role}"),
    }
}

fn validity(der: &[u8]) -> Result<(SystemTime, SystemTime), IdentityError> {
    let (_, cert) = X509Certificate::from_der(der)
        .map_err(|e| IdentityError::Invalid(format!("cannot parse certificate: {e}")))?;
    let time = |t: x509_parser::time::ASN1Time| {
        UNIX_EPOCH + Duration::from_secs(t.timestamp().max(0) as u64)
    };
    Ok((
        time(cert.validity().not_before),
        time(cert.validity().not_after),
    ))
}

#[cfg(unix)]
fn warn_if_key_is_readable(path: &Path) {
    if let Ok(meta) = fs::metadata(path) {
        if meta.permissions().mode() & 0o077 != 0 {
            tracing::warn!(key = %path.display(), "private key is readable by other users; chmod 600 it");
        }
    }
}

#[cfg(windows)]
fn warn_if_key_is_readable(path: &Path) {
    if let Ok(dacl) = crate::winacl::dacl(path) {
        if !crate::winacl::is_private(&dacl) {
            tracing::warn!(key = %path.display(), %dacl, "private key has another ACL than the agent sets; check who can read it");
        }
    }
}

/// PEM files returned by enrollment, already checked against the token.
#[derive(Debug)]
pub struct Enrolled {
    pub key_pem: Secret,
    pub cert_pem: String,
    pub ca_cert_pem: String,
}

/// Writes the three files, certificate last, each atomically. Missing
/// directories are created with mode 0700.
pub fn store(paths: &TlsPaths, enrolled: &Enrolled) -> Result<(), IdentityError> {
    write_atomic(&paths.key, enrolled.key_pem.expose().as_bytes(), KEY_MODE)?;
    write_atomic(&paths.ca_cert, enrolled.ca_cert_pem.as_bytes(), PUBLIC_MODE)?;
    write_atomic(&paths.cert, enrolled.cert_pem.as_bytes(), PUBLIC_MODE)
}

/// A renewed key and certificate, already checked. The CA does not change.
#[derive(Debug)]
pub struct Renewed {
    pub key_pem: Secret,
    pub cert_pem: String,
}

/// `<path>.next`, where a renewal writes before replacing `path`.
fn next(path: &Path) -> PathBuf {
    let mut next = path.as_os_str().to_owned();
    next.push(".next");
    PathBuf::from(next)
}

/// Replaces the key and certificate so that a crash at any point leaves a
/// matching pair, the old one or the new one (see the module docs).
pub fn store_renewed(paths: &TlsPaths, renewed: &Renewed) -> Result<(), IdentityError> {
    write_atomic(
        &next(&paths.key),
        renewed.key_pem.expose().as_bytes(),
        KEY_MODE,
    )?;
    write_atomic(&next(&paths.cert), renewed.cert_pem.as_bytes(), PUBLIC_MODE)?;
    commit_renewal(paths)
}

/// Renames the written files over the live ones, key first. Either may be
/// gone already when finishing a renewal interrupted between the two.
fn commit_renewal(paths: &TlsPaths) -> Result<(), IdentityError> {
    for path in [&paths.key, &paths.cert] {
        match fs::rename(next(path), path) {
            Ok(()) => sync_parent(path)?,
            Err(e) if e.kind() == io::ErrorKind::NotFound => {}
            Err(e) => return Err(IdentityError::io(path, e)),
        }
    }
    Ok(())
}

fn recover_renewal(paths: &TlsPaths) -> Result<(), IdentityError> {
    let exists = |path: &Path| path.try_exists().map_err(fail(path));
    let (key, cert) = (next(&paths.key), next(&paths.cert));
    if exists(&cert)? {
        tracing::warn!(cert = %paths.cert.display(), "finishing an interrupted certificate renewal");
        commit_renewal(paths)
    } else if exists(&key)? {
        tracing::info!(key = %key.display(), "discarding the key of an interrupted certificate renewal");
        fs::remove_file(&key).map_err(fail(&key))
    } else {
        Ok(())
    }
}

fn parent(path: &Path) -> &Path {
    path.parent()
        .filter(|d| !d.as_os_str().is_empty())
        .unwrap_or(Path::new("."))
}

#[cfg(unix)]
fn sync_parent(path: &Path) -> Result<(), IdentityError> {
    let dir = parent(path);
    File::open(dir)
        .and_then(|d| d.sync_all())
        .map_err(fail(dir))
}

/// Windows has no directory sync; NTFS journals the rename.
#[cfg(windows)]
fn sync_parent(_: &Path) -> Result<(), IdentityError> {
    Ok(())
}

fn fail(path: &Path) -> impl FnOnce(io::Error) -> IdentityError + '_ {
    move |error| IdentityError::io(path, error)
}

/// Replaces `path` with `contents` so that readers see the old file or the
/// new one, never a partial one, even across a power cut.
/// `mode` is a Unix mode; on Windows, one without bits for the group and
/// others makes the file private.
pub fn write_atomic(path: &Path, contents: &[u8], mode: u32) -> Result<(), IdentityError> {
    let dir = parent(path);
    create_dirs(dir)?;
    let name = path
        .file_name()
        .ok_or_else(|| IdentityError::Invalid(format!("{}: not a file path", path.display())))?;
    let tmp = dir.join(format!(".{}.tmp", name.to_string_lossy()));
    // A leftover from a crash may have other permissions: start fresh.
    match fs::remove_file(&tmp) {
        Err(e) if e.kind() != io::ErrorKind::NotFound => return Err(fail(&tmp)(e)),
        _ => {}
    }
    let mut file = create_new(&tmp, mode).map_err(fail(&tmp))?;
    file.write_all(contents).map_err(fail(&tmp))?;
    file.sync_all().map_err(fail(&tmp))?;
    drop(file);
    fs::rename(&tmp, path).map_err(fail(path))?;
    sync_parent(path)
}

#[cfg(unix)]
fn create_dirs(dir: &Path) -> Result<(), IdentityError> {
    DirBuilder::new()
        .recursive(true)
        .mode(DIR_MODE)
        .create(dir)
        .map_err(fail(dir))
}

/// Directories this call creates get the private DACL, like mode 0700.
#[cfg(windows)]
fn create_dirs(dir: &Path) -> Result<(), IdentityError> {
    let mut missing = Vec::new();
    let mut next = Some(dir);
    while let Some(d) = next.filter(|d| !d.as_os_str().is_empty() && !d.exists()) {
        missing.push(d);
        next = d.parent();
    }
    DirBuilder::new()
        .recursive(true)
        .create(dir)
        .map_err(fail(dir))?;
    for d in missing.into_iter().rev() {
        crate::winacl::set_dacl(d, crate::winacl::PRIVATE_DIR).map_err(fail(d))?;
    }
    Ok(())
}

#[cfg(unix)]
fn create_new(path: &Path, mode: u32) -> io::Result<File> {
    let file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(mode)
        .open(path)?;
    // The umask may have narrowed the mode; widen public files back.
    file.set_permissions(fs::Permissions::from_mode(mode))?;
    Ok(file)
}

/// The private DACL is set before anything is written.
#[cfg(windows)]
fn create_new(path: &Path, mode: u32) -> io::Result<File> {
    let file = OpenOptions::new().write(true).create_new(true).open(path)?;
    if mode & 0o077 == 0 {
        crate::winacl::set_dacl(path, crate::winacl::PRIVATE_FILE)?;
    }
    Ok(file)
}

#[cfg(test)]
mod tests {
    use tempfile::TempDir;

    use super::*;

    fn paths(dir: &Path) -> TlsPaths {
        let tls = dir.join("data/tls");
        TlsPaths {
            ca_cert: tls.join("ca.pem"),
            cert: tls.join("cert.pem"),
            key: tls.join("key.pem"),
        }
    }

    #[cfg(unix)]
    fn mode(path: &Path) -> u32 {
        fs::metadata(path).unwrap().permissions().mode() & 0o777
    }

    /// On Windows, 0o600 (0o700 for a directory) for a private DACL and
    /// 0o644 for any other.
    #[cfg(windows)]
    fn mode(path: &Path) -> u32 {
        match crate::winacl::is_private(&crate::winacl::dacl(path).unwrap()) {
            true if path.is_dir() => 0o700,
            true => 0o600,
            false => 0o644,
        }
    }

    #[test]
    fn atomic_write_replaces_and_sets_modes() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("a/b/key.pem");
        write_atomic(&path, b"one", KEY_MODE).unwrap();
        assert_eq!(fs::read(&path).unwrap(), b"one");
        assert_eq!(mode(&path), 0o600);
        assert_eq!(mode(&dir.path().join("a/b")), 0o700);

        // A stale temporary file with loose permissions is not reused.
        let tmp = dir.path().join("a/b/.key.pem.tmp");
        fs::write(&tmp, "stale").unwrap();
        #[cfg(unix)]
        fs::set_permissions(&tmp, fs::Permissions::from_mode(0o666)).unwrap();
        write_atomic(&path, b"two", KEY_MODE).unwrap();
        assert_eq!(fs::read(&path).unwrap(), b"two");
        assert_eq!(mode(&path), 0o600);
        assert!(!tmp.exists());

        let public = dir.path().join("a/b/cert.pem");
        write_atomic(&public, b"cert", PUBLIC_MODE).unwrap();
        assert_eq!(mode(&public), 0o644);
        let names: Vec<_> = fs::read_dir(dir.path().join("a/b"))
            .unwrap()
            .map(|e| e.unwrap().file_name().into_string().unwrap())
            .collect();
        assert_eq!(names.len(), 2, "{names:?}");
    }

    #[test]
    fn missing_certificate_means_not_enrolled() {
        let dir = TempDir::new().unwrap();
        let paths = paths(dir.path());
        assert!(load(&paths, PeerRole::Agent).unwrap().is_none());
        // A key alone (interrupted enrollment) still counts as not enrolled.
        write_atomic(&paths.key, b"key", KEY_MODE).unwrap();
        assert!(load(&paths, PeerRole::Agent).unwrap().is_none());
    }

    #[test]
    fn store_then_load_round_trips() {
        let dir = TempDir::new().unwrap();
        let paths = paths(dir.path());
        let (enrolled, _) = enrolled("web-01", PeerRole::Agent);
        store(&paths, &enrolled).unwrap();
        assert_eq!(mode(&paths.key), 0o600);
        assert_eq!(mode(&paths.cert), 0o644);
        let identity = load(&paths, PeerRole::Agent).unwrap().unwrap();
        assert_eq!(identity.peer_id(), "web-01");
        let now = SystemTime::now();
        assert!(!identity.is_expired(now) && !identity.renewal_due(now));
        assert!(identity.renewal_due(identity.not_after - Duration::from_secs(60)));
        assert!(identity.is_expired(identity.not_after));
    }

    #[test]
    fn renewal_replaces_key_and_certificate() {
        let dir = TempDir::new().unwrap();
        let paths = paths(dir.path());
        let (old, ca) = enrolled("web-01", PeerRole::Agent);
        store(&paths, &old).unwrap();
        let before = load(&paths, PeerRole::Agent).unwrap().unwrap();
        let new = renewed_by(&ca, "web-01");
        store_renewed(&paths, &new).unwrap();
        assert_eq!(fs::read_to_string(&paths.cert).unwrap(), new.cert_pem);
        assert_eq!(
            fs::read_to_string(&paths.key).unwrap(),
            new.key_pem.expose()
        );
        assert_eq!(mode(&paths.key), 0o600);
        let after = load(&paths, PeerRole::Agent).unwrap().unwrap();
        assert_eq!(after.id, before.id);
        assert_eq!(after.ca_fingerprint, before.ca_fingerprint);
        assert!(!next(&paths.key).exists() && !next(&paths.cert).exists());
    }

    #[test]
    fn interrupted_renewals_leave_a_matching_pair() {
        let dir = TempDir::new().unwrap();
        let paths = paths(dir.path());
        let (old, ca) = enrolled("web-01", PeerRole::Agent);
        let new = renewed_by(&ca, "web-01");
        let write_next = |key: bool, cert: bool| {
            store(&paths, &old).unwrap();
            if key {
                write_atomic(&next(&paths.key), new.key_pem.expose().as_bytes(), KEY_MODE).unwrap();
            }
            if cert {
                write_atomic(&next(&paths.cert), new.cert_pem.as_bytes(), PUBLIC_MODE).unwrap();
            }
        };

        // Crash before the marker: the old pair stays.
        write_next(true, false);
        load(&paths, PeerRole::Agent).unwrap().unwrap();
        assert_eq!(fs::read_to_string(&paths.cert).unwrap(), old.cert_pem);
        assert!(!next(&paths.key).exists());

        // Crash after the marker, before any rename: the new pair wins.
        write_next(true, true);
        load(&paths, PeerRole::Agent).unwrap().unwrap();
        assert_eq!(fs::read_to_string(&paths.cert).unwrap(), new.cert_pem);
        assert_eq!(
            fs::read_to_string(&paths.key).unwrap(),
            new.key_pem.expose()
        );

        // Crash between the two renames: the key is new, the cert follows.
        write_next(true, true);
        fs::rename(next(&paths.key), &paths.key).unwrap();
        load(&paths, PeerRole::Agent).unwrap().unwrap();
        assert_eq!(fs::read_to_string(&paths.cert).unwrap(), new.cert_pem);
        assert!(!next(&paths.cert).exists());
    }

    #[test]
    fn invalid_identities_are_refused() {
        let dir = TempDir::new().unwrap();
        let paths = paths(dir.path());
        let (relay, _) = enrolled("r1", PeerRole::Relay);
        store(&paths, &relay).unwrap();
        let err = load(&paths, PeerRole::Agent).unwrap_err().to_string();
        assert!(err.contains("relay certificate, not an agent one"), "{err}");

        // Key from another enrollment.
        let (other, _) = enrolled("web-01", PeerRole::Agent);
        let (agent, _) = enrolled("web-01", PeerRole::Agent);
        store(&paths, &agent).unwrap();
        write_atomic(&paths.key, other.key_pem.expose().as_bytes(), KEY_MODE).unwrap();
        let err = load(&paths, PeerRole::Agent).unwrap_err().to_string();
        assert!(err.contains("does not match"), "{err}");

        fs::write(&paths.cert, "garbage").unwrap();
        assert!(matches!(
            load(&paths, PeerRole::Agent),
            Err(IdentityError::Invalid(_))
        ));
    }

    #[cfg(unix)]
    #[test]
    fn unreadable_key_is_permission_denied() {
        let dir = TempDir::new().unwrap();
        let paths = paths(dir.path());
        store(&paths, &enrolled("web-01", PeerRole::Agent).0).unwrap();
        fs::set_permissions(&paths.key, fs::Permissions::from_mode(0o000)).unwrap();
        // Root reads anything; the check only makes sense for other users.
        if fs::read(&paths.key).is_err() {
            let err = load(&paths, PeerRole::Agent).unwrap_err();
            assert!(
                matches!(err, IdentityError::PermissionDenied { .. }),
                "{err}"
            );
        }
    }

    struct TestCa {
        cert: rcgen::Certificate,
        key: rcgen::KeyPair,
    }

    /// A CA and a certificate for `role`/`id` with a fresh key.
    fn enrolled(id: &str, role: PeerRole) -> (Enrolled, TestCa) {
        use rcgen::{BasicConstraints, CertificateParams, IsCa, KeyPair};
        let key = KeyPair::generate().unwrap();
        let mut params = CertificateParams::default();
        params.is_ca = IsCa::Ca(BasicConstraints::Unconstrained);
        let ca = TestCa {
            cert: params.self_signed(&key).unwrap(),
            key,
        };
        let (key_pem, cert_pem) = issue(&ca, role, id);
        let enrolled = Enrolled {
            key_pem,
            cert_pem,
            ca_cert_pem: ca.cert.pem(),
        };
        (enrolled, ca)
    }

    fn renewed_by(ca: &TestCa, id: &str) -> Renewed {
        let (key_pem, cert_pem) = issue(ca, PeerRole::Agent, id);
        Renewed { key_pem, cert_pem }
    }

    fn issue(ca: &TestCa, role: PeerRole, id: &str) -> (Secret, String) {
        use rcgen::{CertificateParams, KeyPair, SanType};
        let key = KeyPair::generate().unwrap();
        let mut params = CertificateParams::default();
        let uri = PeerIdentity::new(role, id).unwrap().uri();
        params.subject_alt_names = vec![SanType::URI(uri.try_into().unwrap())];
        let cert = params.signed_by(&key, &ca.cert, &ca.key).unwrap();
        (Secret::new(key.serialize_pem()), cert.pem())
    }
}
