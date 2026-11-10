//! What the root helpers trust (ADR-0108). The service's user runs them
//! through sudo, or through the update service's pipe on Windows, and is
//! not believed: a helper acts only on a command the central server
//! signed for this machine, which it checks itself.
//!
//! The service's own files cannot say which CA signs commands, since its
//! user can rewrite them. The helper keeps its own copy, the anchor, in a
//! directory only root writes. It is made once: from the CA whose
//! fingerprint the installer wrote as root from the install command (the
//! pin), or, on a machine installed before pins, from the CAs the service
//! holds at that moment. A record of the command IDs already used, in the
//! same directory, stops a command from being run twice.
//!
//! When the certificate authority changes (ADR-0088), the anchor follows
//! on proof only: the bundles the central server signed for this
//! machine's renewals (ADR-0107), which the service keeps and the helper
//! reads, each accepted only if a CA the anchor already holds signed it.

use std::fs::{self, OpenOptions};
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};

use base64::Engine;
use monitorflow_common::command::{self, SignatureError};
use monitorflow_common::config::TlsPaths;
use monitorflow_common::proto::v1::Command;
use monitorflow_common::tls::{self, Fingerprint, PeerIdentity, PeerRole};
use prost::Message;
use rustls::pki_types::CertificateDer;
use serde::{Deserialize, Serialize};

use crate::commands::journal::valid_id;
use crate::self_update::read_small;

/// Largest request read: the stream refuses commands over 4 MiB.
pub const MAX_REQUEST: usize = 8 * 1024 * 1024;

const ANCHOR_FILE: &str = "trust.json";
const USED_DIR: &str = "used";

/// What the service's user sends a helper, after a 4-byte big-endian
/// length: the command exactly as the central server signed it.
#[derive(Clone, PartialEq, Message)]
pub struct HelperRequest {
    #[prost(message, optional, tag = "1")]
    pub command: Option<Command>,
}

impl HelperRequest {
    pub fn of(command: &Command) -> Self {
        Self {
            command: Some(command.clone()),
        }
    }

    /// The bytes a helper reads first on its stdin.
    pub fn framed(&self) -> Vec<u8> {
        let body = self.encode_to_vec();
        let mut bytes = (body.len() as u32).to_be_bytes().to_vec();
        bytes.extend_from_slice(&body);
        bytes
    }

    /// Reads one framed request, leaving the rest of `input` unread.
    pub fn read(input: &mut impl Read) -> Result<Self, TrustError> {
        let mut len = [0u8; 4];
        input
            .read_exact(&mut len)
            .map_err(|_| TrustError::Input("no signed command on the input".into()))?;
        let len = u32::from_be_bytes(len) as usize;
        if len > MAX_REQUEST {
            return Err(TrustError::Input(format!(
                "the request is larger than {MAX_REQUEST} bytes"
            )));
        }
        let mut body = vec![0u8; len];
        input
            .read_exact(&mut body)
            .map_err(|_| TrustError::Input("the request is cut short".into()))?;
        Self::decode(body.as_slice()).map_err(|e| TrustError::Input(e.to_string()))
    }
}

#[derive(Debug, thiserror::Error)]
pub enum TrustError {
    #[error("invalid input: {0}")]
    Input(String),
    #[error(transparent)]
    Signature(#[from] SignatureError),
    #[error("the command is for {0}, not for this machine")]
    OtherPeer(String),
    #[error("the command is a {found}, and this helper runs {expected}")]
    Kind { found: String, expected: String },
    #[error("the command has expired")]
    Expired,
    #[error("the command was already run")]
    Replayed,
    #[error("cannot tell which certificate authority to trust: {0}")]
    Anchor(String),
    #[error("{0}: {1}")]
    Io(String, io::Error),
}

fn io_error(what: impl AsRef<Path>) -> impl FnOnce(io::Error) -> TrustError {
    let what = what.as_ref().display().to_string();
    move |error| TrustError::Io(what, error)
}

/// Where a helper keeps its anchor, and the service's files it is made
/// from.
#[derive(Debug, Clone)]
pub struct Place {
    /// The role of the service's certificate.
    pub role: PeerRole,
    /// Written by the helper's user only.
    pub state_dir: PathBuf,
    /// The CA fingerprint of the install command, written by the installer.
    pub pin_file: PathBuf,
    /// The service's CA bundle and certificate, which its user can rewrite.
    pub ca_cert: PathBuf,
    pub cert: PathBuf,
}

impl Place {
    /// The agent's on a Linux host.
    #[cfg(unix)]
    pub fn agent(tls: &TlsPaths) -> Self {
        Self::of(
            PeerRole::Agent,
            "/var/lib/monitorflow/helper",
            "/etc/monitorflow/agent-ca.pin",
            tls,
        )
    }

    /// The relay's on a Linux host.
    #[cfg(unix)]
    pub fn relay(tls: &TlsPaths) -> Self {
        Self::of(
            PeerRole::Relay,
            "/var/lib/monitorflow/relay-helper",
            "/etc/monitorflow/relay-ca.pin",
            tls,
        )
    }

    /// The agent's on Windows, for the update service: a folder of its own
    /// under `%ProgramData%\MonitorFlow\update`, which only LocalSystem
    /// and administrators write.
    #[cfg(windows)]
    pub fn agent(tls: &TlsPaths) -> Self {
        let base = monitorflow_common::config::windows_base_dir();
        Self {
            role: PeerRole::Agent,
            state_dir: base.join("update").join("helper"),
            pin_file: base.join("ca.pin"),
            ca_cert: tls.ca_cert.clone(),
            cert: tls.cert.clone(),
        }
    }

    pub fn of(role: PeerRole, state_dir: &str, pin_file: &str, tls: &TlsPaths) -> Self {
        Self {
            role,
            state_dir: state_dir.into(),
            pin_file: pin_file.into(),
            ca_cert: tls.ca_cert.clone(),
            cert: tls.cert.clone(),
        }
    }
}

/// How an anchor was made.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Origin {
    /// From the CA the install command named.
    Pinned,
    /// From the CAs the service held, on a machine installed before pins.
    Adopted,
}

/// The helper's own record of who signs commands for this machine.
/// Unknown fields are kept out of the way, not refused: a later version
/// may add some, and a rollback reads its file.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Anchor {
    /// This machine's ID, as commands name it.
    pub peer_id: String,
    /// The trusted CA certificates, DER in base64.
    pub ca_certs: Vec<String>,
    /// The pin it was made from, in hexadecimal.
    #[serde(default)]
    pub pin: Option<String>,
    pub origin: Origin,
    /// Start of validity of the service's certificate it was made from,
    /// Unix seconds.
    pub cert_not_before: i64,
}

impl Anchor {
    fn certs(&self) -> Vec<CertificateDer<'static>> {
        let engine = base64::engine::general_purpose::STANDARD;
        self.ca_certs
            .iter()
            .filter_map(|cert| engine.decode(cert).ok())
            .map(CertificateDer::from)
            .collect()
    }

    /// From the service's files: the CA of `pin`, or every CA it holds,
    /// and the ID of its certificate, which one of them must have signed.
    fn from_service(place: &Place, pin: Option<Fingerprint>) -> Result<Self, TrustError> {
        let read = |path: &Path| -> Result<Vec<CertificateDer<'static>>, TrustError> {
            let bytes = read_small(path).map_err(io_error(path))?.ok_or_else(|| {
                TrustError::Anchor(format!(
                    "{} is missing; has this machine connected yet?",
                    path.display()
                ))
            })?;
            tls::certs_from_pem(&bytes)
                .filter(|certs| !certs.is_empty())
                .ok_or_else(|| {
                    TrustError::Anchor(format!("{} holds no certificate", path.display()))
                })
        };
        let mut cas = read(&place.ca_cert)?;
        if let Some(pin) = pin {
            cas.retain(|ca| tls::fingerprint(ca) == pin);
            if cas.is_empty() {
                return Err(TrustError::Anchor(format!(
                    "the certificate authority of the install command ({}) is not in {}",
                    tls::fingerprint_hex(&pin),
                    place.ca_cert.display()
                )));
            }
        }
        if cas.len() > 2 {
            return Err(TrustError::Anchor(format!(
                "{} holds more than two certificate authorities",
                place.ca_cert.display()
            )));
        }
        let leaf = read(&place.cert)?.swap_remove(0);
        if !cas.iter().any(|ca| tls::signed_by(&leaf, ca)) {
            return Err(TrustError::Anchor(format!(
                "{} was not issued by a trusted certificate authority",
                place.cert.display()
            )));
        }
        let identity =
            PeerIdentity::from_cert(&leaf).map_err(|e| TrustError::Anchor(e.to_string()))?;
        if identity.role() != place.role {
            return Err(TrustError::Anchor(format!(
                "{} is the certificate of a {}",
                place.cert.display(),
                identity.role()
            )));
        }
        let engine = base64::engine::general_purpose::STANDARD;
        Ok(Self {
            peer_id: identity.id().to_owned(),
            ca_certs: cas.iter().map(|ca| engine.encode(ca)).collect(),
            pin: pin.as_ref().map(tls::fingerprint_hex),
            origin: match pin {
                Some(_) => Origin::Pinned,
                None => Origin::Adopted,
            },
            cert_not_before: tls::not_before_unix(&leaf).unwrap_or_default(),
        })
    }

    /// Moves to the CAs of the signed bundles the service kept from its
    /// renewals, oldest first; true when something changed. The file is
    /// the service user's, so each bundle must prove itself: signed for
    /// its certificate by a CA held here (ADR-0107), that certificate
    /// issued to this machine by a CA of the bundle, and later than the
    /// last one accepted, so that an old bundle cannot bring a retired CA
    /// back. Anything else is passed over.
    fn follow(&mut self, place: &Place) -> bool {
        use monitorflow_common::ca_bundle::{self, SignedBundles};

        let kept = read_small(&ca_bundle::kept_path(&place.ca_cert))
            .ok()
            .flatten()
            .and_then(|bytes| SignedBundles::decode(bytes.as_slice()).ok())
            .unwrap_or_default();
        let engine = base64::engine::general_purpose::STANDARD;
        let mut changed = false;
        for bundle in &kept.bundles {
            let cas = tls::certs_from_pem(bundle.ca_bundle_pem.as_bytes()).unwrap_or_default();
            let leaf = tls::certs_from_pem(bundle.cert_pem.as_bytes())
                .unwrap_or_default()
                .into_iter()
                .next();
            let (Some(leaf), 1..=2) = (leaf, cas.len()) else {
                continue;
            };
            let Some(not_before) = tls::not_before_unix(&leaf) else {
                continue;
            };
            let fingerprints: Vec<Fingerprint> = cas.iter().map(tls::fingerprint).collect();
            let signed = ca_bundle::verify(
                &tls::fingerprint(&leaf),
                &fingerprints,
                &bundle.signatures,
                &self.certs(),
            );
            let ours = PeerIdentity::from_cert(&leaf)
                .is_ok_and(|id| id.role() == place.role && id.id() == self.peer_id);
            let issued = cas.iter().any(|ca| tls::signed_by(&leaf, ca));
            if not_before <= self.cert_not_before || signed.is_err() || !ours || !issued {
                continue;
            }
            let ca_certs: Vec<String> = cas.iter().map(|ca| engine.encode(ca)).collect();
            changed |= ca_certs != self.ca_certs;
            self.ca_certs = ca_certs;
            self.cert_not_before = not_before;
        }
        changed
    }

    /// The stored anchor, made first when there is none, and made again
    /// when the installer wrote another pin since.
    fn load(place: &Place) -> Result<Self, TrustError> {
        let path = place.state_dir.join(ANCHOR_FILE);
        let stored: Option<Self> = match fs::read(&path) {
            Ok(bytes) => Some(
                serde_json::from_slice(&bytes)
                    .map_err(|e| TrustError::Anchor(format!("{}: {e}", path.display())))?,
            ),
            Err(e) if e.kind() == io::ErrorKind::NotFound => None,
            Err(e) => return Err(io_error(&path)(e)),
        };
        let pin = read_pin(&place.pin_file)?;
        let pin_hex = pin.as_ref().map(tls::fingerprint_hex);
        match stored {
            Some(mut anchor) if pin.is_none() || anchor.pin == pin_hex => {
                if anchor.follow(place) {
                    let bytes = serde_json::to_vec_pretty(&anchor).map_err(io::Error::other);
                    bytes
                        .and_then(|bytes| write_synced(&path, &bytes))
                        .map_err(io_error(&path))?;
                }
                Ok(anchor)
            }
            _ => {
                let mut anchor = Self::from_service(place, pin)?;
                anchor.follow(place);
                if anchor.origin == Origin::Adopted {
                    tracing::warn!(
                        peer_id = %anchor.peer_id,
                        "no install command named a certificate authority for this machine: \
                         trusting the ones the service holds now"
                    );
                }
                let bytes = serde_json::to_vec_pretty(&anchor).map_err(io::Error::other);
                bytes
                    .and_then(|bytes| write_synced(&path, &bytes))
                    .map_err(io_error(&path))?;
                Ok(anchor)
            }
        }
    }
}

/// The installer's pin: 64 hexadecimal digits on one line.
fn read_pin(path: &Path) -> Result<Option<Fingerprint>, TrustError> {
    let text = match fs::read_to_string(path) {
        Ok(text) => text,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(io_error(path)(e)),
    };
    parse_pin(text.trim())
        .map(Some)
        .ok_or_else(|| TrustError::Anchor(format!("{} is not a SHA-256", path.display())))
}

/// What [`write_pin`] did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Pinned {
    /// The pin is written; the anchor is made from it at the next request.
    Written,
    /// The machine is connected with another CA than the token's, so the
    /// token is an old one: nothing changed.
    Stale,
}

/// The installer's step, as root: records the CA of the install command's
/// token as the one helpers trust. On a machine that is not connected
/// yet, an anchor left by an earlier install is dropped, as the machine
/// may come back under another name.
pub fn write_pin(place: &Place, token: &tls::EnrollmentToken) -> Result<Pinned, TrustError> {
    let pin = token.ca_fingerprint();
    let enrolled = read_small(&place.cert)
        .map_err(io_error(&place.cert))?
        .is_some();
    if enrolled {
        let held = read_small(&place.ca_cert)
            .map_err(io_error(&place.ca_cert))?
            .and_then(|bytes| tls::certs_from_pem(&bytes))
            .unwrap_or_default();
        if !held.iter().any(|ca| tls::fingerprint(ca) == *pin) {
            return Ok(Pinned::Stale);
        }
    } else {
        let anchor = place.state_dir.join(ANCHOR_FILE);
        match fs::remove_file(&anchor) {
            Err(e) if e.kind() != io::ErrorKind::NotFound => return Err(io_error(&anchor)(e)),
            _ => {}
        }
    }
    let line = format!("{}\n", tls::fingerprint_hex(pin));
    write_synced(&place.pin_file, line.as_bytes()).map_err(io_error(&place.pin_file))?;
    Ok(Pinned::Written)
}

/// Writes the pin from `token` when there is none yet. The update service
/// on Windows has no installer step of its own: it reads the token the
/// MSI wrote, as LocalSystem, in `agent.toml`, which the agent's account
/// cannot write. A pin already there is never replaced from here, so the
/// agent's account cannot have the anchor made again by removing its own
/// certificate.
pub fn pin_once(place: &Place, token: Option<&str>) {
    if place.pin_file.exists() {
        return;
    }
    let Some(Ok(token)) = token.map(tls::EnrollmentToken::parse) else {
        return;
    };
    if let Err(error) = write_pin(place, &token) {
        tracing::warn!(%error, "cannot record the certificate authority of the install command");
    }
}

/// A SHA-256 in hexadecimal.
pub fn parse_pin(hex: &str) -> Option<Fingerprint> {
    if hex.len() != 64 || !hex.is_ascii() {
        return None;
    }
    let mut pin = [0u8; 32];
    for (byte, pair) in pin.iter_mut().zip(hex.as_bytes().chunks(2)) {
        *byte = u8::from_str_radix(std::str::from_utf8(pair).ok()?, 16).ok()?;
    }
    Some(pin)
}

/// Writes `bytes` to a synced temporary file renamed over `path`.
fn write_synced(path: &Path, bytes: &[u8]) -> io::Result<()> {
    let tmp = path.with_extension("tmp");
    let mut file = fs::File::create(&tmp)?;
    file.write_all(bytes)?;
    file.sync_all()?;
    fs::rename(&tmp, path)
}

/// `dir` exists, belongs to the helper's user and nobody else can write
/// it: what it holds decides which commands run as root.
fn prepare_dir(dir: &Path) -> Result<(), TrustError> {
    fs::create_dir_all(dir).map_err(io_error(dir))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        let meta = fs::symlink_metadata(dir).map_err(io_error(dir))?;
        let ours = meta.uid() == rustix::process::geteuid().as_raw();
        if !meta.is_dir() || !ours || meta.mode() & 0o022 != 0 {
            return Err(TrustError::Anchor(format!(
                "{} must be a directory of the helper's user, writable by it only",
                dir.display()
            )));
        }
    }
    // The folder above gives the agent's account read access, which is
    // inherited; here nothing is: LocalSystem and administrators only.
    #[cfg(windows)]
    monitorflow_common::winacl::set_dacl(dir, "D:P(A;OICI;FA;;;SY)(A;OICI;FA;;;BA)")
        .map_err(io_error(dir))?;
    Ok(())
}

/// Held while a request is checked, so two helpers cannot both take one
/// command. The update service on Windows runs one request at a time.
#[cfg(unix)]
fn lock(dir: &Path) -> Result<fs::File, TrustError> {
    let path = dir.join("lock");
    let file = OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(&path)
        .map_err(io_error(&path))?;
    rustix::fs::flock(&file, rustix::fs::FlockOperation::LockExclusive)
        .map_err(|e| io_error(&path)(e.into()))?;
    Ok(file)
}

/// Records `command` as used, refusing one already recorded, and forgets
/// those that expired: the expiry refuses them from then on.
fn mark_used(
    dir: &Path,
    command: &Command,
    step: &str,
    now_unix_ms: i64,
) -> Result<(), TrustError> {
    let used = dir.join(USED_DIR);
    prepare_dir(&used)?;
    for entry in fs::read_dir(&used).map_err(io_error(&used))?.flatten() {
        let expiry = fs::read_to_string(entry.path())
            .ok()
            .and_then(|text| text.trim().parse::<i64>().ok());
        if expiry.is_some_and(|expiry| expiry <= now_unix_ms) {
            let _ = fs::remove_file(entry.path());
        }
    }
    // IDs hold no dot (`valid_id`), so a step never reads as another ID.
    let path = match step {
        "" => used.join(&command.command_id),
        step => used.join(format!("{}.{step}", command.command_id)),
    };
    let mut file = match OpenOptions::new().write(true).create_new(true).open(&path) {
        Ok(file) => file,
        Err(e) if e.kind() == io::ErrorKind::AlreadyExists => return Err(TrustError::Replayed),
        Err(e) => return Err(io_error(&path)(e)),
    };
    file.write_all(command.expires_at_unix_ms.to_string().as_bytes())
        .and_then(|()| file.sync_all())
        .map_err(io_error(&path))
}

/// Checks that `request` holds a command of `kind` the central server
/// signed for this machine, not expired and not run before, and records
/// it as run. The helper then acts on the returned command's payload and
/// on nothing else it was given.
pub fn authorize(
    place: &Place,
    request: &HelperRequest,
    kind: &str,
    now_unix_ms: i64,
) -> Result<Command, TrustError> {
    authorize_step(place, request, kind, "", now_unix_ms)
}

/// [`authorize`] for a command done in several privileged steps, each of
/// which runs once: on Windows an `apply_updates` installs, then restarts.
pub fn authorize_step(
    place: &Place,
    request: &HelperRequest,
    kind: &str,
    step: &str,
    now_unix_ms: i64,
) -> Result<Command, TrustError> {
    let Some(command) = request.command.clone() else {
        return Err(TrustError::Input("the request holds no command".into()));
    };
    if !valid_id(&command.command_id) {
        return Err(TrustError::Input("invalid command ID".into()));
    }
    prepare_dir(&place.state_dir)?;
    #[cfg(unix)]
    let _lock = lock(&place.state_dir)?;
    let anchor = Anchor::load(place)?;
    command::verify(&command, &anchor.certs())?;
    if command.agent_id != anchor.peer_id {
        return Err(TrustError::OtherPeer(command.agent_id));
    }
    if command.kind != kind {
        return Err(TrustError::Kind {
            found: command.kind,
            expected: kind.to_owned(),
        });
    }
    if now_unix_ms >= command.expires_at_unix_ms {
        return Err(TrustError::Expired);
    }
    mark_used(&place.state_dir, &command, step, now_unix_ms)?;
    Ok(command)
}

#[cfg(test)]
pub(crate) mod tests;
