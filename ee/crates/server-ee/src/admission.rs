//! Letting an additional central server start (ADR-0100).
//!
//! The first central server never comes here: it always runs. One set up
//! as additional (`MONITORFLOW_ADDITIONAL_INSTANCE`) needs, at its first
//! start, a valid license with room for it; it is then recorded in
//! `server_instance_admissions` by the name of its host and starts again
//! after that whatever the license says, so expiry stops adding central
//! servers, never the ones running.

use chrono::{DateTime, NaiveDate, Utc};
use serde::Serialize;
use sqlx::PgConnection;

use crate::license::{License, Verifier};
use crate::store::{self, State, Status};

/// Where to get a license, in every refusal.
pub const HOW_TO_GET_ONE: &str = "To get one, see https://github.com/Kidoly/monitorflow#licence";

/// Why an additional central server may not be added. The messages are
/// for the person installing it.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum Refusal {
    #[error(
        "adding a central server needs a MonitorFlow Enterprise license. Install its license key \
         under Administration > License, then try again. {HOW_TO_GET_ONE}. The first central \
         server keeps running"
    )]
    NoLicense,
    #[error(
        "the license expired on {0}: central servers already added keep running, but no other \
         can be added. Install a new license key under Administration > License. {HOW_TO_GET_ONE}"
    )]
    Expired(NaiveDate),
    #[error(
        "the license key no longer verifies ({0}). Install it again under Administration > \
         License. {HOW_TO_GET_ONE}"
    )]
    Invalid(String),
    #[error(
        "the license allows {allowed} central servers, the first one included, and there are \
         already {used}. Remove one you no longer use under Administration > License, or ask for \
         a license with more. {HOW_TO_GET_ONE}"
    )]
    Full { allowed: u32, used: u32 },
}

/// Why it may start.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Admission {
    /// Added before: it starts whatever the license says now.
    Again,
    /// Added now, under this license.
    New(License),
}

/// A central server added under a license.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, sqlx::FromRow)]
pub struct Added {
    pub name: String,
    pub admitted_at: DateTime<Utc>,
    /// Whether an instance of that name has a recent heartbeat.
    pub running: bool,
}

/// Whether `name` may start as an additional central server on `today`,
/// recording it when it is new. Run in a transaction the caller commits,
/// with its audit entry: the table is locked until then, so two servers
/// joining at once cannot both take the last place.
pub async fn admit(
    db: &mut PgConnection,
    verifier: &Verifier,
    name: &str,
    today: NaiveDate,
) -> Result<Result<Admission, Refusal>, sqlx::Error> {
    sqlx::query("LOCK TABLE server_instance_admissions IN SHARE ROW EXCLUSIVE MODE")
        .execute(&mut *db)
        .await?;
    let known: bool = sqlx::query_scalar(
        "SELECT EXISTS (SELECT 1 FROM server_instance_admissions WHERE name = $1)",
    )
    .bind(name)
    .fetch_one(&mut *db)
    .await?;
    if known {
        return Ok(Ok(Admission::Again));
    }
    let license = match check(db, verifier, today).await? {
        Ok(license) => license,
        Err(refusal) => return Ok(Err(refusal)),
    };
    sqlx::query("INSERT INTO server_instance_admissions (name, license_id) VALUES ($1, $2)")
        .bind(name)
        .bind(license.id)
        .execute(&mut *db)
        .await?;
    Ok(Ok(Admission::New(license)))
}

/// Whether one more central server may be added on `today`, without
/// adding it: for the installer, before it changes anything.
pub async fn check(
    db: &mut PgConnection,
    verifier: &Verifier,
    today: NaiveDate,
) -> Result<Result<License, Refusal>, sqlx::Error> {
    let installed = sqlx::query_as("SELECT text, installed_at, installed_by FROM license")
        .fetch_optional(&mut *db)
        .await?;
    let status = Status::of(installed, verifier, today);
    let license = match (status.state, status.license) {
        (State::Valid | State::ExpiresSoon, Some(license)) => license,
        (State::Expired, Some(license)) => return Ok(Err(Refusal::Expired(license.expires))),
        (State::Invalid, _) => {
            return Ok(Err(Refusal::Invalid(status.problem.unwrap_or_default())))
        }
        _ => return Ok(Err(Refusal::NoLicense)),
    };
    let added: i64 = sqlx::query_scalar("SELECT count(*) FROM server_instance_admissions")
        .fetch_one(&mut *db)
        .await?;
    // The first central server, those added, and this one.
    let used = u32::try_from(added).unwrap_or(u32::MAX).saturating_add(1);
    if used >= license.server_instances {
        return Ok(Err(Refusal::Full {
            allowed: license.server_instances,
            used,
        }));
    }
    Ok(Ok(license))
}

/// The central servers added, oldest first, and whether each runs.
pub async fn added(db: &mut PgConnection) -> Result<Vec<Added>, sqlx::Error> {
    sqlx::query_as(
        "SELECT a.name, a.admitted_at, EXISTS (
             SELECT 1 FROM server_instances i
             WHERE i.hostname = a.name AND i.heartbeat_at > now() - interval '30 seconds'
         ) AS running
         FROM server_instance_admissions a ORDER BY a.admitted_at, a.name",
    )
    .fetch_all(&mut *db)
    .await
}

/// Frees the place of `name`; false if it was not added. A server still
/// running under that name keeps running until it restarts, when it needs
/// a place again.
pub async fn remove(db: &mut PgConnection, name: &str) -> Result<bool, sqlx::Error> {
    let done = sqlx::query("DELETE FROM server_instance_admissions WHERE name = $1")
        .bind(name)
        .execute(db)
        .await?;
    Ok(done.rows_affected() > 0)
}

pub fn today() -> NaiveDate {
    store::today()
}
