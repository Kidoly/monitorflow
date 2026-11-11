//! The server's database role (ADR-0109) against a real PostgreSQL: what it
//! may not do to the audit log and the remediation history, that its rights
//! are enough for what the server does there, and how the server starts on
//! each kind of role. Ignored without `DATABASE_URL`.

use std::process::{Command, Output};
use std::time::Duration;

use monitorflow_common::config::{DatabaseUrl, ServerConfig};
use monitorflow_server::db::{self, roles, DbError};
use monitorflow_server::remediation::audit::{self, Actor, NewEvent, NewIncident};
use sqlx::PgPool;
use uuid::Uuid;

/// Marks every test in the block as ignored when `DATABASE_URL` is unset.
macro_rules! db_tests {
    ($($item:item)*) => {
        $(
            #[cfg_attr(
                not(db_tests),
                ignore = "DATABASE_URL is not set; run `docker compose -f deploy/docker-compose.dev.yml up -d postgres` and export DATABASE_URL from .env.example"
            )]
            $item
        )*
    };
}

const INSUFFICIENT_PRIVILEGE: &str = "42501";

/// The owner's URL of the test database and the URL of a role of its own,
/// which does not exist yet. Roles belong to the whole PostgreSQL server,
/// so the name follows the database's.
struct Urls {
    owner: DatabaseUrl,
    runtime: DatabaseUrl,
    role: String,
}

async fn urls(pool: &PgPool) -> Urls {
    let (database, role): (String, String) = sqlx::query_as(
        "SELECT current_database()::text, 'mf_app_' || substr(md5(current_database()), 1, 20)",
    )
    .fetch_one(pool)
    .await
    .unwrap();
    let base = std::env::var("DATABASE_URL").unwrap();
    let (server, _) = base.rsplit_once('/').unwrap();
    let (_, host) = server.rsplit_once('@').unwrap();
    // Left by a run that failed before the end.
    drop_role(pool, &role).await;
    Urls {
        owner: format!("{server}/{database}").parse().unwrap(),
        runtime: format!("postgres://{role}:pw-{role}@{host}/{database}")
            .parse()
            .unwrap(),
        role,
    }
}

async fn drop_role(owner: &PgPool, role: &str) {
    let exists: bool =
        sqlx::query_scalar("SELECT EXISTS (SELECT FROM pg_roles WHERE rolname = $1)")
            .bind(role)
            .fetch_one(owner)
            .await
            .unwrap();
    if exists {
        for sql in [
            format!("DROP OWNED BY \"{role}\""),
            format!("DROP ROLE \"{role}\""),
        ] {
            sqlx::query(sqlx::AssertSqlSafe(sql))
                .execute(owner)
                .await
                .unwrap();
        }
    }
}

/// The schema and the role, then a pool of the role.
async fn runtime_pool(urls: &Urls) -> PgPool {
    db::migrate_as_owner(&urls.owner, &urls.runtime)
        .await
        .unwrap();
    db::connect(&urls.runtime).await.unwrap()
}

/// SQLSTATE and message of a statement that must fail.
async fn refused(pool: &PgPool, sql: &'static str) -> (String, String) {
    match sqlx::query(sql).execute(pool).await {
        Err(sqlx::Error::Database(e)) => (e.code().unwrap().into_owned(), e.message().to_owned()),
        other => panic!("{sql}: expected a database error, got {other:?}"),
    }
}

/// A remediation incident with one event, written through `pool`.
async fn incident(pool: &PgPool) -> Uuid {
    let customer: Uuid =
        sqlx::query_scalar("INSERT INTO customers (name) VALUES ('acme') RETURNING id")
            .fetch_one(pool)
            .await
            .unwrap();
    let site: Uuid = sqlx::query_scalar(
        "INSERT INTO sites (name, customer_id) VALUES ('paris', $1) RETURNING id",
    )
    .bind(customer)
    .fetch_one(pool)
    .await
    .unwrap();
    let server: Uuid = sqlx::query_scalar(
        "INSERT INTO servers (peer_id, site_id, hostname, os_family)
         VALUES ('agent-1', $1, 'agent-1', 'debian') RETURNING id",
    )
    .bind(site)
    .fetch_one(pool)
    .await
    .unwrap();
    let id = audit::open(
        pool,
        &NewIncident {
            server_id: server,
            alert_incident_id: None,
            metric: "monitorflow_disk_used_ratio".into(),
            instance: "/".into(),
            max_commands: 10,
            max_duration: Duration::from_secs(1800),
        },
    )
    .await
    .unwrap();
    audit::append(pool, id, NewEvent::note(Actor::System, "opened"))
        .await
        .unwrap();
    id
}

fn config(url: &DatabaseUrl) -> ServerConfig {
    ServerConfig {
        database_url: url.clone(),
        ..ServerConfig::default()
    }
}

/// Runs the server binary with an empty configuration file.
fn server(env: &[(&str, &str)], args: &[&str]) -> Output {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("server.toml");
    std::fs::write(&file, "").unwrap();
    Command::new(env!("CARGO_BIN_EXE_monitorflow-server"))
        .arg("--config")
        .arg(&file)
        .args(args)
        .envs(env.iter().copied())
        .env("MONITORFLOW_DATA_DIR", dir.path())
        .output()
        .unwrap()
}

db_tests! {
    #[sqlx::test(migrations = false)]
    async fn the_server_role_cannot_rewrite_the_audit_tables(owner: PgPool) {
        let urls = urls(&owner).await;
        let app = runtime_pool(&urls).await;

        let status = roles::status(&app).await.unwrap();
        assert_eq!(status.name, urls.role);
        assert!(!status.privileged(), "{status:?}");
        assert!(roles::status(&owner).await.unwrap().privileged());

        // What the server does works: audit rows, a history and its state.
        sqlx::query(
            "INSERT INTO audit_log (actor_kind, actor_id, action) VALUES ('system', 'test', 'x.y')",
        )
        .execute(&app)
        .await
        .unwrap();
        let id = incident(&app).await;
        sqlx::query("UPDATE remediation_incidents SET state = 'running' WHERE id = $1")
            .bind(id)
            .execute(&app)
            .await
            .unwrap();
        let rows: i64 = sqlx::query_scalar(
            "SELECT (SELECT count(*) FROM audit_log) + (SELECT count(*) FROM remediation_incident_events)",
        )
        .fetch_one(&app)
        .await
        .unwrap();
        assert_eq!(rows, 2);

        // Refused by the role's rights, before any trigger: the triggers
        // raise the same SQLSTATE with another message.
        for sql in [
            "UPDATE audit_log SET action = 'forged'",
            "DELETE FROM audit_log",
            "TRUNCATE audit_log",
            "UPDATE remediation_incident_events SET kind = 'note'",
            "DELETE FROM remediation_incident_events",
            "TRUNCATE remediation_incident_events CASCADE",
            "DELETE FROM remediation_incidents",
            "TRUNCATE remediation_incidents CASCADE",
            "UPDATE incident_events SET kind = 'opened'",
            "DELETE FROM _sqlx_migrations",
        ] {
            let (code, message) = refused(&app, sql).await;
            assert_eq!(code, INSUFFICIENT_PRIVILEGE, "{sql}: {message}");
            assert!(message.starts_with("permission denied for table"), "{sql}: {message}");
        }

        // Nor can it take the protections away.
        for sql in [
            "ALTER TABLE audit_log DISABLE TRIGGER USER",
            "ALTER TABLE audit_log DISABLE TRIGGER audit_log_no_update_delete",
            "ALTER TABLE remediation_incident_events DISABLE TRIGGER USER",
            "ALTER TABLE remediation_incidents DISABLE TRIGGER USER",
            "DROP TRIGGER audit_log_no_update_delete ON audit_log",
            "DROP TABLE audit_log",
            "ALTER TABLE audit_log OWNER TO CURRENT_USER",
        ] {
            let (code, message) = refused(&app, sql).await;
            assert_eq!(code, INSUFFICIENT_PRIVILEGE, "{sql}: {message}");
            assert!(
                message.starts_with("must be owner of") || message.starts_with("permission denied"),
                "{sql}: {message}"
            );
        }
        for sql in [
            "SET session_replication_role = replica",
            "CREATE OR REPLACE FUNCTION audit_log_append_only() RETURNS trigger
             LANGUAGE plpgsql AS $$ BEGIN RETURN NEW; END $$",
            "CREATE TABLE scratch (id int)",
            "COPY audit_log (actor_id) FROM PROGRAM 'id'",
            "COPY audit_log TO '/tmp/audit'",
        ] {
            let (code, message) = refused(&app, sql).await;
            assert_eq!(code, INSUFFICIENT_PRIVILEGE, "{sql}: {message}");
        }

        // Nothing changed, and the chain agrees.
        let action: String = sqlx::query_scalar("SELECT action FROM audit_log")
            .fetch_one(&app)
            .await
            .unwrap();
        assert_eq!(action, "x.y");
        assert!(audit::verify(&app, id).await.unwrap().valid);

        app.close().await;
        drop_role(&owner, &urls.role).await;
    }

    #[sqlx::test(migrations = false)]
    async fn rights_are_given_again_and_cover_later_tables(owner: PgPool) {
        let urls = urls(&owner).await;
        let app = runtime_pool(&urls).await;

        // What a later migration does, as the owner.
        sqlx::query("CREATE TABLE later (id bigserial PRIMARY KEY, note text)")
            .execute(&owner)
            .await
            .unwrap();
        sqlx::query("INSERT INTO later (note) VALUES ('a')")
            .execute(&app)
            .await
            .unwrap();
        sqlx::query("UPDATE later SET note = 'b'").execute(&app).await.unwrap();
        let (code, _) = refused(&app, "TRUNCATE later").await;
        assert_eq!(code, INSUFFICIENT_PRIVILEGE);

        // A right given by hand does not survive the next start, and a
        // second run changes nothing else.
        let grant = format!("GRANT UPDATE, DELETE, TRUNCATE ON audit_log TO \"{}\"", urls.role);
        sqlx::query(sqlx::AssertSqlSafe(grant)).execute(&owner).await.unwrap();
        db::migrate_as_owner(&urls.owner, &urls.runtime).await.unwrap();
        for sql in ["DELETE FROM audit_log", "TRUNCATE audit_log"] {
            let (code, message) = refused(&app, sql).await;
            assert_eq!(code, INSUFFICIENT_PRIVILEGE, "{sql}");
            assert!(message.starts_with("permission denied for table"), "{sql}: {message}");
        }
        sqlx::query("DELETE FROM later").execute(&app).await.unwrap();

        // The password follows the URL.
        let renamed: DatabaseUrl = urls
            .runtime
            .expose()
            .replace(&format!("pw-{}", urls.role), "another")
            .parse()
            .unwrap();
        db::migrate_as_owner(&urls.owner, &renamed).await.unwrap();
        assert!(matches!(db::connect(&urls.runtime).await, Err(DbError::Connect(_))));
        db::connect(&renamed).await.unwrap().close().await;

        app.close().await;
        drop_role(&owner, &urls.role).await;
    }

    #[sqlx::test(migrations = false)]
    async fn the_owner_cannot_be_the_server_role(owner: PgPool) {
        let urls = urls(&owner).await;
        let error = db::migrate_as_owner(&urls.owner, &urls.owner).await.unwrap_err();
        assert!(matches!(error, DbError::Role(_)), "{error}");
        assert!(!error.to_string().contains("monitorflow:monitorflow"), "{error}");
    }

    #[sqlx::test(migrations = false)]
    async fn the_server_opens_its_pool_by_the_role_it_has(owner: PgPool) {
        let urls = urls(&owner).await;

        // One URL of the owner, as before ADR-0109: it migrates, unless told
        // to refuse.
        let strict = ServerConfig {
            database_require_restricted_role: true,
            ..config(&urls.owner)
        };
        let error = db::open(&strict, true).await.unwrap_err();
        assert!(matches!(error, DbError::Role(_)), "{error}");
        db::open(&config(&urls.owner), true).await.unwrap().close().await;
        let tables: i64 = sqlx::query_scalar("SELECT count(*) FROM audit_log")
            .fetch_one(&owner)
            .await
            .unwrap();
        assert_eq!(tables, 0);

        // Both URLs: the role appears and the pool is its own.
        let alone = ServerConfig {
            database_require_restricted_role: true,
            ..config(&urls.runtime)
        };
        let both = ServerConfig {
            database_migrate_url: Some(urls.owner.clone()),
            ..alone.clone()
        };
        let pool = db::open(&both, true).await.unwrap();
        assert_eq!(roles::status(&pool).await.unwrap().name, urls.role);
        pool.close().await;

        // The role's URL alone: it checks the schema and changes nothing.
        db::open(&alone, true).await.unwrap().close().await;

        // A migration this build does not know: a newer server was there.
        sqlx::query(
            "INSERT INTO _sqlx_migrations (version, description, success, checksum, execution_time)
             VALUES (99990101000000, 'from the future', true, '\\x00', 0)",
        )
        .execute(&owner)
        .await
        .unwrap();
        let error = db::open(&alone, true).await.unwrap_err().to_string();
        assert!(error.contains("99990101000000") && error.contains("newer server"), "{error}");
        sqlx::query("DELETE FROM _sqlx_migrations WHERE version = 99990101000000")
            .execute(&owner)
            .await
            .unwrap();

        // A migration that is missing: the owner has not migrated yet.
        let last: i64 = sqlx::query_scalar(
            "DELETE FROM _sqlx_migrations
             WHERE version = (SELECT max(version) FROM _sqlx_migrations) RETURNING version",
        )
        .fetch_one(&owner)
        .await
        .unwrap();
        let error = db::open(&alone, true).await.unwrap_err().to_string();
        assert!(
            error.contains(&last.to_string()) && error.contains("is not applied"),
            "{error}"
        );

        drop_role(&owner, &urls.role).await;
    }

    #[sqlx::test(migrations = false)]
    async fn the_migrate_command_prepares_the_database_for_the_server(owner: PgPool) {
        let urls = urls(&owner).await;
        let (owner_url, runtime_url) =
            (urls.owner.expose().to_owned(), urls.runtime.expose().to_owned());

        let output = tokio::task::spawn_blocking(move || {
            let missing = server(&[("MONITORFLOW_DATABASE_URL", &runtime_url)], &["migrate"]);
            let migrated = server(
                &[
                    ("MONITORFLOW_DATABASE_URL", &runtime_url),
                    ("MONITORFLOW_DATABASE_MIGRATE_URL", &owner_url),
                ],
                &["migrate"],
            );
            let command = server(
                &[
                    ("MONITORFLOW_DATABASE_URL", &runtime_url),
                    ("MONITORFLOW_DATABASE_REQUIRE_RESTRICTED_ROLE", "true"),
                ],
                &["customer", "add", "--name", "acme"],
            );
            [missing, migrated, command]
        })
        .await
        .unwrap();

        let [missing, migrated, command] = output;
        assert!(!missing.status.success());
        let stderr = String::from_utf8_lossy(&missing.stderr);
        assert!(stderr.contains("database_migrate_url is not set"), "{stderr}");
        assert!(migrated.status.success(), "{migrated:?}");
        assert!(command.status.success(), "{command:?}");
        assert!(String::from_utf8_lossy(&command.stdout).starts_with("created customer acme"));

        drop_role(&owner, &urls.role).await;
    }
}
