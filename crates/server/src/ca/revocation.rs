//! Revocation store (`revocations.json`) and CRL generation.

use std::path::Path;
use std::sync::{MutexGuard, PoisonError};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

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
    /// Unix seconds: when the certificate expires. Absent in records made
    /// before ADR-0113; [`CaOptions::max_cert_validity`] after `revoked_at`
    /// bounds it.
    ///
    /// [`CaOptions::max_cert_validity`]: super::CaOptions::max_cert_validity
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub not_after: Option<i64>,
}

impl Revoked {
    /// Unix seconds after which the serial is left out and forgotten.
    fn listed_until(&self, max_validity: Duration, margin: Duration) -> i64 {
        let expires = self
            .not_after
            .unwrap_or_else(|| self.revoked_at.saturating_add(secs(max_validity)));
        expires.saturating_add(secs(margin))
    }
}

fn secs(duration: Duration) -> i64 {
    i64::try_from(duration.as_secs()).unwrap_or(i64::MAX)
}

fn unix(time: SystemTime) -> i64 {
    match time.duration_since(UNIX_EPOCH) {
        Ok(after) => secs(after),
        Err(before) => -secs(before.duration()),
    }
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
    /// Records the revocation of `serial`, a certificate that expires at
    /// `not_after`. It appears in every later [`Self::crl`] until
    /// [`CaOptions::revocation_margin`] after `not_after`, when it is
    /// forgotten: an expired certificate is refused anyway. Revoking a
    /// serial twice keeps the first record.
    ///
    /// [`CaOptions::revocation_margin`]: super::CaOptions::revocation_margin
    pub fn revoke(
        &self,
        serial: &Serial,
        reason: RevocationReason,
        not_after: SystemTime,
    ) -> Result<(), CaError> {
        let mut state = self.lock();
        if state.revoked.iter().any(|r| &r.serial == serial) {
            return Ok(());
        }
        let mut next = state.clone();
        next.revoked.push(Revoked {
            serial: serial.clone(),
            revoked_at: now().unix_timestamp(),
            reason,
            not_after: Some(unix(not_after)),
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

    /// A new DER CRL per trusted CA generation, the active one first, each
    /// listing every revoked serial whose certificate has not expired, valid
    /// from now for [`CaOptions::crl_validity`], with a CRL number one above
    /// the last.
    ///
    /// The revocation list is shared: serials are 128 random bits, so a
    /// serial of one generation in the other's CRL revokes nothing else.
    ///
    /// [`CaOptions::crl_validity`]: super::CaOptions::crl_validity
    pub fn crls(&self) -> Result<Vec<CertificateRevocationListDer<'static>>, CaError> {
        let mut state = self.lock();
        let this_update = now();
        let mut next = state.clone();
        next.crl_number += 1;
        // Expired certificates fail verification without the CRL; listing
        // them forever would grow it past what relays accept (ADR-0113).
        let before = next.revoked.len();
        let cutoff = this_update.unix_timestamp();
        let (max, margin) = (
            self.options.max_cert_validity,
            self.options.revocation_margin,
        );
        next.revoked
            .retain(|r| r.listed_until(max, margin) >= cutoff);
        // Persist first, so a number is never handed out twice.
        save(&self.dir.join(REVOCATIONS_FILE), &next)?;
        *state = next;
        let pruned = before - state.revoked.len();
        if pruned > 0 {
            info!(pruned, "forgot revocations of expired certificates");
        }

        let next_update = time::Duration::try_from(self.options.crl_validity)
            .ok()
            .and_then(|v| this_update.checked_add(v))
            .ok_or_else(|| CaError::InvalidRequest("CRL validity is too long".into()))?;
        let revoked_certs: Vec<_> = state
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
        let generations: Vec<_> = self.state().trusted().cloned().collect();
        generations
            .iter()
            .map(|generation| {
                let crl = CertificateRevocationListParams {
                    this_update,
                    next_update,
                    crl_number: SerialNumber::from(state.crl_number),
                    issuing_distribution_point: None,
                    revoked_certs: revoked_certs.clone(),
                    key_identifier_method: generation.key_id.clone(),
                }
                .signed_by(&generation.issuer)
                .map_err(CaError::signing)?;
                Ok(crl.der().clone())
            })
            .collect()
    }

    /// The CRL of the active CA alone; see [`Self::crls`].
    pub fn crl(&self) -> Result<CertificateRevocationListDer<'static>, CaError> {
        Ok(self.crls()?.swap_remove(0))
    }

    fn lock(&self) -> MutexGuard<'_, Revocations> {
        // Every update is applied whole after a successful save.
        self.revocations
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
    }
}
