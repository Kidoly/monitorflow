//! Secrets sealed at rest (ADR-0125): an added central server never makes
//! a key of its own and refuses to start without the first one's, values
//! stored before are sealed once at startup, and a secret that does not
//! open fails its own deliveries only, never going out without it. Ignored
//! without `DATABASE_URL`.

use std::process::Command;
use std::sync::{Arc, Mutex};

use axum::http::HeaderMap;
use monitorflow_common::redact::Secret;
use monitorflow_server::api::users::{self, GrantScope};
use monitorflow_server::api::Role;
use monitorflow_server::integrations::{self, Clients, StoredEvent};
use monitorflow_server::notify::{self, Coverage, Destination, Dispatcher, Webhook};
use monitorflow_server::remediation::switch;
use monitorflow_server::secrets::{self, Column, Keyring};
use sqlx::PgPool;
use tokio::net::TcpListener;
use uuid::Uuid;

/// A webhook receiver: the signature header of each request.
async fn receiver() -> (String, Arc<Mutex<Vec<Option<String>>>>) {
    let seen = Arc::new(Mutex::new(Vec::new()));
    let store = seen.clone();
    let app = axum::Router::new().route(
        "/hook",
        axum::routing::post(move |headers: HeaderMap| {
            let store = store.clone();
            async move {
                let signature = headers
                    .get("x-monitorflow-signature")
                    .map(|v| v.to_str().unwrap().to_owned());
                store.lock().unwrap().push(signature);
                "ok"
            }
        }),
    );
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}/hook", listener.local_addr().unwrap());
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    (url, seen)
}

async fn stored(pool: &PgPool, sql: &str) -> Vec<Option<String>> {
    sqlx::query_scalar(sqlx::AssertSqlSafe(sql.to_owned()))
        .fetch_all(pool)
        .await
        .unwrap()
}

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

db_tests! {
    #[sqlx::test(migrations = "./migrations")]
    async fn an_added_server_without_the_key_does_not_start(pool: PgPool) {
        let database: String = sqlx::query_scalar("SELECT current_database()")
            .fetch_one(&pool)
            .await
            .unwrap();
        let base = std::env::var("DATABASE_URL").unwrap();
        let (server, _) = base.rsplit_once('/').unwrap();
        let url = format!("{server}/{database}?sslmode=disable");
        let dir = tempfile::tempdir().unwrap();
        let config = dir.path().join("server.toml");
        std::fs::write(&config, "").unwrap();
        let data = dir.path().to_owned();
        let output = tokio::task::spawn_blocking(move || {
            Command::new(env!("CARGO_BIN_EXE_monitorflow-server"))
                .arg("--config")
                .arg(&config)
                .env("MONITORFLOW_DATABASE_URL", url)
                .env("MONITORFLOW_ADDITIONAL_INSTANCE", "true")
                .env("MONITORFLOW_DATA_DIR", &data)
                .env("HOSTNAME", "host2")
                .output()
                .unwrap()
        })
        .await
        .unwrap();
        assert!(!output.status.success());
        let said = String::from_utf8_lossy(&output.stderr);
        assert!(said.contains("there is no secrets key at"), "{said}");
        assert!(said.contains("install-server.sh --add-server"), "{said}");
        assert!(!dir.path().join("secrets").exists());
        let places: i64 = sqlx::query_scalar("SELECT count(*) FROM server_instance_admissions")
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(places, 0);
    }

    #[sqlx::test(migrations = "./migrations")]
    async fn values_stored_before_are_sealed_once(pool: PgPool) {
        secrets::install_for_tests();
        sqlx::query(
            "INSERT INTO notification_channels (kind, settings, secret)
             VALUES ('webhook', '{\"url\": \"https://a.example/\"}', 's3cret'),
                    ('webhook', '{\"url\": \"https://b.example/\"}', NULL)",
        )
        .execute(&pool)
        .await
        .unwrap();
        let alice = users::create_user_with(
            &pool, "alice", &Secret::new("correct horse battery"), Role::Viewer, GrantScope::Global, "test",
        )
        .await
        .unwrap();
        sqlx::query(
            "INSERT INTO user_contacts (user_id, kind, settings, secret)
             VALUES ($1, 'slack', '{\"name\": \"me\", \"host\": \"hooks.slack.com\"}', 'https://hooks.slack.com/services/T/B/X')",
        )
        .bind(alice)
        .execute(&pool)
        .await
        .unwrap();

        sqlx::query(
            "INSERT INTO integration_endpoints (name, url, secret, events)
             VALUES ('psa', 'https://psa.example/hook', 'mfi_plain', '{incident.opened}')",
        )
        .execute(&pool)
        .await
        .unwrap();
        sqlx::query(
            "INSERT INTO server_settings (singleton, ai_enabled, ai_api_key)
             VALUES (true, true, 'sk-ant-plain')",
        )
        .execute(&pool)
        .await
        .unwrap();

        let sealed = secrets::seal_stored(&pool).await.unwrap();
        assert_eq!(
            sealed,
            [Column::NotificationChannels, Column::UserContacts, Column::IntegrationEndpoints, Column::AiApiKey]
        );
        let channels = "SELECT secret FROM notification_channels ORDER BY settings->>'url'";
        let first = stored(&pool, channels).await;
        assert_eq!(first[1], None, "no secret stays none");
        let value = first[0].clone().unwrap();
        assert!(value.starts_with("mfs1:"), "{value}");
        assert_eq!(secrets::open(Column::NotificationChannels, &value).unwrap().expose(), "s3cret");
        let contact = stored(&pool, "SELECT secret FROM user_contacts").await;
        let contact = contact[0].as_deref().unwrap();
        assert_eq!(
            secrets::open(Column::UserContacts, contact).unwrap().expose(),
            "https://hooks.slack.com/services/T/B/X"
        );
        let listed: Vec<String> = sqlx::query_scalar("SELECT name FROM sealed_columns ORDER BY name")
            .fetch_all(&pool)
            .await
            .unwrap();
        assert_eq!(
            listed,
            [
                "integration_endpoints.secret",
                "notification_channels.secret",
                "server_settings.ai_api_key",
                "user_contacts.secret",
            ]
        );
        let endpoint = stored(&pool, "SELECT secret FROM integration_endpoints").await;
        let endpoint = endpoint[0].as_deref().unwrap();
        assert_eq!(secrets::open(Column::IntegrationEndpoints, endpoint).unwrap().expose(), "mfi_plain");
        let wanted = switch::wanted(&pool).await.unwrap().unwrap();
        assert_eq!(wanted.api_key.unwrap().expose(), "sk-ant-plain");

        // The next start seals nothing: a value is never sealed twice.
        assert!(secrets::seal_stored(&pool).await.unwrap().is_empty());
        assert_eq!(stored(&pool, channels).await, first);

        // What the server reads opens.
        let loaded = notify::channels(&pool).await.unwrap();
        let with_secret = loaded.iter().find(|c| c.secret.is_some()).unwrap();
        assert_eq!(with_secret.secret.as_ref().unwrap().expose(), "s3cret");
        assert!(loaded.iter().all(|c| c.secret_error.is_none()));
    }

    #[sqlx::test(migrations = "./migrations")]
    async fn a_secret_that_does_not_open_fails_alone_and_is_never_left_out(pool: PgPool) {
        secrets::install_for_tests();
        let (url, seen) = receiver().await;
        let hook = Destination::Webhook(Webhook { url });
        // Signed with a secret this server can open.
        let good = notify::insert(&pool, &hook, Some(&Secret::new("ours")), Coverage::Everything, false)
            .await
            .unwrap();
        // And one sealed with a key it does not have, as after a lost key.
        let other = Keyring::generate().unwrap();
        let lost = notify::insert(&pool, &hook, None, Coverage::Everything, false).await.unwrap();
        sqlx::query("UPDATE notification_channels SET secret = $2 WHERE id = $1")
            .bind(lost)
            .bind(other.seal(Column::NotificationChannels, "theirs").unwrap())
            .execute(&pool)
            .await
            .unwrap();
        // A value copied from another column does not open either.
        let moved = notify::insert(&pool, &hook, None, Coverage::Everything, false).await.unwrap();
        sqlx::query("UPDATE notification_channels SET secret = $2 WHERE id = $1")
            .bind(moved)
            .bind(secrets::seal(Column::UserContacts, "from a contact").unwrap())
            .execute(&pool)
            .await
            .unwrap();

        let dispatcher = Dispatcher::new(pool.clone(), "https://mf.example");
        let channels = notify::channels(&pool).await.unwrap();
        let by_id = |id: Uuid| channels.iter().find(|c| c.id == id).unwrap();
        let failed = dispatcher.test(by_id(lost)).await.unwrap().unwrap_err();
        assert!(failed.message.contains(&format!("key {}", other.ids()[0])), "{}", failed.message);
        let failed = dispatcher.test(by_id(moved)).await.unwrap().unwrap_err();
        assert!(failed.message.contains("does not open"), "{}", failed.message);
        let last: Option<String> = sqlx::query_scalar("SELECT last_error FROM notification_channels WHERE id = $1")
            .bind(lost)
            .fetch_one(&pool)
            .await
            .unwrap();
        assert!(last.unwrap().contains("which this server does not have"));
        assert!(seen.lock().unwrap().is_empty(), "nothing went out unsigned");

        dispatcher.test(by_id(good)).await.unwrap().unwrap();
        let signatures = seen.lock().unwrap().clone();
        assert_eq!(signatures.len(), 1);
        assert!(signatures[0].is_some(), "signed with the secret that opens");
    }

    #[sqlx::test(migrations = "./migrations")]
    async fn an_endpoint_or_ai_key_that_does_not_open_is_never_used(pool: PgPool) {
        secrets::install_for_tests();
        let (url, seen) = receiver().await;
        let other = Keyring::generate().unwrap();
        let id: Uuid = sqlx::query_scalar(
            "INSERT INTO integration_endpoints (name, url, secret, events, allow_private)
             VALUES ('psa', $1, $2, '{incident.opened}', true) RETURNING id",
        )
        .bind(&url)
        .bind(other.seal(Column::IntegrationEndpoints, "mfi_theirs").unwrap())
        .fetch_one(&pool)
        .await
        .unwrap();
        let endpoint = integrations::load(&pool, id).await.unwrap().unwrap();
        let event = StoredEvent {
            id: Uuid::new_v4(),
            event: "incident.opened".into(),
            details: serde_json::json!({}),
            created_at: chrono::Utc::now(),
        };
        let failure = Clients::default()
            .post(&endpoint, &event, "https://mf.example", false)
            .await
            .unwrap_err();
        assert!(failure.message.contains("which this server does not have"), "{}", failure.message);
        assert!(seen.lock().unwrap().is_empty(), "nothing went out unsigned");

        // AI stays off rather than calling without its key.
        sqlx::query(
            "INSERT INTO server_settings (singleton, ai_enabled, ai_api_key) VALUES (true, true, $1)",
        )
        .bind(other.seal(Column::AiApiKey, "sk-ant-theirs").unwrap())
        .execute(&pool)
        .await
        .unwrap();
        assert!(switch::wanted(&pool).await.unwrap().is_none());
    }
}
