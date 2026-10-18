//! Enrollment tokens in PostgreSQL. Without `DATABASE_URL` at build time the
//! tests are reported as ignored (see `build.rs`).

use std::time::Duration;

use monitorflow_common::tls::PeerRole;
use monitorflow_server::enroll::token::{self, Creator, TokenError, TokenRequest};
use sqlx::PgPool;
use uuid::Uuid;

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

const CA: [u8; 32] = [9; 32];

fn request(site_id: Uuid, peer_id: &str) -> TokenRequest {
    TokenRequest {
        role: PeerRole::Agent,
        peer_id: peer_id.into(),
        site_id,
        ttl: token::DEFAULT_TTL,
        creator: Creator::Cli,
    }
}

async fn site(pool: &PgPool) -> Uuid {
    token::site_id(pool, "paris", true).await.unwrap().unwrap()
}

db_tests! {
    #[sqlx::test]
    async fn sites_are_created_only_on_request(pool: PgPool) {
        assert_eq!(token::site_id(&pool, "paris", false).await.unwrap(), None);
        let created = site(&pool).await;
        assert_eq!(token::site_id(&pool, "paris", false).await.unwrap(), Some(created));
        assert_eq!(site(&pool).await, created);
    }

    #[sqlx::test]
    async fn create_stores_only_the_hash_and_audits(pool: PgPool) {
        let site_id = site(&pool).await;
        let created = token::create(&pool, CA, &request(site_id, "web-01")).await.unwrap();
        assert_eq!(created.token.ca_fingerprint(), &CA);

        let (hash, role, peer_id, created_by, ttl): (Vec<u8>, String, String, Option<Uuid>, f64) =
            sqlx::query_as(
                "SELECT token_hash, role, peer_id, created_by,
                        extract(epoch FROM expires_at - created_at)::float8
                 FROM enrollment_tokens WHERE id = $1",
            )
            .bind(created.id)
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(hash, token::hash(&created.token));
        assert_ne!(hash.as_slice(), created.token.secret());
        assert_eq!((role.as_str(), peer_id.as_str(), created_by), ("agent", "web-01", None));
        assert!((ttl - 3600.0).abs() < 1.0, "{ttl}");

        let (actor_kind, actor_id, action, target, details): (String, String, String, String, String) =
            sqlx::query_as("SELECT actor_kind, actor_id, action, target_kind || '/' || target_id, details::text FROM audit_log")
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(
            (actor_kind.as_str(), actor_id.as_str(), action.as_str(), target.as_str()),
            ("system", "cli", "enrollment_token.create", "agent/web-01")
        );
        assert!(details.contains(&created.id.to_string()), "{details}");
        let secret = created.token.to_secret_string();
        assert!(!details.contains(secret.expose()), "{details}");
    }

    #[sqlx::test]
    async fn tokens_record_their_user(pool: PgPool) {
        let site_id = site(&pool).await;
        let user: Uuid = sqlx::query_scalar(
            "INSERT INTO users (username, password_hash) VALUES ('alice', '$argon2id$x') RETURNING id",
        )
        .fetch_one(&pool)
        .await
        .unwrap();
        let mut req = request(site_id, "relay-1");
        req.role = PeerRole::Relay;
        req.creator = Creator::User(user);
        req.ttl = Duration::from_secs(120);
        let created = token::create(&pool, CA, &req).await.unwrap();
        let row: (String, Option<Uuid>) =
            sqlx::query_as("SELECT role, created_by FROM enrollment_tokens WHERE id = $1")
                .bind(created.id)
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(row, ("relay".into(), Some(user)));
        let actor: String = sqlx::query_scalar("SELECT actor_kind || '/' || actor_id FROM audit_log")
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(actor, format!("user/{user}"));
    }

    #[sqlx::test]
    async fn invalid_requests_store_nothing(pool: PgPool) {
        let site_id = site(&pool).await;
        let mut req = request(site_id, "central");
        req.role = PeerRole::Server;
        assert!(matches!(token::create(&pool, CA, &req).await, Err(TokenError::Invalid(_))));
        // Unknown site: the foreign key rejects it and nothing is audited.
        assert!(matches!(
            token::create(&pool, CA, &request(Uuid::new_v4(), "web-01")).await,
            Err(TokenError::Db(_))
        ));
        let rows: i64 = sqlx::query_scalar(
            "SELECT (SELECT count(*) FROM enrollment_tokens) + (SELECT count(*) FROM audit_log)",
        )
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(rows, 0);
    }
}
