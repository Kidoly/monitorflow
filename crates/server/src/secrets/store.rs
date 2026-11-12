//! Sealing the values stored before ADR-0110, once per column.

use sqlx::{PgPool, Row};

use super::{keyring, Column, SecretsError};

/// The columns whose values the server keeps sealed.
pub const SEALED: &[Column] = &[Column::NotificationChannels, Column::UserContacts];

#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    #[error(transparent)]
    Secrets(#[from] SecretsError),
    #[error(transparent)]
    Database(#[from] sqlx::Error),
}

/// Seals every value of each column of [`SEALED`] not yet listed in
/// `sealed_columns`, then lists it, in one transaction under a lock that
/// makes other central servers wait. A value that already opens is left
/// as it is. Returns the columns it sealed.
pub async fn seal_stored(pool: &PgPool) -> Result<Vec<Column>, StoreError> {
    let keyring = keyring()?;
    let mut tx = pool.begin().await?;
    sqlx::query("SELECT pg_advisory_xact_lock(hashtext('monitorflow.sealed_columns'))")
        .execute(&mut *tx)
        .await?;
    let listed: Vec<String> = sqlx::query_scalar("SELECT name FROM sealed_columns")
        .fetch_all(&mut *tx)
        .await?;
    let mut sealed = Vec::new();
    for &column in SEALED {
        if listed.iter().any(|name| name == column.name()) {
            continue;
        }
        let (table, field) = column.parts();
        // SQL-safe: interpolates only the constant names of `Column`.
        let rows = sqlx::query(sqlx::AssertSqlSafe(format!(
            "SELECT ctid::text AS row, {field} AS value FROM {table}
             WHERE {field} IS NOT NULL FOR UPDATE"
        )))
        .fetch_all(&mut *tx)
        .await?;
        let mut count = 0;
        for row in rows {
            let (at, value): (String, String) = (row.try_get("row")?, row.try_get("value")?);
            if keyring.open(column, &value).is_ok() {
                continue;
            }
            let value = keyring.seal(column, &value)?;
            // SQL-safe: interpolates only the constant names of `Column`.
            sqlx::query(sqlx::AssertSqlSafe(format!(
                "UPDATE {table} SET {field} = $1 WHERE ctid = $2::tid"
            )))
            .bind(value)
            .bind(at)
            .execute(&mut *tx)
            .await?;
            count += 1;
        }
        sqlx::query("INSERT INTO sealed_columns (name) VALUES ($1)")
            .bind(column.name())
            .execute(&mut *tx)
            .await?;
        tracing::info!(
            column = column.name(),
            values = count,
            "sealed stored secrets"
        );
        sealed.push(column);
    }
    tx.commit().await?;
    for &column in SEALED {
        report_unreadable(pool, column).await?;
    }
    Ok(sealed)
}

/// Logs, once at startup, how many values of `column` do not open: a key
/// missing from this server shows here rather than only at the next
/// delivery.
async fn report_unreadable(pool: &PgPool, column: Column) -> Result<(), StoreError> {
    let keyring = keyring()?;
    let (table, field) = column.parts();
    // SQL-safe: interpolates only the constant names of `Column`.
    let values: Vec<String> = sqlx::query_scalar(sqlx::AssertSqlSafe(format!(
        "SELECT {field} FROM {table} WHERE {field} IS NOT NULL"
    )))
    .fetch_all(pool)
    .await?;
    let failed: Vec<String> = values
        .iter()
        .filter_map(|v| keyring.open(column, v).err())
        .map(|e| e.to_string())
        .collect();
    if let Some(first) = failed.first() {
        tracing::error!(
            column = column.name(),
            count = failed.len(),
            error = %first,
            "stored secrets do not open; what uses them fails until the key is restored or they are entered again"
        );
    }
    Ok(())
}
