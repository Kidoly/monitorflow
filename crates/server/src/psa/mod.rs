//! The PSA connection (ADR-0164, ADR-0166): MonitorFlow calls a PSA's API
//! with credentials it keeps, HaloPSA first. One connection per
//! installation, managed by global administrators; its client secret is
//! sealed at rest (ADR-0125) and never returned.

pub mod halopsa;

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
    pub last_success_at: Option<DateTime<Utc>>,
    pub last_error: Option<String>,
    pub last_error_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub tickets: TicketSettings,
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
    last_success_at: Option<DateTime<Utc>>,
    last_error: Option<String>,
    last_error_at: Option<DateTime<Utc>>,
    created_at: DateTime<Utc>,
    updated_at: DateTime<Utc>,
    #[sqlx(flatten)]
    tickets: TicketSettings,
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
            last_success_at: row.last_success_at,
            last_error: row.last_error,
            last_error_at: row.last_error_at,
            created_at: row.created_at,
            updated_at: row.updated_at,
            tickets: row.tickets,
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
        agent_name, last_success_at, last_error, last_error_at, created_at, updated_at,
        ticket_type_id, ticket_type_name, category, min_severity, resolved_status_id,
        resolved_status_name
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
    agent: Option<&str>,
) -> Result<(), sqlx::Error> {
    sqlx::query(
        "UPDATE psa_connections SET last_success_at = now(), last_error = NULL,
             last_error_at = NULL, agent_name = coalesce($2, agent_name)
         WHERE id = $1",
    )
    .bind(id)
    .bind(agent)
    .execute(db)
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
