//! Drives a change of certificate authority (ADR-0088) from start to end.
//!
//! The CA ([`crate::ca`]) keeps the files; this module decides when each
//! step happens, from what the `certificates` table says about every
//! enrolled agent and relay:
//!
//! 1. **Introduced**: the new CA is trusted, the old one still issues.
//!    Peers ask every hour and get the two CAs. Done when every peer with a
//!    valid certificate trusts the new CA.
//! 2. **Switched**: the new CA issues, the server presents a certificate
//!    from it. Peers get a new certificate within the hour. Done when every
//!    peer has one, or, in an emergency, at the distrust time.
//! 3. **Retired**: the old CA is no longer trusted and its key is gone.
//!
//! A peer that stays offline counts until its certificate expires: at most
//! the certificate validity (7 days). After each step the server reissues
//! its own certificate if needed and reloads its listeners.

use std::sync::Arc;
use std::time::{Duration, SystemTime};

use chrono::{DateTime, Utc};
use monitorflow_common::config::TlsPaths;
use monitorflow_common::tls::{Fingerprint, Identity};
use serde::Serialize;
use serde_json::json;
use sqlx::PgPool;
use tokio::sync::Notify;

use crate::api::audit::{self, Event};
use crate::ca::{CaError, CertificateAuthority, ServerName, Stage};
use crate::crl::CrlPublisher;
use crate::enroll::server_cert;
use crate::renew::{from_hex, hex};

/// Time between two looks at the fleet.
pub const POLL: Duration = Duration::from_secs(5 * 60);
/// A planned change starts by itself when the CA has less than this left.
pub const AUTO_START_BEFORE: Duration = Duration::from_secs(180 * 24 * 60 * 60);
/// The status page warns when the CA has less than this left.
pub const WARN_BEFORE: Duration = Duration::from_secs(365 * 24 * 60 * 60);
/// A planned change stops waiting for the last peers when the old CA has
/// less than this left: they could not keep it anyway.
pub const FORCE_BEFORE: Duration = Duration::from_secs(30 * 24 * 60 * 60);

#[derive(Debug, thiserror::Error)]
pub enum RotationError {
    #[error(transparent)]
    Ca(#[from] CaError),
    #[error("database: {0}")]
    Db(#[from] sqlx::Error),
    #[error("cannot reload the server's certificate: {0}")]
    Reload(String),
    #[error("task failed: {0}")]
    Task(#[from] tokio::task::JoinError),
}

/// Where an enrolled peer stands, from its newest certificate.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct PeerState {
    /// `agent` or `relay`.
    pub role: String,
    pub peer_id: String,
    /// The host name of a server, the name of a relay.
    pub name: String,
    pub last_seen_at: Option<DateTime<Utc>>,
    /// When its certificate expires: it stops counting then.
    pub not_after: DateTime<Utc>,
    /// Its certificate comes from the CA that issues now.
    pub moved: bool,
    /// It trusts every CA the server trusts.
    pub trusts_all: bool,
    /// Its version takes no CA bundle: it must update first.
    pub too_old: bool,
}

/// Every peer with a valid certificate, as the fleet stands now.
pub async fn fleet(pool: &PgPool, ca: &CertificateAuthority) -> Result<Vec<PeerState>, sqlx::Error> {
    let rows: Vec<FleetRow> = sqlx::query_as(
        "WITH latest AS (
             SELECT DISTINCT ON (peer_role, peer_id)
                    peer_role, peer_id, issuer_ca, trusted_cas, not_after, revoked_at
             FROM certificates WHERE peer_role IN ('agent', 'relay')
             ORDER BY peer_role, peer_id, issued_at DESC
         )
         SELECT l.peer_role, l.peer_id, coalesce(s.hostname, r.name, l.peer_id),
                coalesce(s.last_seen_at, r.last_seen_at), l.not_after,
                l.issuer_ca, l.trusted_cas
         FROM latest l
         LEFT JOIN servers s ON l.peer_role = 'agent' AND s.peer_id = l.peer_id
         LEFT JOIN relays r ON l.peer_role = 'relay' AND r.peer_id = l.peer_id
         WHERE l.revoked_at IS NULL AND l.not_after > now()
           AND s.revoked_at IS NULL AND r.revoked_at IS NULL
         ORDER BY l.peer_role, l.peer_id",
    )
    .fetch_all(pool)
    .await?;
    let trust = ca.trust();
    Ok(rows
        .into_iter()
        .map(|(role, peer_id, name, last_seen_at, not_after, issuer, trusted)| {
            let issuer = issuer.as_deref().and_then(from_hex);
            let fingerprints: Vec<Fingerprint> = match &trusted {
                Some(list) => list.iter().filter_map(|fp| from_hex(fp)).collect(),
                None => issuer.into_iter().collect(),
            };
            PeerState {
                role,
                peer_id,
                name,
                last_seen_at,
                not_after,
                moved: issuer == Some(trust.issuer),
                trusts_all: trust.bundle.iter().all(|fp| fingerprints.contains(fp)),
                too_old: trusted.is_none(),
            }
        })
        .collect())
}

type FleetRow = (
    String,
    String,
    String,
    Option<DateTime<Utc>>,
    DateTime<Utc>,
    Option<String>,
    Option<Vec<String>>,
);

/// Whether a peer still holds up the current step.
pub fn is_waiting(stage: Stage, peer: &PeerState) -> bool {
    match stage {
        Stage::Introduced => !peer.trusts_all,
        Stage::Switched => !peer.moved,
    }
}

/// Who asked for a step, for the audit log.
#[derive(Debug, Clone, Copy)]
pub enum Actor<'a> {
    /// The server, by itself.
    System,
    /// An administrator: the user's ID.
    User(&'a str),
}

impl Actor<'_> {
    fn kind_and_id(&self) -> (&str, &str) {
        match self {
            Actor::System => ("system", "ca-rotation"),
            Actor::User(id) => ("user", id),
        }
    }
}

/// Moves the change of certificate authority along. Cheap to clone.
#[derive(Clone)]
pub struct Rotator {
    pool: PgPool,
    ca: Arc<CertificateAuthority>,
    crl: Arc<CrlPublisher>,
    tls_paths: TlsPaths,
    names: Vec<ServerName>,
    wake: Arc<Notify>,
}

impl Rotator {
    pub fn new(
        pool: PgPool,
        ca: Arc<CertificateAuthority>,
        crl: Arc<CrlPublisher>,
        tls_paths: TlsPaths,
        names: Vec<ServerName>,
    ) -> Self {
        Self {
            pool,
            ca,
            crl,
            tls_paths,
            names,
            wake: Arc::new(Notify::new()),
        }
    }

    pub fn ca(&self) -> &Arc<CertificateAuthority> {
        &self.ca
    }

    pub fn pool(&self) -> &PgPool {
        &self.pool
    }

    /// Gives certificates recorded before ADR-0088 their issuer: the only
    /// CA there was. Nothing to do once a change has started.
    pub async fn backfill(&self) -> Result<(), sqlx::Error> {
        if self.ca.rotation().is_some() {
            return Ok(());
        }
        let done = sqlx::query("UPDATE certificates SET issuer_ca = $1 WHERE issuer_ca IS NULL")
            .bind(hex(&self.ca.fingerprint()))
            .execute(&self.pool)
            .await?;
        if done.rows_affected() > 0 {
            tracing::info!(certificates = done.rows_affected(), "recorded the issuer of earlier certificates");
        }
        Ok(())
    }

    /// Starts a change: planned, or `emergency` after a suspected
    /// compromise. Audited.
    pub async fn start(&self, emergency: bool, actor: Actor<'_>) -> Result<(), RotationError> {
        let ca = self.ca.clone();
        let before = ca.fingerprint();
        let rotation = tokio::task::spawn_blocking(move || ca.start_rotation(emergency)).await??;
        let status = self.ca.status();
        let new = match rotation.stage {
            Stage::Introduced => status.other.as_ref().map(|o| o.fingerprint),
            Stage::Switched => Some(status.active.fingerprint),
        };
        self.audit(
            actor,
            "ca.rotation.start",
            json!({
                "emergency": emergency,
                "old_ca": hex(&before),
                "new_ca": new.as_ref().map(hex),
                "distrust_at": rotation.distrust_at,
            }),
        )
        .await?;
        self.reload().await?;
        self.wake.notify_one();
        Ok(())
    }

    /// Takes the next step now, whoever has not moved: switch to the new
    /// CA, or retire the old one. Audited.
    pub async fn finish_now(&self, actor: Actor<'_>) -> Result<(), RotationError> {
        let waiting = self.waiting().await?;
        self.step(actor, waiting).await
    }

    async fn waiting(&self) -> Result<usize, sqlx::Error> {
        let Some(rotation) = self.ca.rotation() else {
            return Ok(0);
        };
        let fleet = fleet(&self.pool, &self.ca).await?;
        Ok(fleet.iter().filter(|p| is_waiting(rotation.stage, p)).count())
    }

    async fn step(&self, actor: Actor<'_>, waiting: usize) -> Result<(), RotationError> {
        let Some(rotation) = self.ca.rotation() else {
            return Err(CaError::InvalidRequest(
                "no change of certificate authority is under way".into(),
            )
            .into());
        };
        let ca = self.ca.clone();
        match rotation.stage {
            Stage::Introduced => {
                tokio::task::spawn_blocking(move || ca.switch()).await??;
                self.audit(
                    actor,
                    "ca.rotation.switch",
                    json!({ "new_ca": hex(&self.ca.fingerprint()), "not_moved": waiting }),
                )
                .await?;
            }
            Stage::Switched => {
                let retired = tokio::task::spawn_blocking(move || ca.retire()).await??;
                self.audit(
                    actor,
                    "ca.rotation.retire",
                    json!({
                        "old_ca": retired.fingerprint,
                        "emergency": retired.emergency,
                        "not_moved": waiting,
                    }),
                )
                .await?;
            }
        }
        self.reload().await
    }

    /// Takes the next step if the fleet is ready for it, or its time has
    /// come; starts a planned change when the CA nears its end.
    pub async fn advance(&self) -> Result<(), RotationError> {
        let now = SystemTime::now();
        let Some(rotation) = self.ca.rotation() else {
            let left = self.ca.not_after().duration_since(now).unwrap_or_default();
            if left < AUTO_START_BEFORE {
                tracing::warn!(?left, "the certificate authority expires soon; changing it");
                return self.start(false, Actor::System).await;
            }
            return Ok(());
        };
        let waiting = self.waiting().await?;
        let unix_now = DateTime::<Utc>::from(now).timestamp();
        let due = match rotation.stage {
            Stage::Switched if rotation.emergency => {
                rotation.distrust_at.is_some_and(|at| unix_now >= at)
            }
            Stage::Introduced => {
                // The old CA is the active one and its end is near.
                let left = self.ca.not_after().duration_since(now).unwrap_or_default();
                left < FORCE_BEFORE
            }
            Stage::Switched => false,
        };
        if waiting == 0 || due {
            tracing::info!(stage = ?rotation.stage, waiting, "taking the next step of the change of certificate authority");
            self.step(Actor::System, waiting).await?;
        }
        Ok(())
    }

    /// [`Self::advance`] every [`POLL`], or sooner after a start, until
    /// `shutdown`.
    pub async fn run(self, every: Duration, shutdown: impl std::future::Future<Output = ()>) {
        tokio::pin!(shutdown);
        if let Err(error) = self.backfill().await {
            tracing::error!(%error, "cannot record the issuer of earlier certificates");
        }
        loop {
            if let Err(error) = self.advance().await {
                tracing::error!(%error, "the change of certificate authority cannot go on");
            }
            tokio::select! {
                () = &mut shutdown => return,
                () = tokio::time::sleep(every) => {}
                () = self.wake.notified() => {}
            }
        }
    }

    /// Reissues the server's certificate if needed, then switches the
    /// listeners to it and to the current CAs.
    async fn reload(&self) -> Result<(), RotationError> {
        let (ca, paths, names) = (self.ca.clone(), self.tls_paths.clone(), self.names.clone());
        let identity = tokio::task::spawn_blocking(move || {
            server_cert::ensure(&ca, &paths, &names)?;
            Identity::load(&paths).map_err(|e| CaError::Signing(e.to_string()))
        })
        .await??;
        self.crl
            .reload(identity)
            .await
            .map_err(|e| RotationError::Reload(e.to_string()))
    }

    async fn audit(
        &self,
        actor: Actor<'_>,
        action: &str,
        details: serde_json::Value,
    ) -> Result<(), sqlx::Error> {
        let (actor_kind, actor_id) = actor.kind_and_id();
        audit::record(
            &self.pool,
            Event {
                actor_kind,
                actor_id,
                action,
                target: None,
                details,
            },
        )
        .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn peer(moved: bool, trusts_all: bool) -> PeerState {
        PeerState {
            role: "agent".into(),
            peer_id: "web-01".into(),
            name: "web-01".into(),
            last_seen_at: None,
            not_after: Utc::now(),
            moved,
            trusts_all,
            too_old: false,
        }
    }

    #[test]
    fn each_step_waits_for_its_own_condition() {
        assert!(is_waiting(Stage::Introduced, &peer(true, false)));
        assert!(!is_waiting(Stage::Introduced, &peer(true, true)));
        assert!(is_waiting(Stage::Switched, &peer(false, true)));
        assert!(!is_waiting(Stage::Switched, &peer(true, false)));
    }

    #[test]
    fn actors_name_themselves_in_the_audit_log() {
        assert_eq!(Actor::System.kind_and_id(), ("system", "ca-rotation"));
        assert_eq!(Actor::User("u1").kind_and_id(), ("user", "u1"));
    }
}
