//! The tables of the script library (ADR-0189).

use chrono::{DateTime, Utc};
use sqlx::types::Json;
use sqlx::PgExecutor;
use uuid::Uuid;

use super::{Interpreter, Parameter};
use crate::api::Role;

/// A row of `scripts` with its current version.
#[derive(Debug, Clone)]
pub struct Script {
    pub id: Uuid,
    pub customer_id: Option<Uuid>,
    pub name: String,
    pub description: String,
    pub interpreter: Interpreter,
    pub run_role: Role,
    pub ai_allowed: bool,
    pub timeout_s: i32,
    pub version: i32,
    pub created_by: Option<Uuid>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub deleted_at: Option<DateTime<Utc>>,
    /// The current version's text and parameters.
    pub body: String,
    pub parameters: Vec<Parameter>,
}

#[derive(sqlx::FromRow)]
struct ScriptRow {
    id: Uuid,
    customer_id: Option<Uuid>,
    name: String,
    description: String,
    interpreter: String,
    run_role: String,
    ai_allowed: bool,
    timeout_s: i32,
    version: i32,
    created_by: Option<Uuid>,
    created_at: DateTime<Utc>,
    updated_at: DateTime<Utc>,
    deleted_at: Option<DateTime<Utc>>,
    body: String,
    parameters: Json<Vec<Parameter>>,
}

impl From<ScriptRow> for Script {
    fn from(r: ScriptRow) -> Self {
        Self {
            id: r.id,
            customer_id: r.customer_id,
            name: r.name,
            description: r.description,
            // The table's checks allow only these.
            interpreter: Interpreter::parse(&r.interpreter).unwrap_or(Interpreter::Sh),
            run_role: r.run_role.parse().unwrap_or(Role::Administrator),
            ai_allowed: r.ai_allowed,
            timeout_s: r.timeout_s,
            version: r.version,
            created_by: r.created_by,
            created_at: r.created_at,
            updated_at: r.updated_at,
            deleted_at: r.deleted_at,
            body: r.body,
            parameters: r.parameters.0,
        }
    }
}

const SELECT: &str = "SELECT s.id, s.customer_id, s.name, s.description, s.interpreter,
        s.run_role, s.ai_allowed, s.timeout_s, s.version, s.created_by, s.created_at,
        s.updated_at, s.deleted_at, v.body, v.parameters
    FROM scripts s JOIN script_versions v ON v.script_id = s.id AND v.version = s.version";

/// The script `id`, deleted or not.
pub async fn get(db: impl PgExecutor<'_>, id: Uuid) -> Result<Option<Script>, sqlx::Error> {
    // SQL-safe: interpolates only the constant SELECT.
    let row: Option<ScriptRow> =
        sqlx::query_as(sqlx::AssertSqlSafe(format!("{SELECT} WHERE s.id = $1")))
            .bind(id)
            .fetch_optional(db)
            .await?;
    Ok(row.map(Script::from))
}

/// Live scripts of every customer and of the customers `customers`
/// (`all` for every one), by name.
pub async fn list(
    db: impl PgExecutor<'_>,
    all: bool,
    customers: &[Uuid],
) -> Result<Vec<Script>, sqlx::Error> {
    // SQL-safe: interpolates only the constant SELECT.
    let rows: Vec<ScriptRow> = sqlx::query_as(sqlx::AssertSqlSafe(format!(
        "{SELECT} WHERE s.deleted_at IS NULL
           AND ($1 OR s.customer_id IS NULL OR s.customer_id = ANY($2))
         ORDER BY s.name, s.customer_id NULLS FIRST"
    )))
    .bind(all)
    .bind(customers)
    .fetch_all(db)
    .await?;
    Ok(rows.into_iter().map(Script::from).collect())
}

/// One version of a script.
#[derive(Debug, Clone, sqlx::FromRow)]
pub struct Version {
    pub version: i32,
    pub body: String,
    pub parameters: Json<Vec<Parameter>>,
    pub note: String,
    pub created_by: Option<Uuid>,
    pub created_by_name: Option<String>,
    pub created_at: DateTime<Utc>,
}

/// Every version of `script`, newest first.
pub async fn versions(db: impl PgExecutor<'_>, script: Uuid) -> Result<Vec<Version>, sqlx::Error> {
    sqlx::query_as(
        "SELECT v.version, v.body, v.parameters, v.note, v.created_by,
                u.username AS created_by_name, v.created_at
         FROM script_versions v LEFT JOIN users u ON u.id = v.created_by
         WHERE v.script_id = $1 ORDER BY v.version DESC",
    )
    .bind(script)
    .fetch_all(db)
    .await
}

/// The version `version` of `script`.
pub async fn version(
    db: impl PgExecutor<'_>,
    script: Uuid,
    version: i32,
) -> Result<Option<Version>, sqlx::Error> {
    sqlx::query_as(
        "SELECT v.version, v.body, v.parameters, v.note, v.created_by,
                u.username AS created_by_name, v.created_at
         FROM script_versions v LEFT JOIN users u ON u.id = v.created_by
         WHERE v.script_id = $1 AND v.version = $2",
    )
    .bind(script)
    .bind(version)
    .fetch_optional(db)
    .await
}

/// What a new script or a change sets.
#[derive(Debug, Clone)]
pub struct Fields<'a> {
    pub name: &'a str,
    pub description: &'a str,
    pub interpreter: Interpreter,
    pub run_role: Role,
    pub ai_allowed: bool,
    pub timeout_s: i32,
}

/// Inserts a script and its first version.
pub async fn insert(
    tx: &mut sqlx::PgConnection,
    customer: Option<Uuid>,
    fields: &Fields<'_>,
    body: &str,
    parameters: &[Parameter],
    note: &str,
    user: Uuid,
) -> Result<Uuid, sqlx::Error> {
    let id: Uuid = sqlx::query_scalar(
        "INSERT INTO scripts (customer_id, name, description, interpreter, run_role, ai_allowed,
                              timeout_s, created_by)
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8) RETURNING id",
    )
    .bind(customer)
    .bind(fields.name)
    .bind(fields.description)
    .bind(fields.interpreter.as_str())
    .bind(fields.run_role.as_str())
    .bind(fields.ai_allowed)
    .bind(fields.timeout_s)
    .bind(user)
    .fetch_one(&mut *tx)
    .await?;
    add_version(tx, id, 1, body, parameters, note, user).await?;
    Ok(id)
}

async fn add_version(
    tx: &mut sqlx::PgConnection,
    script: Uuid,
    version: i32,
    body: &str,
    parameters: &[Parameter],
    note: &str,
    user: Uuid,
) -> Result<(), sqlx::Error> {
    sqlx::query(
        "INSERT INTO script_versions (script_id, version, body, parameters, note, created_by)
         VALUES ($1, $2, $3, $4, $5, $6)",
    )
    .bind(script)
    .bind(version)
    .bind(body)
    .bind(Json(parameters))
    .bind(note)
    .bind(user)
    .execute(tx)
    .await?;
    Ok(())
}

/// Changes `script`; a new text or new parameters make a new version.
/// Returns the current version after the change.
#[allow(clippy::too_many_arguments)]
pub async fn update(
    tx: &mut sqlx::PgConnection,
    script: &Script,
    fields: &Fields<'_>,
    body: &str,
    parameters: &[Parameter],
    note: &str,
    user: Uuid,
) -> Result<i32, sqlx::Error> {
    let changed = body != script.body || parameters != script.parameters.as_slice();
    let version = if changed {
        script.version + 1
    } else {
        script.version
    };
    if changed {
        add_version(tx, script.id, version, body, parameters, note, user).await?;
    }
    sqlx::query(
        "UPDATE scripts SET name = $2, description = $3, interpreter = $4, run_role = $5,
                ai_allowed = $6, timeout_s = $7, version = $8, updated_at = now()
         WHERE id = $1",
    )
    .bind(script.id)
    .bind(fields.name)
    .bind(fields.description)
    .bind(fields.interpreter.as_str())
    .bind(fields.run_role.as_str())
    .bind(fields.ai_allowed)
    .bind(fields.timeout_s)
    .bind(version)
    .execute(tx)
    .await?;
    Ok(version)
}

/// Marks `script` deleted; its versions and runs stay.
pub async fn delete(db: impl PgExecutor<'_>, script: Uuid) -> Result<(), sqlx::Error> {
    sqlx::query("UPDATE scripts SET deleted_at = now(), ai_allowed = false WHERE id = $1")
        .bind(script)
        .execute(db)
        .await?;
    Ok(())
}

/// How many automation rules run `script`.
pub async fn rules_using(db: impl PgExecutor<'_>, script: Uuid) -> Result<i64, sqlx::Error> {
    sqlx::query_scalar("SELECT count(*) FROM automation_rules WHERE script_id = $1")
        .bind(script)
        .fetch_one(db)
        .await
}

/// Whether `error` is the unique name index's.
pub fn name_taken(error: &sqlx::Error) -> bool {
    matches!(error, sqlx::Error::Database(e) if e.constraint() == Some("scripts_name_key"))
}
