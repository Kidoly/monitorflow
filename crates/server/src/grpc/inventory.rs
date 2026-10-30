//! Inventory rows of connected peers (ADR-0008): an agent's `Hello` creates
//! or refreshes its `servers` row, relays must have their `relays` row, and
//! a revoked row refuses the stream even with a valid certificate.

use chrono::{DateTime, Utc};
use monitorflow_common::proto::v1::{Hello, RelayMirrors};
use monitorflow_common::tls::{PeerIdentity, PeerRole};
use sqlx::{PgPool, Postgres, Transaction};
use tonic::async_trait;
use uuid::Uuid;

use crate::api::audit::{self, Event};
use crate::mirror::relays;

#[derive(Debug, thiserror::Error)]
pub enum InventoryError {
    #[error("{role} {id} is revoked")]
    Revoked { role: PeerRole, id: String },
    #[error("{role} {id} is not enrolled")]
    NotEnrolled { role: PeerRole, id: String },
    /// Enrolled by an install key that asks for approval (ADR-0058).
    #[error("waiting for an administrator to approve this host in the web interface")]
    AwaitingApproval,
    #[error("inventory database error: {0}")]
    Db(#[from] sqlx::Error),
}

/// An accepted `Hello`, after the identity checks of the stream.
#[derive(Debug, Clone, Copy)]
pub struct AcceptedHello<'a> {
    /// The certificate identity of the connection.
    pub peer: &'a PeerIdentity,
    pub hello: &'a Hello,
    /// Peer ID of the relay the peer is attached to, if any: the relay
    /// speaking for an agent, or the parent of a chained relay (ADR-0028).
    pub via_relay: Option<&'a str>,
    /// Role of the peer the Hello is about, whose ID is `hello.agent_id`:
    /// a relay speaking for itself, or an agent.
    pub role: PeerRole,
}

impl AcceptedHello<'_> {
    pub fn role(&self) -> PeerRole {
        self.role
    }
}

/// Records connected peers. The server runs [`DbInventory`].
#[async_trait]
pub trait Inventory: Send + Sync + 'static {
    /// Records a Hello and returns the name of the peer's site, the `site`
    /// label of its series (ADR-0013). An error refuses the stream.
    async fn hello(&self, hello: AcceptedHello<'_>) -> Result<String, InventoryError>;
    /// Records that the peer `id` with `role` is still connected.
    async fn seen(&self, role: PeerRole, id: &str) -> Result<(), InventoryError>;
    /// Records the mirrors the relay `id` serves, received at
    /// `received_at` (ADR-0037). An invalid report clears them.
    async fn relay_mirrors(
        &self,
        _id: &str,
        _received_at: DateTime<Utc>,
        _report: RelayMirrors,
    ) -> Result<(), InventoryError> {
        Ok(())
    }
}

/// Records nothing, accepts every peer and reports no site. **Tests only**:
/// in production it would ignore revocations.
#[derive(Debug, Clone, Copy, Default)]
pub struct NoInventory;

#[async_trait]
impl Inventory for NoInventory {
    async fn hello(&self, _hello: AcceptedHello<'_>) -> Result<String, InventoryError> {
        Ok(String::new())
    }

    async fn seen(&self, _role: PeerRole, _id: &str) -> Result<(), InventoryError> {
        Ok(())
    }
}

/// The production inventory, in the `servers` and `relays` tables.
#[derive(Debug, Clone)]
pub struct DbInventory {
    pool: PgPool,
}

impl DbInventory {
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }
}

#[async_trait]
impl Inventory for DbInventory {
    async fn hello(&self, hello: AcceptedHello<'_>) -> Result<String, InventoryError> {
        let mut tx = self.pool.begin().await?;
        let site = match hello.role() {
            PeerRole::Relay => {
                relay_hello(&mut tx, &hello.hello.agent_id, &hello.hello.agent_version).await?
            }
            _ => agent_hello(&mut tx, hello).await?,
        };
        tx.commit().await?;
        Ok(site)
    }

    async fn seen(&self, role: PeerRole, id: &str) -> Result<(), InventoryError> {
        let sql = match role {
            PeerRole::Relay => {
                "UPDATE relays SET last_seen_at = now() WHERE peer_id = $1 AND revoked_at IS NULL"
            }
            _ => {
                "UPDATE servers SET last_seen_at = now() WHERE peer_id = $1 AND revoked_at IS NULL"
            }
        };
        sqlx::query(sql).bind(id).execute(&self.pool).await?;
        Ok(())
    }

    async fn relay_mirrors(
        &self,
        id: &str,
        received_at: DateTime<Utc>,
        report: RelayMirrors,
    ) -> Result<(), InventoryError> {
        let served = relays::check(&report).unwrap_or_else(|error| {
            tracing::warn!(relay_id = id, %error, "ignoring the relay's invalid mirror report");
            None
        });
        let stored = relays::record(&self.pool, id, received_at, served.as_ref()).await?;
        tracing::debug!(
            relay_id = id,
            url = served.as_ref().map(|s| s.url.as_str()),
            mirrors = served.as_ref().map_or(0, |s| s.mirrors.len()),
            stored,
            "relay mirror report"
        );
        Ok(())
    }
}

/// Name of the site `site_id`.
async fn site_name(
    tx: &mut Transaction<'_, Postgres>,
    site_id: Uuid,
) -> Result<String, InventoryError> {
    Ok(sqlx::query_scalar("SELECT name FROM sites WHERE id = $1")
        .bind(site_id)
        .fetch_one(&mut **tx)
        .await?)
}

/// A relay's own Hello: its row was created at enrollment. Records the
/// version it reports (ADR-0055).
async fn relay_hello(
    tx: &mut Transaction<'_, Postgres>,
    id: &str,
    version: &str,
) -> Result<String, InventoryError> {
    let row: Option<(bool, Uuid)> = sqlx::query_as(
        "UPDATE relays SET last_seen_at = CASE WHEN revoked_at IS NULL THEN now() ELSE last_seen_at END,
                           relay_version = coalesce($2, relay_version)
         WHERE peer_id = $1
         RETURNING revoked_at IS NOT NULL, site_id",
    )
    .bind(id)
    .bind(Some(version).filter(|v| !v.is_empty()))
    .fetch_optional(&mut **tx)
    .await?;
    let (role, id) = (PeerRole::Relay, id.to_owned());
    match row {
        Some((false, site_id)) => site_name(tx, site_id).await,
        Some((true, _)) => Err(InventoryError::Revoked { role, id }),
        None => Err(InventoryError::NotEnrolled { role, id }),
    }
}

/// An agent's Hello: updates its `servers` row, or creates it in the site
/// of its enrollment token. The site of an existing row never changes.
async fn agent_hello(
    tx: &mut Transaction<'_, Postgres>,
    accepted: AcceptedHello<'_>,
) -> Result<String, InventoryError> {
    let hello = accepted.hello;
    let id = hello.agent_id.as_str();
    let existing: Option<(Uuid, bool, Uuid)> = sqlx::query_as(
        "SELECT id, revoked_at IS NOT NULL, site_id FROM servers WHERE peer_id = $1 FOR UPDATE",
    )
    .bind(id)
    .fetch_optional(&mut **tx)
    .await?;
    let relay_id: Option<Uuid> = match accepted.via_relay {
        Some(relay) => sqlx::query_scalar("SELECT id FROM relays WHERE peer_id = $1")
            .bind(relay)
            .fetch_optional(&mut **tx)
            .await?
            .ok_or_else(|| InventoryError::NotEnrolled {
                role: PeerRole::Relay,
                id: relay.to_owned(),
            })
            .map(Some)?,
        None => None,
    };
    let non_empty = |s: &str| Some(s.to_owned()).filter(|s| !s.is_empty());
    let (os_version, agent_version) = (
        non_empty(&hello.os_version),
        non_empty(&hello.agent_version),
    );
    // The family alone says so too, should an older relay drop the flag.
    let package_updates = !hello.no_package_manager && hello.os_family != "linux";

    match existing {
        Some((_, true, _)) => Err(InventoryError::Revoked {
            role: PeerRole::Agent,
            id: id.to_owned(),
        }),
        Some((server_id, false, site_id)) => {
            // relay_id follows the path of the latest connection; the
            // composite foreign key keeps it in the server's site.
            sqlx::query(
                "UPDATE servers SET hostname = $2, os_family = $3, os_version = $4,
                        agent_version = $5, relay_id = $6, package_updates = $7,
                        last_seen_at = now()
                 WHERE id = $1",
            )
            .bind(server_id)
            .bind(&hello.hostname)
            .bind(&hello.os_family)
            .bind(&os_version)
            .bind(&agent_version)
            .bind(relay_id)
            .bind(package_updates)
            .execute(&mut **tx)
            .await?;
            site_name(tx, site_id).await
        }
        None => {
            let pending: bool = sqlx::query_scalar(
                "SELECT EXISTS (SELECT 1 FROM pending_hosts WHERE peer_id = $1)",
            )
            .bind(id)
            .fetch_one(&mut **tx)
            .await?;
            if pending {
                return Err(InventoryError::AwaitingApproval);
            }
            let site_id: Uuid = sqlx::query_scalar(crate::db::ENROLLED_AGENT_SITE)
                .bind(id)
                .fetch_optional(&mut **tx)
                .await?
                .ok_or_else(|| InventoryError::NotEnrolled {
                    role: PeerRole::Agent,
                    id: id.to_owned(),
                })?;
            let server_id: Uuid = sqlx::query_scalar(
                "INSERT INTO servers (peer_id, site_id, relay_id, hostname, os_family,
                                      os_version, agent_version, package_updates,
                                      last_seen_at)
                 VALUES ($1, $2, $3, $4, $5, $6, $7, $8, now())
                 RETURNING id",
            )
            .bind(id)
            .bind(site_id)
            .bind(relay_id)
            .bind(&hello.hostname)
            .bind(&hello.os_family)
            .bind(&os_version)
            .bind(&agent_version)
            .bind(package_updates)
            .fetch_one(&mut **tx)
            .await?;
            let actor = accepted.peer.uri();
            audit::record(
                &mut **tx,
                Event {
                    actor_kind: "peer",
                    actor_id: &actor,
                    action: "server.create",
                    target: Some(("agent", id)),
                    details: serde_json::json!({
                        "server_id": server_id,
                        "site_id": site_id,
                        "hostname": hello.hostname,
                        "os_family": hello.os_family,
                        "via_relay": accepted.via_relay,
                    }),
                },
            )
            .await?;
            tracing::info!(agent_id = id, %site_id, %server_id, "created a server row");
            site_name(tx, site_id).await
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn no_inventory_accepts_everything() {
        let hello = Hello::default();
        let peer = PeerIdentity::new(PeerRole::Agent, "web-01").unwrap();
        let accepted = AcceptedHello {
            peer: &peer,
            hello: &hello,
            via_relay: None,
            role: PeerRole::Agent,
        };
        assert_eq!(NoInventory.hello(accepted).await.unwrap(), "");
        assert!(NoInventory.seen(PeerRole::Agent, "web-01").await.is_ok());
    }
}
