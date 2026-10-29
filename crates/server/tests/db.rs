//! PostgreSQL schema tests. Each `#[sqlx::test]` gets a fresh database with
//! the migrations applied. Without `DATABASE_URL` at build time the tests are
//! reported as ignored (see `build.rs`).

use monitorflow_server::db;
use monitorflow_server::grpc::{DbRelayPolicy, RelayPolicy};
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

const UNIQUE_VIOLATION: &str = "23505";
const FOREIGN_KEY_VIOLATION: &str = "23503";
const CHECK_VIOLATION: &str = "23514";
const INSUFFICIENT_PRIVILEGE: &str = "42501";

fn sqlstate<T: std::fmt::Debug>(result: Result<T, sqlx::Error>) -> String {
    let err = result.expect_err("statement should fail");
    err.as_database_error()
        .and_then(|e| e.code())
        .unwrap_or_else(|| panic!("not a database error: {err}"))
        .into_owned()
}

async fn site(pool: &PgPool, name: &str) -> Uuid {
    sqlx::query_scalar("INSERT INTO sites (name) VALUES ($1) RETURNING id")
        .bind(name)
        .fetch_one(pool)
        .await
        .unwrap()
}

async fn user(pool: &PgPool, username: &str) -> Uuid {
    insert_user(pool, username).await.unwrap()
}

async fn insert_user(pool: &PgPool, username: &str) -> Result<Uuid, sqlx::Error> {
    sqlx::query_scalar(
        "INSERT INTO users (username, password_hash) VALUES ($1, '$argon2id$v=19$m=19456,t=2,p=1$c2FsdA$aGFzaA') RETURNING id",
    )
    .bind(username)
    .fetch_one(pool)
    .await
}

async fn insert_relay(pool: &PgPool, peer_id: &str, site_id: Uuid) -> Result<Uuid, sqlx::Error> {
    sqlx::query_scalar(
        "INSERT INTO relays (peer_id, site_id, name) VALUES ($1, $2, $1) RETURNING id",
    )
    .bind(peer_id)
    .bind(site_id)
    .fetch_one(pool)
    .await
}

async fn insert_server(
    pool: &PgPool,
    peer_id: &str,
    site_id: Uuid,
    relay_id: Option<Uuid>,
) -> Result<Uuid, sqlx::Error> {
    sqlx::query_scalar(
        "INSERT INTO servers (peer_id, site_id, relay_id, hostname, os_family)
         VALUES ($1, $2, $3, $1, 'debian') RETURNING id",
    )
    .bind(peer_id)
    .bind(site_id)
    .bind(relay_id)
    .fetch_one(pool)
    .await
}

async fn insert_token(
    pool: &PgPool,
    hash: &[u8],
    site_id: Uuid,
    created_by: Uuid,
) -> Result<Uuid, sqlx::Error> {
    sqlx::query_scalar(
        "INSERT INTO enrollment_tokens (token_hash, role, peer_id, site_id, created_by, expires_at)
         VALUES ($1, 'agent', 'agent-1', $2, $3, now() + interval '1 hour') RETURNING id",
    )
    .bind(hash)
    .bind(site_id)
    .bind(created_by)
    .fetch_one(pool)
    .await
}

async fn insert_role(
    pool: &PgPool,
    user_id: Uuid,
    role: &str,
    site_id: Option<Uuid>,
) -> Result<(), sqlx::Error> {
    sqlx::query("INSERT INTO user_roles (user_id, role, site_id) VALUES ($1, $2, $3)")
        .bind(user_id)
        .bind(role)
        .bind(site_id)
        .execute(pool)
        .await
        .map(drop)
}

db_tests! {
    #[tokio::test]
    async fn connect_uses_database_url() {
        let url = std::env::var("DATABASE_URL").unwrap().parse().unwrap();
        let pool = db::connect(&url).await.unwrap();
        let one: i32 = sqlx::query_scalar("SELECT 1").fetch_one(&pool).await.unwrap();
        assert_eq!(one, 1);
    }

    #[sqlx::test(migrations = false)]
    async fn migrations_apply_on_empty_database(pool: PgPool) {
        db::migrate(&pool).await.unwrap();
        // A second run is a no-op.
        db::migrate(&pool).await.unwrap();
        let tables: Vec<String> = sqlx::query_scalar(
            "SELECT table_name::text FROM information_schema.tables
             WHERE table_schema = 'public' AND table_name <> '_sqlx_migrations'
             ORDER BY table_name",
        )
        .fetch_all(&pool)
        .await
        .unwrap();
        assert_eq!(
            tables,
            [
                "agent_auto_updates",
                "alert_states",
                "audit_log",
                "auto_rollouts",
                "campaign_snapshots",
                "campaign_targets",
                "campaign_waves",
                "campaign_windows",
                "campaigns",
                "certificates",
                "commands",
                "customers",
                "enrollment_tokens",
                "incident_events",
                "incidents",
                "mirror_runs",
                "mirror_snapshots",
                "mirrors",
                "password_links",
                "proxmox_snapshots",
                "relay_auto_updates",
                "relay_commands",
                "relay_mirrors",
                "relays",
                "remediation_incident_events",
                "remediation_incidents",
                "remediation_loop_leases",
                "remediation_pending_commands",
                "remediation_policies",
                "remediation_rules",
                "sensor_configs",
                "server_packages",
                "server_proxmox_vms",
                "server_settings",
                "servers",
                "sessions",
                "setup_tokens",
                "sites",
                "thresholds",
                "user_roles",
                "users"
            ]
        );
    }

    #[sqlx::test]
    async fn unique_constraints_hold(pool: PgPool) {
        let s = site(&pool, "paris").await;
        assert_eq!(sqlstate(sqlx::query("INSERT INTO sites (name) VALUES ('paris')").execute(&pool).await), UNIQUE_VIOLATION);

        insert_relay(&pool, "relay-1", s).await.unwrap();
        assert_eq!(sqlstate(insert_relay(&pool, "relay-1", s).await), UNIQUE_VIOLATION);

        insert_server(&pool, "agent-1", s, None).await.unwrap();
        assert_eq!(sqlstate(insert_server(&pool, "agent-1", s, None).await), UNIQUE_VIOLATION);

        let u = user(&pool, "Alice").await;
        assert_eq!(sqlstate(insert_user(&pool, "alice").await), UNIQUE_VIOLATION);
        assert_eq!(sqlstate(insert_user(&pool, "ALICE").await), UNIQUE_VIOLATION);

        let hash = [7u8; 32];
        insert_token(&pool, &hash, s, u).await.unwrap();
        assert_eq!(sqlstate(insert_token(&pool, &hash, s, u).await), UNIQUE_VIOLATION);
        // Only SHA-256 sized hashes are accepted.
        assert_eq!(sqlstate(insert_token(&pool, b"plain-token", s, u).await), CHECK_VIOLATION);

        sqlx::query("INSERT INTO certificates (serial, peer_role, peer_id, not_before, not_after) VALUES ('\\x01', 'agent', 'agent-1', now(), now() + interval '1 day')")
            .execute(&pool).await.unwrap();
        assert_eq!(
            sqlstate(sqlx::query("INSERT INTO certificates (serial, peer_role, peer_id, not_before, not_after) VALUES ('\\x01', 'relay', 'relay-1', now(), now() + interval '1 day')").execute(&pool).await),
            UNIQUE_VIOLATION
        );

        // Global roles (NULL site) are unique too.
        insert_role(&pool, u, "viewer", None).await.unwrap();
        assert_eq!(sqlstate(insert_role(&pool, u, "viewer", None).await), UNIQUE_VIOLATION);
        insert_role(&pool, u, "viewer", Some(s)).await.unwrap();
        assert_eq!(sqlstate(insert_role(&pool, u, "viewer", Some(s)).await), UNIQUE_VIOLATION);
    }

    #[sqlx::test]
    async fn enum_columns_reject_unknown_values(pool: PgPool) {
        let s = site(&pool, "paris").await;
        let u = user(&pool, "alice").await;
        assert_eq!(
            sqlstate(sqlx::query("INSERT INTO servers (peer_id, site_id, hostname, os_family) VALUES ('a', $1, 'a', 'macos')").bind(s).execute(&pool).await),
            CHECK_VIOLATION
        );
        assert_eq!(sqlstate(insert_role(&pool, u, "root", None).await), CHECK_VIOLATION);
        assert_eq!(sqlstate(insert_relay(&pool, "bad id/with slash", s).await), CHECK_VIOLATION);
    }

    #[sqlx::test]
    async fn token_can_be_used_once(pool: PgPool) {
        let s = site(&pool, "paris").await;
        let u = user(&pool, "alice").await;
        let token = insert_token(&pool, &[1u8; 32], s, u).await.unwrap();
        let consume = |peer: &'static str| {
            sqlx::query("UPDATE enrollment_tokens SET used_at = now(), used_by_peer_id = $2 WHERE id = $1")
                .bind(token)
                .bind(peer)
                .execute(&pool)
        };
        // The peer that uses a token is the one it names.
        assert_eq!(sqlstate(consume("agent-2").await), CHECK_VIOLATION);
        assert_eq!(consume("agent-1").await.unwrap().rows_affected(), 1);
        assert_eq!(sqlstate(consume("agent-1").await), CHECK_VIOLATION);
        // Clearing used_at to reuse the token is rejected too.
        assert_eq!(
            sqlstate(sqlx::query("UPDATE enrollment_tokens SET used_at = NULL, used_by_peer_id = NULL WHERE id = $1").bind(token).execute(&pool).await),
            CHECK_VIOLATION
        );
        // used_at and used_by_peer_id go together.
        let other = insert_token(&pool, &[2u8; 32], s, u).await.unwrap();
        assert_eq!(
            sqlstate(sqlx::query("UPDATE enrollment_tokens SET used_at = now() WHERE id = $1").bind(other).execute(&pool).await),
            CHECK_VIOLATION
        );
    }

    #[sqlx::test]
    async fn relay_speaks_only_for_agents_of_its_site(pool: PgPool) {
        let paris = site(&pool, "paris").await;
        let lyon = site(&pool, "lyon").await;
        let relay = insert_relay(&pool, "relay-paris", paris).await.unwrap();
        insert_relay(&pool, "relay-lyon", lyon).await.unwrap();
        insert_server(&pool, "agent-paris", paris, Some(relay)).await.unwrap();
        insert_server(&pool, "agent-paris-direct", paris, None).await.unwrap();
        insert_server(&pool, "agent-lyon", lyon, None).await.unwrap();

        assert!(db::relay_may_speak_for(&pool, "relay-paris", "agent-paris").await.unwrap());
        assert!(db::relay_may_speak_for(&pool, "relay-paris", "agent-paris-direct").await.unwrap());
        assert!(!db::relay_may_speak_for(&pool, "relay-paris", "agent-lyon").await.unwrap());
        assert!(!db::relay_may_speak_for(&pool, "relay-lyon", "agent-paris").await.unwrap());
        assert!(!db::relay_may_speak_for(&pool, "relay-paris", "unknown").await.unwrap());
        assert!(!db::relay_may_speak_for(&pool, "unknown", "agent-paris").await.unwrap());
        // An agent is not a relay.
        assert!(!db::relay_may_speak_for(&pool, "agent-paris", "agent-paris").await.unwrap());

        let policy = DbRelayPolicy::new(pool.clone());
        assert!(policy.may_speak_for("relay-paris", "agent-paris").await);
        assert!(!policy.may_speak_for("relay-paris", "agent-lyon").await);

        sqlx::query("UPDATE servers SET revoked_at = now() WHERE peer_id = 'agent-paris'")
            .execute(&pool).await.unwrap();
        assert!(!db::relay_may_speak_for(&pool, "relay-paris", "agent-paris").await.unwrap());
        sqlx::query("UPDATE relays SET revoked_at = now() WHERE peer_id = 'relay-paris'")
            .execute(&pool).await.unwrap();
        assert!(!db::relay_may_speak_for(&pool, "relay-paris", "agent-paris-direct").await.unwrap());
    }

    #[sqlx::test]
    async fn relay_policy_denies_when_the_database_fails(pool: PgPool) {
        let policy = DbRelayPolicy::new(pool.clone());
        pool.close().await;
        assert!(!policy.may_speak_for("relay-paris", "agent-paris").await);
    }

    #[sqlx::test]
    async fn server_relay_must_belong_to_its_site(pool: PgPool) {
        let paris = site(&pool, "paris").await;
        let lyon = site(&pool, "lyon").await;
        let lyon_relay = insert_relay(&pool, "relay-lyon", lyon).await.unwrap();
        assert_eq!(sqlstate(insert_server(&pool, "agent-1", paris, Some(lyon_relay)).await), FOREIGN_KEY_VIOLATION);
    }

    #[sqlx::test]
    async fn audit_log_is_append_only(pool: PgPool) {
        sqlx::query("INSERT INTO audit_log (actor_kind, actor_id, action, details) VALUES ('user', 'alice', 'token.create', '{\"site\": \"paris\"}')")
            .execute(&pool).await.unwrap();
        for statement in [
            "UPDATE audit_log SET action = 'forged'",
            "DELETE FROM audit_log",
            "TRUNCATE audit_log",
        ] {
            assert_eq!(sqlstate(sqlx::query(statement).execute(&pool).await), INSUFFICIENT_PRIVILEGE, "{statement}");
        }
        let action: String = sqlx::query_scalar("SELECT action FROM audit_log").fetch_one(&pool).await.unwrap();
        assert_eq!(action, "token.create");
    }

    #[sqlx::test]
    async fn foreign_keys_reject_orphans(pool: PgPool) {
        let missing = Uuid::new_v4();
        let s = site(&pool, "paris").await;
        let u = user(&pool, "alice").await;

        assert_eq!(sqlstate(insert_relay(&pool, "relay-1", missing).await), FOREIGN_KEY_VIOLATION);
        assert_eq!(sqlstate(insert_server(&pool, "agent-1", missing, None).await), FOREIGN_KEY_VIOLATION);
        assert_eq!(sqlstate(insert_server(&pool, "agent-1", s, Some(missing)).await), FOREIGN_KEY_VIOLATION);
        assert_eq!(sqlstate(insert_token(&pool, &[1u8; 32], missing, u).await), FOREIGN_KEY_VIOLATION);
        assert_eq!(sqlstate(insert_token(&pool, &[1u8; 32], s, missing).await), FOREIGN_KEY_VIOLATION);
        assert_eq!(sqlstate(insert_role(&pool, missing, "viewer", None).await), FOREIGN_KEY_VIOLATION);
        assert_eq!(sqlstate(insert_role(&pool, u, "viewer", Some(missing)).await), FOREIGN_KEY_VIOLATION);

        // A referenced site cannot be deleted while relays or servers use it.
        insert_relay(&pool, "relay-1", s).await.unwrap();
        assert_eq!(sqlstate(sqlx::query("DELETE FROM sites WHERE id = $1").bind(s).execute(&pool).await), FOREIGN_KEY_VIOLATION);
    }
}
