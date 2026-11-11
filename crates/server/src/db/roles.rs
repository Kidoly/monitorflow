//! The two database roles of ADR-0109. The owner role runs migrations and
//! owns every table. The server runs on a second role that owns nothing and
//! is not a superuser, so it cannot switch off the triggers that keep the
//! audit log and the remediation history append-only.

use monitorflow_common::config::DatabaseUrl;
use percent_encoding::percent_decode_str;
use sqlx::{Connection, PgConnection, PgPool};

use super::{DbError, MIGRATOR};

/// Rights the server's role does not get, on top of never getting TRUNCATE
/// or ownership. A migration that adds an append-only table adds it here.
pub const WITHHELD: &[(&str, &str)] = &[
    ("audit_log", "UPDATE, DELETE"),
    ("remediation_incident_events", "UPDATE, DELETE"),
    ("remediation_incidents", "DELETE"),
    ("incident_events", "UPDATE"),
    ("_sqlx_migrations", "INSERT, UPDATE, DELETE"),
];

/// What the role of a connection may do to the schema.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RoleStatus {
    pub name: String,
    pub superuser: bool,
    /// It owns the audit log, or may create it while it does not exist.
    pub owner: bool,
}

impl RoleStatus {
    /// Whether the role can undo the append-only protections.
    pub fn privileged(&self) -> bool {
        self.superuser || self.owner
    }
}

pub async fn status(pool: &PgPool) -> Result<RoleStatus, DbError> {
    let (name, superuser, owner) = sqlx::query_as(
        "SELECT current_user::text,
                (SELECT rolsuper FROM pg_roles WHERE rolname = current_user),
                coalesce(
                    (SELECT pg_has_role(current_user, c.relowner, 'USAGE')
                     FROM pg_class c WHERE c.oid = to_regclass('public.audit_log')),
                    has_schema_privilege(current_user, 'public', 'CREATE'))",
    )
    .fetch_one(pool)
    .await?;
    Ok(RoleStatus {
        name,
        superuser,
        owner,
    })
}

/// Checks that the database has exactly the migrations of this build,
/// without changing anything: what a role that cannot migrate does instead
/// of [`super::migrate`].
pub async fn verify_schema(pool: &PgPool) -> Result<(), DbError> {
    let exists: bool =
        sqlx::query_scalar("SELECT to_regclass('public._sqlx_migrations') IS NOT NULL")
            .fetch_one(pool)
            .await?;
    if !exists {
        return Err(DbError::Schema(
            "the database has no schema yet; run `monitorflow-server migrate` with database_migrate_url".into(),
        ));
    }
    let applied: Vec<(i64, bool, Vec<u8>)> =
        sqlx::query_as("SELECT version, success, checksum FROM _sqlx_migrations ORDER BY version")
            .fetch_all(pool)
            .await?;
    for (version, success, _) in &applied {
        if !success {
            return Err(DbError::Schema(format!(
                "migration {version} failed part way; restore a backup or fix it by hand"
            )));
        }
        if !MIGRATOR.iter().any(|m| m.version == *version) {
            return Err(DbError::Schema(format!(
                "the database has migration {version}, which this version does not know: it was used by a newer server"
            )));
        }
    }
    for migration in MIGRATOR
        .iter()
        .filter(|m| !m.migration_type.is_down_migration())
    {
        match applied.iter().find(|(v, ..)| *v == migration.version) {
            None => {
                return Err(DbError::Schema(format!(
                    "migration {} is not applied; run `monitorflow-server migrate` with database_migrate_url (the first central server does when it starts)",
                    migration.version
                )))
            }
            Some((_, _, checksum)) if checksum[..] != migration.checksum[..] => {
                return Err(DbError::Schema(format!(
                    "migration {} was changed after it was applied",
                    migration.version
                )))
            }
            Some(_) => {}
        }
    }
    Ok(())
}

/// The role name and password in the userinfo of `url`, percent-decoded.
/// Errors never echo the URL.
pub fn credentials(url: &DatabaseUrl) -> Result<(String, String), DbError> {
    let missing = || DbError::Role("database_url must name a role and its password".into());
    let rest = url.expose().split_once("://").ok_or_else(missing)?.1;
    let authority = rest.split(['/', '?', '#']).next().unwrap_or_default();
    let (userinfo, _) = authority.rsplit_once('@').ok_or_else(missing)?;
    let (name, password) = userinfo.split_once(':').ok_or_else(missing)?;
    let decode = |part: &str| {
        percent_decode_str(part)
            .decode_utf8()
            .map(|s| s.into_owned())
            .map_err(|_| missing())
    };
    let (name, password) = (decode(name)?, decode(password)?);
    if name.is_empty() || password.is_empty() {
        return Err(missing());
    }
    Ok((name, password))
}

/// Creates the server's role if needed, sets its password and gives it its
/// rights on every table and sequence there is, and on those the calling
/// role creates later. `owner` is a connection of the role that owns the
/// schema. Running it again changes nothing; it runs after every migration.
pub async fn ensure_runtime_role(
    owner: &mut PgConnection,
    name: &str,
    password: &str,
) -> Result<(), DbError> {
    let mut tx = owner.begin().await?;
    // One at a time: two servers starting together would otherwise update
    // the same catalog rows.
    sqlx::query("SELECT pg_advisory_xact_lock(hashtext('monitorflow.runtime_role'))")
        .execute(&mut *tx)
        .await?;
    let (same, member, owns): (bool, bool, bool) = sqlx::query_as(
        "SELECT $1 = current_user,
                coalesce((SELECT pg_has_role(r.oid, current_user, 'MEMBER')
                          FROM pg_roles r WHERE r.rolname = $1), false),
                EXISTS (SELECT 1 FROM pg_class c
                        JOIN pg_roles r ON r.oid = c.relowner
                        WHERE r.rolname = $1 AND c.relnamespace = 'public'::regnamespace)",
    )
    .bind(name)
    .fetch_one(&mut *tx)
    .await?;
    if same || member || owns {
        return Err(DbError::Role(format!(
            "the role {name:?} of database_url must not be the role of database_migrate_url, one of its members, or the owner of a table"
        )));
    }
    // The name and the password travel as bound values, so no statement
    // text holds them, whatever logs statements.
    sqlx::query(
        "SELECT set_config('monitorflow.runtime_role', $1, true),
                set_config('monitorflow.runtime_password', $2, true)",
    )
    .bind(name)
    .bind(password)
    .execute(&mut *tx)
    .await?;
    let withheld: String = WITHHELD
        .iter()
        .map(|(table, rights)| {
            format!(
                "IF to_regclass('public.{table}') IS NOT NULL THEN
                     EXECUTE format('REVOKE {rights} ON {table} FROM %I', app);
                 END IF;\n"
            )
        })
        .collect();
    // Built from the constants of `WITHHELD` alone.
    sqlx::query(sqlx::AssertSqlSafe(format!(
        "DO $$
         DECLARE
             app text := current_setting('monitorflow.runtime_role');
         BEGIN
             IF NOT EXISTS (SELECT FROM pg_roles WHERE rolname = app) THEN
                 EXECUTE format('CREATE ROLE %I', app);
             END IF;
             EXECUTE format(
                 'ALTER ROLE %I LOGIN NOSUPERUSER NOCREATEDB NOCREATEROLE NOREPLICATION NOBYPASSRLS PASSWORD %L',
                 app, current_setting('monitorflow.runtime_password'));
             REVOKE CREATE ON SCHEMA public FROM PUBLIC;
             EXECUTE format('REVOKE ALL ON DATABASE %I FROM PUBLIC', current_database());
             EXECUTE format('REVOKE ALL ON DATABASE %I FROM %I', current_database(), app);
             EXECUTE format('GRANT CONNECT ON DATABASE %I TO %I', current_database(), app);
             EXECUTE format('REVOKE ALL ON SCHEMA public FROM %I', app);
             EXECUTE format('GRANT USAGE ON SCHEMA public TO %I', app);
             EXECUTE format('REVOKE ALL ON ALL TABLES IN SCHEMA public FROM %I', app);
             EXECUTE format('REVOKE ALL ON ALL SEQUENCES IN SCHEMA public FROM %I', app);
             EXECUTE format(
                 'GRANT SELECT, INSERT, UPDATE, DELETE ON ALL TABLES IN SCHEMA public TO %I', app);
             EXECUTE format('GRANT USAGE, SELECT ON ALL SEQUENCES IN SCHEMA public TO %I', app);
             -- Tables and sequences this role creates later, in a migration.
             EXECUTE format(
                 'ALTER DEFAULT PRIVILEGES IN SCHEMA public
                  GRANT SELECT, INSERT, UPDATE, DELETE ON TABLES TO %I', app);
             EXECUTE format(
                 'ALTER DEFAULT PRIVILEGES IN SCHEMA public
                  GRANT USAGE, SELECT ON SEQUENCES TO %I', app);
             {withheld}
         END $$"
    )))
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;
    tracing::info!(role = name, "database role of the server is in place");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn url(text: &str) -> DatabaseUrl {
        text.parse().unwrap()
    }

    #[test]
    fn credentials_come_from_the_userinfo() {
        let (name, password) = credentials(&url(
            "postgres://monitorflow_app:p%40ss%3Aw@db:5432/mf?sslmode=verify-full",
        ))
        .unwrap();
        assert_eq!(
            (name.as_str(), password.as_str()),
            ("monitorflow_app", "p@ss:w")
        );
    }

    #[test]
    fn credentials_are_required_and_never_echoed() {
        for text in [
            "postgres://db/mf",
            "postgres://app@db/mf",
            "postgres://app:@db/mf",
            "postgres://:hunter2@db/mf",
            "hunter2",
        ] {
            let error = credentials(&url(text)).unwrap_err().to_string();
            assert!(error.contains("must name a role"), "{error}");
            assert!(!error.contains("hunter2"), "{error}");
        }
    }

    #[test]
    fn a_superuser_or_an_owner_is_privileged() {
        let status = |superuser, owner| RoleStatus {
            name: "r".into(),
            superuser,
            owner,
        };
        assert!(status(true, false).privileged());
        assert!(status(false, true).privileged());
        assert!(!status(false, false).privileged());
    }
}
