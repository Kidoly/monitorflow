//! Revocation store (`revocations.json`) and CRL generation.

use std::path::Path;
use std::sync::{MutexGuard, PoisonError};

use rcgen::{CertificateRevocationListParams, RevokedCertParams, SerialNumber};
use rustls::pki_types::CertificateRevocationListDer;
use serde::{Deserialize, Serialize};
use time::OffsetDateTime;
use tracing::info;

use super::{now, store, CaError, CertificateAuthority, Serial, REVOCATIONS_FILE};

/// Why a certificate was revoked (RFC 5280 §5.3.1).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RevocationReason {
    Unspecified,
    KeyCompromise,
    AffiliationChanged,
    Superseded,
    CessationOfOperation,
}

impl From<RevocationReason> for rcgen::RevocationReason {
    fn from(reason: RevocationReason) -> Self {
        match reason {
            RevocationReason::Unspecified => Self::Unspecified,
            RevocationReason::KeyCompromise => Self::KeyCompromise,
            RevocationReason::AffiliationChanged => Self::AffiliationChanged,
            RevocationReason::Superseded => Self::Superseded,
            RevocationReason::CessationOfOperation => Self::CessationOfOperation,
        }
    }
}

/// Content of `revocations.json`.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub(super) struct Revocations {
    /// Number of the last CRL produced; the next one gets this plus one.
    pub crl_number: u64,
    pub revoked: Vec<Revoked>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub(super) struct Revoked {
    pub serial: Serial,
    /// Unix seconds.
    pub revoked_at: i64,
    pub reason: RevocationReason,
}

/// Reads the store; a missing file is an empty store.
pub(super) fn load(path: &Path) -> Result<Revocations, CaError> {
    match std::fs::read(path) {
        Ok(bytes) => serde_json::from_slice(&bytes).map_err(|e| CaError::corrupt(path, e)),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(Revocations::default()),
        Err(error) => Err(CaError::io(path, error)),
    }
}

fn save(path: &Path, revocations: &Revocations) -> Result<(), CaError> {
    let json = serde_json::to_vec_pretty(revocations).map_err(|e| CaError::corrupt(path, e))?;
    store::write_atomic(path, &json, 0o644)
}

impl CertificateAuthority {
    /// Records the revocation of `serial`, which appears in every later
    /// [`Self::crl`]. Revoking a serial twice keeps the first record.
    pub fn revoke(&self, serial: &Serial, reason: RevocationReason) -> Result<(), CaError> {
        let mut state = self.lock();
        if state.revoked.iter().any(|r| &r.serial == serial) {
            return Ok(());
        }
        let mut next = state.clone();
        next.revoked.push(Revoked {
            serial: serial.clone(),
            revoked_at: now().unix_timestamp(),
            reason,
        });
        save(&self.dir.join(REVOCATIONS_FILE), &next)?;
        *state = next;
        info!(%serial, ?reason, "revoked a certificate");
        Ok(())
    }

    /// True if `serial` was revoked with [`Self::revoke`].
    pub fn is_revoked(&self, serial: &Serial) -> bool {
        self.lock().revoked.iter().any(|r| &r.serial == serial)
    }

    /// A new DER CRL listing every revoked serial, valid from now for
    /// [`CaOptions::crl_validity`], with a CRL number one above the last.
    pub fn crl(&self) -> Result<CertificateRevocationListDer<'static>, CaError> {
        let mut state = self.lock();
        let mut next = state.clone();
        next.crl_number += 1;
        // Persist first, so a number is never handed out twice.
        save(&self.dir.join(REVOCATIONS_FILE), &next)?;
        *state = next;

        let this_update = now();
        let next_update = time::Duration::try_from(self.options.crl_validity)
            .ok()
            .and_then(|v| this_update.checked_add(v))
            .ok_or_else(|| CaError::InvalidRequest("CRL validity is too long".into()))?;
        let revoked_certs = state
            .revoked
            .iter()
            .map(|r| RevokedCertParams {
                serial_number: SerialNumber::from_slice(r.serial.as_bytes()),
                revocation_time: OffsetDateTime::from_unix_timestamp(r.revoked_at)
                    .unwrap_or(this_update),
                reason_code: Some(r.reason.into()),
                invalidity_date: None,
            })
            .collect();
        let crl = CertificateRevocationListParams {
            this_update,
            next_update,
            crl_number: SerialNumber::from(state.crl_number),
            issuing_distribution_point: None,
            revoked_certs,
            key_identifier_method: self.issuer.params().key_identifier_method.clone(),
        }
        .signed_by(&self.issuer, &self.key)
        .map_err(CaError::signing)?;
        Ok(crl.der().clone())
    }

    fn lock(&self) -> MutexGuard<'_, Revocations> {
        // Every update is applied whole after a successful save.
        self.revocations
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
    }
}
