//! First-run enrollment against the real `Enroll` service. Tests that need
//! PostgreSQL are reported as ignored without `DATABASE_URL` at build time
//! (see `build.rs`).

mod support;

use std::os::unix::fs::PermissionsExt;
use std::time::Duration;

use monitorflow_agent::backoff::Backoff;
use monitorflow_agent::enroll::{ensure_identity, EnrollError};
use monitorflow_agent::shutdown;
use monitorflow_common::redact::Secret;
use monitorflow_common::tls::EnrollmentToken;
use sqlx::PgPool;
use support::{agent_config, closed_port_url, Pki};
use tempfile::TempDir;

const TIMEOUT: Duration = Duration::from_secs(10);

fn fast() -> Backoff {
    Backoff::new(Duration::from_millis(20), Duration::from_millis(100))
}

fn mode(path: &std::path::Path) -> u32 {
    std::fs::metadata(path).unwrap().permissions().mode() & 0o777
}

/// Retries a transient failure until shutdown, writing nothing.
async fn retries_until_shutdown(config: monitorflow_common::config::AgentConfig) {
    let (trigger, stop) = shutdown::channel();
    let task = tokio::spawn(async move { ensure_identity(&config, fast(), stop).await });
    tokio::time::sleep(Duration::from_millis(500)).await;
    assert!(!task.is_finished(), "a transient failure must be retried");
    trigger.fire();
    let result = tokio::time::timeout(TIMEOUT, task).await.unwrap().unwrap();
    assert!(matches!(result, Err(EnrollError::Shutdown)), "{result:?}");
}

#[tokio::test]
async fn unreachable_server_is_retried_until_shutdown() {
    let data = TempDir::new().unwrap();
    let token = EnrollmentToken::new([1; 32], [2; 32]).to_secret_string();
    let mut config = agent_config(
        data.path().into(),
        "https://localhost:1",
        &closed_port_url().await,
    );
    config.enrollment_token = Some(token);
    retries_until_shutdown(config).await;
    assert!(!data.path().join("tls/cert.pem").exists());
}

#[tokio::test]
async fn server_without_the_pinned_ca_gets_nothing() {
    let pki = Pki::new();
    // The handshake fails before the database is ever used.
    let pool = PgPool::connect_lazy("postgres://nobody@127.0.0.1:1/none").unwrap();
    let url = pki.serve_enroll(&pool).await;
    let data = TempDir::new().unwrap();
    let mut config = agent_config(data.path().into(), "https://localhost:1", &url);
    config.enrollment_token = Some(EnrollmentToken::new([1; 32], [2; 32]).to_secret_string());
    retries_until_shutdown(config).await;
    assert!(!data.path().join("tls").exists());
}

#[tokio::test]
async fn missing_or_malformed_token_is_fatal() {
    let data = TempDir::new().unwrap();
    let mut config = agent_config(
        data.path().into(),
        "https://localhost:1",
        "https://localhost:2",
    );
    let (_trigger, stop) = shutdown::channel();
    let err = ensure_identity(&config, fast(), stop.clone())
        .await
        .unwrap_err();
    assert!(matches!(err, EnrollError::NoToken { .. }), "{err}");
    config.enrollment_token = Some(Secret::new("mfe1.s3cret"));
    let err = ensure_identity(&config, fast(), stop)
        .await
        .unwrap_err()
        .to_string();
    assert!(err.starts_with("enrollment_token: malformed"), "{err}");
    assert!(!err.contains("s3cret"), "{err}");
}

db_tests! {
    #[sqlx::test(migrations = "../server/migrations")]
    async fn enrolls_once_then_reuses_the_identity(pool: PgPool) {
        let pki = Pki::new();
        let url = pki.serve_enroll(&pool).await;
        let data = TempDir::new().unwrap();
        let mut config = agent_config(data.path().into(), "https://localhost:1", &url);
        config.enrollment_token = Some(pki.token(&pool, "web-01").await);
        let (_trigger, stop) = shutdown::channel();

        let identity = ensure_identity(&config, fast(), stop.clone()).await.unwrap();
        assert_eq!(identity.id.uri(), "monitorflow://agent/web-01");
        let tls = config.tls_paths();
        assert_eq!(tls.cert, data.path().join("tls/cert.pem"));
        assert_eq!(mode(&tls.key), 0o600);
        assert_eq!(mode(tls.key.parent().unwrap()), 0o700);
        assert_eq!(std::fs::read_to_string(&tls.ca_cert).unwrap(), pki.ca.ca_cert_pem());
        assert!(!identity.renewal_due(std::time::SystemTime::now()));

        // The second start loads the files and never calls the server: the
        // spent token is ignored.
        let again = ensure_identity(&config, fast(), stop).await.unwrap();
        assert_eq!(again.id, identity.id);
        let certs: i64 = sqlx::query_scalar("SELECT count(*) FROM certificates")
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(certs, 1);
    }

    #[sqlx::test(migrations = "../server/migrations")]
    async fn a_spent_token_is_refused_cleanly(pool: PgPool) {
        let pki = Pki::new();
        let url = pki.serve_enroll(&pool).await;
        let token = pki.token(&pool, "web-01").await;
        let (_trigger, stop) = shutdown::channel();

        let first = TempDir::new().unwrap();
        let mut config = agent_config(first.path().into(), "https://localhost:1", &url);
        config.enrollment_token = Some(token.clone());
        ensure_identity(&config, fast(), stop.clone()).await.unwrap();

        let second = TempDir::new().unwrap();
        config.data_dir = second.path().into();
        let err = tokio::time::timeout(TIMEOUT, ensure_identity(&config, fast(), stop))
            .await
            .expect("a refusal is final, not retried")
            .unwrap_err();
        assert!(matches!(err, EnrollError::Refused), "{err}");
        assert!(!err.to_string().contains(token.expose()));
        assert!(!second.path().join("tls").exists());
    }
}
