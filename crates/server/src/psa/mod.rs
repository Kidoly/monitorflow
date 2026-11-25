//! The PSA connection (ADR-0164, ADR-0166): MonitorFlow calls a PSA's API
//! with credentials it keeps, HaloPSA first. One connection per
//! installation, managed by global administrators; its client secret is
//! sealed at rest (ADR-0125) and never returned.

pub mod assets;
pub mod halopsa;
pub mod names;
pub mod poller;
pub mod sync;

use chrono::{DateTime, Utc};
use monitorflow_common::redact::Secret;
use sqlx::PgExecutor;
use uuid::Uuid;

use crate::secrets::{self, Column};

pub use halopsa::{Credentials, Halo, HaloError};

/// The page of the HaloPSA connection, under the web interface's URL.
pub const PAGE: &str = "/settings/integrations/halopsa/";
/// The only kind of PSA so far.
pub const HALOPSA: &str = "halopsa";
/// Longest client ID and tenant.
pub const MAX_ID: usize = 200;

/// The connection as stored, with its secret opened or why it does not.
#[derive(Debug, Clone)]
pub struct Connection {
    pub id: Uuid,
    pub kind: String,
    pub base_url: String,
    pub tenant: Option<String>,
    pub client_id: String,
    pub client_secret: secrets::Opened,
    pub enabled: bool,
    pub agent_name: Option<String>,
    pub agent_id: Option<i64>,
    pub last_success_at: Option<DateTime<Utc>>,
    pub last_error: Option<String>,
    pub last_error_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub tickets: TicketSettings,
    pub default_client: DefaultClient,
    pub assets: AssetSettings,
}

/// Where incidents of a site matched to nothing open tickets (ADR-0191).
#[derive(Debug, Clone, PartialEq, Eq, sqlx::FromRow, serde::Serialize, utoipa::ToSchema)]
#[schema(as = PsaDefaultClient)]
pub struct DefaultClient {
    /// The PSA client; `null` opens no ticket for such sites.
    pub default_client_id: Option<i64>,
    pub default_client_name: Option<String>,
    /// One of its sites; `null` lets the PSA choose.
    pub default_site_id: Option<i64>,
    pub default_site_name: Option<String>,
}

/// Whether servers are synced as assets (ADR-0191).
#[derive(Debug, Clone, PartialEq, Eq, sqlx::FromRow, serde::Serialize, utoipa::ToSchema)]
#[schema(as = PsaAssetSwitches)]
pub struct AssetSettings {
    /// Servers of matched sites and customers are synced as PSA assets.
    pub asset_sync: bool,
    /// A removed server's asset is marked inactive; otherwise left as it
    /// is.
    pub asset_inactive_on_removal: bool,
}

/// What tickets the connection opens and how it closes them (ADR-0166).
#[derive(Debug, Clone, PartialEq, Eq, sqlx::FromRow, serde::Serialize, utoipa::ToSchema)]
#[schema(as = PsaTicketSettings)]
pub struct TicketSettings {
    /// The PSA ticket type of new tickets; `null` for the PSA's default.
    pub ticket_type_id: Option<i64>,
    pub ticket_type_name: Option<String>,
    /// The category of new tickets (HaloPSA's `category_1`), which some
    /// instances require; `null` for none.
    pub category: Option<String>,
    /// `critical` (the default) or `warning`: the least severity whose
    /// incidents open a ticket.
    pub min_severity: String,
    /// The status a ticket moves to when its incident is resolved, 9
    /// (Closed) by default; `null` adds a note only.
    pub resolved_status_id: Option<i64>,
    pub resolved_status_name: Option<String>,
    /// The priority of tickets of warning incidents, a priority number of
    /// the ticket type's SLA; `null` for the PSA's default.
    pub warning_priority_id: Option<i64>,
    pub warning_priority_name: Option<String>,
    /// The priority of tickets of critical incidents, which an incident
    /// escalated from warning also moves its ticket to; `null` for the
    /// PSA's default.
    pub critical_priority_id: Option<i64>,
    pub critical_priority_name: Option<String>,
    /// The PSA statuses that mean closed, 9 (Closed) by default: a ticket
    /// moved to one acknowledges its open incident.
    pub closed_status_ids: Vec<i64>,
    /// A ticket assigned to an agent, other than the application's own,
    /// acknowledges its incident.
    pub acknowledge_on_assign: bool,
}

impl TicketSettings {
    /// The priority of a ticket for an incident of `severity`.
    pub fn priority(&self, severity: &str) -> Option<i64> {
        match severity {
            "critical" => self.critical_priority_id,
            "warning" => self.warning_priority_id,
            _ => None,
        }
    }
}

#[derive(sqlx::FromRow)]
struct Row {
    id: Uuid,
    kind: String,
    base_url: String,
    tenant: Option<String>,
    client_id: String,
    client_secret: String,
    enabled: bool,
    agent_name: Option<String>,
    agent_id: Option<i64>,
    last_success_at: Option<DateTime<Utc>>,
    last_error: Option<String>,
    last_error_at: Option<DateTime<Utc>>,
    created_at: DateTime<Utc>,
    updated_at: DateTime<Utc>,
    #[sqlx(flatten)]
    tickets: TicketSettings,
    #[sqlx(flatten)]
    default_client: DefaultClient,
    #[sqlx(flatten)]
    assets: AssetSettings,
}

impl From<Row> for Connection {
    fn from(row: Row) -> Self {
        Self {
            id: row.id,
            kind: row.kind,
            base_url: row.base_url,
            tenant: row.tenant,
            client_id: row.client_id,
            client_secret: secrets::open(Column::PsaConnections, &row.client_secret)
                .map_err(|e| e.to_string()),
            enabled: row.enabled,
            agent_name: row.agent_name,
            agent_id: row.agent_id,
            last_success_at: row.last_success_at,
            last_error: row.last_error,
            last_error_at: row.last_error_at,
            created_at: row.created_at,
            updated_at: row.updated_at,
            tickets: row.tickets,
            default_client: row.default_client,
            assets: row.assets,
        }
    }
}

impl Connection {
    /// What signs in to the PSA, or why the secret does not open.
    pub fn credentials(&self) -> Result<Credentials, HaloError> {
        let secret = self.client_secret.as_ref().map_err(|message| HaloError {
            message: message.clone(),
            status: None,
        })?;
        Ok(Credentials {
            base_url: self.base_url.clone(),
            tenant: self.tenant.clone(),
            client_id: self.client_id.clone(),
            client_secret: secret.clone(),
        })
    }
}

const SELECT: &str = "SELECT id, kind, base_url, tenant, client_id, client_secret, enabled,
        agent_name, agent_id, last_success_at, last_error, last_error_at, created_at, updated_at,
        ticket_type_id, ticket_type_name, category, min_severity, resolved_status_id,
        resolved_status_name, warning_priority_id, warning_priority_name, critical_priority_id,
        critical_priority_name, closed_status_ids, acknowledge_on_assign, default_client_id,
        default_client_name, default_site_id, default_site_name, asset_sync,
        asset_inactive_on_removal
    FROM psa_connections";

/// The connection, if there is one.
pub async fn load(db: impl PgExecutor<'_>) -> Result<Option<Connection>, sqlx::Error> {
    let row: Option<Row> = sqlx::query_as(SELECT).fetch_optional(db).await?;
    Ok(row.map(Connection::from))
}

/// The connection, locked until the transaction ends.
pub async fn lock(db: impl PgExecutor<'_>) -> Result<Option<Connection>, sqlx::Error> {
    // SQL-safe: interpolates only the constant SELECT.
    let row: Option<Row> = sqlx::query_as(sqlx::AssertSqlSafe(format!("{SELECT} FOR UPDATE")))
        .fetch_optional(db)
        .await?;
    Ok(row.map(Connection::from))
}

/// `secret` sealed for its column (ADR-0125), as a database error when it
/// cannot be.
pub fn sealed(secret: &Secret) -> Result<String, sqlx::Error> {
    secrets::seal(Column::PsaConnections, secret.expose())
        .map_err(|e| sqlx::Error::Encode(Box::new(e)))
}

/// Records that a request to the PSA worked, as the agent `agent` when
/// known.
pub async fn record_success(
    db: impl PgExecutor<'_>,
    id: Uuid,
    agent: Option<&halopsa::Agent>,
) -> Result<(), sqlx::Error> {
    sqlx::query(
        "UPDATE psa_connections SET last_success_at = now(), last_error = NULL,
             last_error_at = NULL, agent_name = coalesce($2, agent_name),
             agent_id = coalesce($3, agent_id)
         WHERE id = $1",
    )
    .bind(id)
    .bind(agent.map(|a| a.name.as_str()))
    .bind(agent.map(|a| a.id))
    .execute(db)
    .await?;
    Ok(())
}

/// Forgets what belongs to another PSA instance: matches, ids and the
/// asset settings, after the connection's address changed.
pub async fn forget_instance(db: &mut sqlx::PgConnection) -> Result<(), sqlx::Error> {
    for table in [
        "psa_site_links",
        "psa_customer_links",
        "psa_asset_links",
        "psa_asset_types",
    ] {
        // SQL-safe: interpolates only the constant table names above.
        sqlx::query(sqlx::AssertSqlSafe(format!("DELETE FROM {table}")))
            .execute(&mut *db)
            .await?;
    }
    sqlx::query(
        "UPDATE psa_connections SET default_client_id = NULL, default_client_name = NULL,
             default_site_id = NULL, default_site_name = NULL, warning_priority_id = NULL,
             warning_priority_name = NULL, critical_priority_id = NULL,
             critical_priority_name = NULL, agent_id = NULL",
    )
    .execute(&mut *db)
    .await?;
    Ok(())
}

/// Records why a request to the PSA failed, for the connection's page.
pub async fn record_failure(
    db: impl PgExecutor<'_>,
    id: Uuid,
    error: &str,
) -> Result<(), sqlx::Error> {
    sqlx::query("UPDATE psa_connections SET last_error = $2, last_error_at = now() WHERE id = $1")
        .bind(id)
        .bind(error)
        .execute(db)
        .await?;
    Ok(())
}
