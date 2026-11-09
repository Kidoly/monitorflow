//! Which IDs a new enrollment may have (ADR-0105). `servers.peer_id` and
//! `relays.peer_id` are each unique, and nothing in the schema relates
//! them, so the rules that keep one ID with one role and one site are
//! checked here, by every path that creates or consumes a token.

use monitorflow_common::tls::PeerRole;
use sqlx::PgConnection;
use uuid::Uuid;

/// Serialises the choice of IDs between concurrent enrollments and token
/// creations, so two of them cannot both find an ID free.
const ID_LOCK: i64 = 0x6d66_6964; // "mfid"

/// Why a new enrollment cannot have an ID.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum IdTaken {
    #[error("the ID is held by a peer of the other role")]
    OtherRole,
    #[error("the ID is of an agent of another site")]
    OtherSite,
}

/// Which tokens of the other role hold an ID.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Pending {
    /// Creating a token: one waiting for the other role counts, or both
    /// would be made and only one could be used.
    Counts,
    /// Consuming a token: the first of two to enroll has the ID.
    Ignored,
}

/// Waits for the ID lock, held until the end of the transaction of `conn`.
pub async fn lock(conn: &mut PgConnection) -> Result<(), sqlx::Error> {
    sqlx::query("SELECT pg_advisory_xact_lock($1)")
        .bind(ID_LOCK)
        .execute(conn)
        .await
        .map(drop)
}

/// The `servers` or `relays` table and the role name, for `role` and for
/// the other one.
fn tables(role: PeerRole) -> ((&'static str, &'static str), (&'static str, &'static str)) {
    let (agent, relay) = (("servers", "agent"), ("relays", "relay"));
    match role {
        PeerRole::Relay => (relay, agent),
        _ => (agent, relay),
    }
}

/// Whether the other role than `role` holds `peer_id`: an unrevoked row, a
/// certificate that still works or, with [`Pending::Counts`], a token not
/// used yet. Never for a peer that holds the ID itself already: a fleet
/// from before this rule may have an agent and a relay of one ID, which
/// keep enrolling as they did.
pub async fn held_by_other_role(
    conn: &mut PgConnection,
    role: PeerRole,
    peer_id: &str,
    pending: Pending,
) -> Result<bool, sqlx::Error> {
    let ((own, own_role), (other, other_role)) = tables(role);
    // SQL-safe: the table names are constants, never input.
    sqlx::query_scalar(sqlx::AssertSqlSafe(format!(
        "SELECT NOT (EXISTS (SELECT 1 FROM {own} WHERE peer_id = $1 AND revoked_at IS NULL)
                     OR EXISTS (SELECT 1 FROM certificates
                                WHERE peer_role = $2 AND peer_id = $1
                                  AND revoked_at IS NULL AND not_after > now()))
            AND (EXISTS (SELECT 1 FROM {other} WHERE peer_id = $1 AND revoked_at IS NULL)
                 OR EXISTS (SELECT 1 FROM certificates
                            WHERE peer_role = $3 AND peer_id = $1
                              AND revoked_at IS NULL AND not_after > now())
                 OR ($4 AND EXISTS (SELECT 1 FROM enrollment_tokens
                                    WHERE role = $3 AND peer_id = $1
                                      AND used_at IS NULL AND expires_at > now())))"
    )))
    .bind(peer_id)
    .bind(own_role)
    .bind(other_role)
    .bind(pending == Pending::Counts)
    .fetch_one(conn)
    .await
}

/// Whether the agent `peer_id` belongs to another site than `site_id`: its
/// row is there, or a host of that ID waits for approval there, or it has
/// no row yet and a certificate that still works, enrolled in another
/// site. The site of a certificate is the one of the latest consumed
/// token (ADR-0008), `consumed` aside: the token of the enrollment that
/// asks. Without this, whoever consumed a token for the ID in their own
/// site before the agent's first Hello got its row there.
pub async fn agent_of_another_site(
    conn: &mut PgConnection,
    peer_id: &str,
    site_id: Uuid,
    consumed: Option<Uuid>,
) -> Result<bool, sqlx::Error> {
    sqlx::query_scalar(
        "SELECT EXISTS (SELECT 1 FROM servers WHERE peer_id = $1 AND site_id <> $2)
             OR EXISTS (SELECT 1 FROM pending_hosts WHERE peer_id = $1 AND site_id <> $2)
             OR (NOT EXISTS (SELECT 1 FROM servers WHERE peer_id = $1)
                 AND EXISTS (SELECT 1 FROM certificates
                             WHERE peer_role = 'agent' AND peer_id = $1
                               AND revoked_at IS NULL AND not_after > now())
                 AND COALESCE((SELECT t.site_id <> $2 FROM enrollment_tokens t
                               WHERE t.role = 'agent' AND t.peer_id = $1
                                 AND t.used_at IS NOT NULL
                                 AND ($3::uuid IS NULL OR t.id <> $3)
                               ORDER BY t.used_at DESC LIMIT 1), false))",
    )
    .bind(peer_id)
    .bind(site_id)
    .bind(consumed)
    .fetch_one(conn)
    .await
}

/// Both checks, for a token being created or consumed in `site_id`. The
/// caller holds [`lock`].
pub async fn check(
    conn: &mut PgConnection,
    role: PeerRole,
    peer_id: &str,
    site_id: Uuid,
    consumed: Option<Uuid>,
) -> Result<Result<(), IdTaken>, sqlx::Error> {
    let pending = match consumed {
        Some(_) => Pending::Ignored,
        None => Pending::Counts,
    };
    if held_by_other_role(&mut *conn, role, peer_id, pending).await? {
        return Ok(Err(IdTaken::OtherRole));
    }
    if role == PeerRole::Agent && agent_of_another_site(conn, peer_id, site_id, consumed).await? {
        return Ok(Err(IdTaken::OtherSite));
    }
    Ok(Ok(()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn each_role_looks_at_the_table_of_the_other() {
        assert_eq!(
            tables(PeerRole::Agent),
            (("servers", "agent"), ("relays", "relay"))
        );
        assert_eq!(
            tables(PeerRole::Relay),
            (("relays", "relay"), ("servers", "agent"))
        );
    }
}
