//! The versions agents and relays run (ADR-0055), for global
//! administrators:
//!
//! - [`distribution`]: how many agents or relays run each version, the
//!   `versions` of `GET /api/auto-update`.
//! - `GET /api/auto-update/{kind}/versions/peers?version=…`: the agents or
//!   relays that run one of them, with where each stands towards the
//!   target.

use std::cmp::Ordering;
use std::collections::HashMap;
use std::str::FromStr;

use axum::extract::rejection::QueryRejection;
use axum::extract::{Path, Query, State};
use axum::Json;
use monitorflow_common::release::Version;
use serde::{Deserialize, Serialize};
use sqlx::PgPool;
use uuid::Uuid;

use super::auth::CurrentUser;
use super::auto_update::{DEFAULT_PEERS, MAX_PEERS};
use super::server_update::require_global_admin;
use super::servers::page_limit;
use super::{ApiError, ApiState};
use crate::auto_update::policy;
use crate::auto_update::store::{self, Kind, Rollout};
use crate::auto_update::{server_version, PeerState};

/// How many agents or relays run one version.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, utoipa::ToSchema)]
#[schema(as = AutoUpdateVersionCount)]
pub struct VersionCount {
    /// As they report it; `null` for those that have not reported one
    /// yet.
    pub version: Option<String>,
    pub count: usize,
    /// On the target or newer. `false` when there is no target, and for a
    /// version that is not a release version, like a development build.
    pub up_to_date: bool,
}

/// The agents (servers with an agent, not revoked, imported ones that
/// have no agent yet left out) or relays (not revoked) that `GET
/// /api/auto-update` counts, as `p`: `$head FROM (…) p $tail`. A macro,
/// so each query stays a static string.
macro_rules! peers_sql {
    (agent, $head:literal, $tail:literal) => {
        concat!(
            $head,
            " FROM (
                SELECT s.id, s.hostname AS name, s.agent_version AS version, s.site_id,
                       si.name AS site_name, c.id AS customer_id, c.name AS customer_name
                FROM servers s
                JOIN sites si ON si.id = s.site_id
                JOIN customers c ON c.id = si.customer_id
                WHERE s.revoked_at IS NULL AND NOT s.awaiting_agent AND s.kind = 'agent'
             ) p ",
            $tail
        )
    };
    (relay, $head:literal, $tail:literal) => {
        concat!(
            $head,
            " FROM (
                SELECT r.id, r.name, r.relay_version AS version, r.site_id,
                       si.name AS site_name, c.id AS customer_id, c.name AS customer_name
                FROM relays r
                JOIN sites si ON si.id = r.site_id
                JOIN customers c ON c.id = si.customer_id
                WHERE r.revoked_at IS NULL
             ) p ",
            $tail
        )
    };
}

/// Newest release first, then other versions by name, then no version.
fn newest_first(a: &Option<String>, b: &Option<String>) -> Ordering {
    let parse = |v: &Option<String>| v.as_deref().and_then(|v| Version::from_str(v).ok());
    match (a, b) {
        (None, None) => Ordering::Equal,
        (None, Some(_)) => Ordering::Greater,
        (Some(_), None) => Ordering::Less,
        (Some(x), Some(y)) => match (parse(a), parse(b)) {
            (Some(va), Some(vb)) => vb.cmp(&va).then_with(|| x.cmp(y)),
            (Some(_), None) => Ordering::Less,
            (None, Some(_)) => Ordering::Greater,
            (None, None) => x.cmp(y),
        },
    }
}

fn up_to_date(version: Option<&str>, target: Option<&Version>) -> bool {
    match (version.and_then(|v| Version::from_str(v).ok()), target) {
        (Some(version), Some(target)) => version >= *target,
        _ => false,
    }
}

/// `counts` per version, newest first.
pub fn count(
    counts: impl IntoIterator<Item = (Option<String>, usize)>,
    target: Option<&Version>,
) -> Vec<VersionCount> {
    let mut list: Vec<VersionCount> = counts
        .into_iter()
        .map(|(version, count)| VersionCount {
            up_to_date: up_to_date(version.as_deref(), target),
            version,
            count,
        })
        .collect();
    list.sort_by(|a, b| newest_first(&a.version, &b.version));
    list
}

/// The `versions` of `GET /api/auto-update` for `kind`.
pub async fn distribution(
    pool: &PgPool,
    kind: Kind,
    target: Option<&Version>,
) -> Result<Vec<VersionCount>, sqlx::Error> {
    let rows: Vec<(Option<String>, i64)> = match kind {
        Kind::Agent => sqlx::query_as(peers_sql!(
            agent,
            "SELECT p.version, count(*)",
            "GROUP BY p.version"
        )),
        Kind::Relay => sqlx::query_as(peers_sql!(
            relay,
            "SELECT p.version, count(*)",
            "GROUP BY p.version"
        )),
    }
    .fetch_all(pool)
    .await?;
    let counts = rows
        .into_iter()
        .map(|(version, n)| (version, usize::try_from(n).unwrap_or(0)));
    Ok(count(counts, target))
}

/// Query string of `GET /api/auto-update/{kind}/versions/peers`.
#[derive(Debug, Deserialize, utoipa::IntoParams)]
#[into_params(parameter_in = Query)]
pub struct VersionPeersParams {
    /// The version, as `versions` of `GET /api/auto-update` gives it.
    /// Absent: those that have not reported one yet.
    pub version: Option<String>,
    /// Only the peers of this site.
    pub site_id: Option<Uuid>,
    /// 1 to 500, default 100.
    pub limit: Option<u32>,
    /// Peers to skip, default 0.
    pub offset: Option<u32>,
}

/// Body of `GET /api/auto-update/{kind}/versions/peers`.
#[derive(Debug, Serialize, utoipa::ToSchema)]
#[schema(as = AutoUpdateVersionPeerList)]
pub struct VersionPeerList {
    pub version: Option<String>,
    /// What peers move to; `null` when there is none.
    pub target: Option<String>,
    /// Peers on this version (and site), the `count` of `versions`;
    /// `peers` holds one page of them.
    pub total: usize,
    /// By customer, site and name.
    pub peers: Vec<VersionPeer>,
}

/// One agent or relay on that version.
#[derive(Debug, Serialize, utoipa::ToSchema)]
#[schema(as = AutoUpdateVersionPeer)]
pub struct VersionPeer {
    /// The server's ID for an agent, the relay's for a relay.
    pub id: Uuid,
    /// The server's hostname or the relay's name.
    pub name: String,
    pub site_id: Uuid,
    pub site_name: String,
    pub customer_id: Uuid,
    pub customer_name: String,
    /// Where it stands towards the target, as the totals count it; `null`
    /// when there is no target, or when it is counted in none of them.
    pub state: Option<PeerState>,
}

#[derive(sqlx::FromRow)]
struct Row {
    id: Uuid,
    name: String,
    site_id: Uuid,
    site_name: String,
    customer_id: Uuid,
    customer_name: String,
}

/// `GET /api/auto-update/{kind}/versions/peers`.
pub async fn peers(
    State(state): State<ApiState>,
    user: CurrentUser,
    Path(kind): Path<String>,
    params: Result<Query<VersionPeersParams>, QueryRejection>,
) -> Result<Json<VersionPeerList>, ApiError> {
    require_global_admin(&user)?;
    let kind = Kind::from_str(&kind).map_err(|()| ApiError::NotFound)?;
    let Query(params) = params.map_err(|r| ApiError::Invalid(r.body_text()))?;
    let limit = page_limit(params.limit, DEFAULT_PEERS, MAX_PEERS)? as usize;
    let offset = params.offset.unwrap_or(0) as usize;
    let rows: Vec<Row> = match kind {
        Kind::Agent => sqlx::query_as(peers_sql!(
            agent,
            "SELECT *",
            "WHERE p.version IS NOT DISTINCT FROM $1
             AND ($2::uuid IS NULL OR p.site_id = $2)
             ORDER BY p.customer_name, p.site_name, p.name, p.id"
        )),
        Kind::Relay => sqlx::query_as(peers_sql!(
            relay,
            "SELECT *",
            "WHERE p.version IS NOT DISTINCT FROM $1
             AND ($2::uuid IS NULL OR p.site_id = $2)
             ORDER BY p.customer_name, p.site_name, p.name, p.id"
        )),
    }
    .bind(&params.version)
    .bind(params.site_id)
    .fetch_all(&state.pool)
    .await?;
    let held = state.settings.agent_releases.held().await;
    let target = policy::target(&held, &server_version()).cloned();
    let mut states: HashMap<Uuid, PeerState> = HashMap::new();
    if let Some(target) = &target {
        let counted_from = Rollout::load(&state.pool, kind, target)
            .await?
            .map(|r| r.counted_from);
        for peer in store::peers(&state.pool, kind, target, counted_from).await? {
            if let Some(peer_state) = peer.state(target) {
                states.insert(peer.id, peer_state);
            }
        }
    }
    let total = rows.len();
    let peers = rows
        .into_iter()
        .skip(offset)
        .take(limit)
        .map(|row| VersionPeer {
            state: states.get(&row.id).copied(),
            id: row.id,
            name: row.name,
            site_id: row.site_id,
            site_name: row.site_name,
            customer_id: row.customer_id,
            customer_name: row.customer_name,
        })
        .collect();
    Ok(Json(VersionPeerList {
        version: params.version,
        target: target.map(|t| t.to_string()),
        total,
        peers,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn v(text: &str) -> Option<String> {
        Some(text.to_owned())
    }

    #[test]
    fn versions_are_counted_newest_release_first() {
        let target = Version::from_str("0.9.1").unwrap();
        let counted = count(
            [
                (v("0.9.0"), 2),
                (None, 1),
                (v("dev"), 1),
                (v("0.10.0"), 1),
                (v("0.9.1"), 3),
            ],
            Some(&target),
        );
        let seen: Vec<(Option<&str>, usize, bool)> = counted
            .iter()
            .map(|c| (c.version.as_deref(), c.count, c.up_to_date))
            .collect();
        assert_eq!(
            seen,
            [
                (Some("0.10.0"), 1, true),
                (Some("0.9.1"), 3, true),
                (Some("0.9.0"), 2, false),
                (Some("dev"), 1, false),
                (None, 1, false),
            ]
        );
    }

    #[test]
    fn nothing_is_up_to_date_without_a_target() {
        let counted = count([(v("0.9.1"), 4)], None);
        assert!(!counted[0].up_to_date);
        assert!(count([], None).is_empty());
    }
}
