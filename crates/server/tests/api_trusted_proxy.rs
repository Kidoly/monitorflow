//! The client address sign-in limits count by, over a real listener
//! (ADR-0110): `X-Forwarded-For` counts only from a trusted proxy, so a
//! local user reaching the API directly cannot pick a fresh address per
//! attempt. Ignored without `DATABASE_URL`.

use std::sync::Arc;
use std::time::Duration;

use monitorflow_server::api::auth::CSRF_HEADER;
use monitorflow_server::api::client_addr::TrustedProxies;
use monitorflow_server::api::login_limit::LoginLimits;
use monitorflow_server::api::{self, ApiSettings, ApiState};
use monitorflow_server::grpc::Registry;
use reqwest::StatusCode;
use serde_json::json;
use sqlx::PgPool;
use tokio::net::TcpListener;

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

/// Serves the API on loopback with `proxies` trusted and 3 wrong attempts
/// per source; returns its base URL.
async fn start(pool: &PgPool, proxies: &[&str]) -> String {
    let settings = ApiSettings {
        trusted_proxies: Arc::new(TrustedProxies::new(
            &proxies.iter().map(|p| p.to_string()).collect::<Vec<_>>(),
        )),
        ..ApiSettings::default()
    };
    let mut state = ApiState::new(pool.clone(), Registry::default(), settings);
    state.login_limits = Arc::new(LoginLimits::new(3, 50, Duration::from_secs(600)));
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    tokio::spawn(api::serve(listener, state, std::future::pending()));
    url
}

/// One wrong attempt for its own account, claiming to come from
/// `claimed`.
async fn attempt(url: &str, n: u32, claimed: &str) -> StatusCode {
    reqwest::Client::new()
        .post(format!("{url}/api/auth/login"))
        .header(CSRF_HEADER, "1")
        .header("x-forwarded-for", claimed)
        .header("content-type", "application/json")
        .body(json!({ "username": format!("ghost{n}"), "password": "wrong" }).to_string())
        .send()
        .await
        .unwrap()
        .status()
}

db_tests! {
    #[sqlx::test]
    async fn an_untrusted_peer_cannot_choose_its_address(pool: PgPool) {
        // The listener is reached from loopback, which is not trusted here:
        // every attempt counts for 127.0.0.1, whatever the header says.
        let url = start(&pool, &["192.0.2.0/24"]).await;
        for n in 0..3 {
            let status = attempt(&url, n, &format!("198.51.100.{n}")).await;
            assert_eq!(status, StatusCode::UNAUTHORIZED);
        }
        assert_eq!(attempt(&url, 3, "198.51.100.3").await, StatusCode::TOO_MANY_REQUESTS);
    }

    #[sqlx::test]
    async fn a_trusted_proxy_gives_each_client_its_own_limit(pool: PgPool) {
        // The default: the proxy is on loopback.
        let url = start(&pool, &["127.0.0.1", "::1"]).await;
        for n in 0..3 {
            assert_eq!(attempt(&url, n, "198.51.100.1").await, StatusCode::UNAUTHORIZED);
        }
        assert_eq!(attempt(&url, 3, "198.51.100.1").await, StatusCode::TOO_MANY_REQUESTS);
        // Another client behind the same proxy is not limited by the first.
        assert_eq!(attempt(&url, 4, "198.51.100.2").await, StatusCode::UNAUTHORIZED);
    }
}
