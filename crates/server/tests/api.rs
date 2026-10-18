//! REST API against a real PostgreSQL. Each `#[sqlx::test]` gets a fresh
//! database with the migrations applied. Without `DATABASE_URL` at build
//! time the tests are reported as ignored (see `build.rs`).

use axum::body::Body;
use axum::http::{Request, StatusCode};
use axum::Router;
use monitorflow_server::api::{self, ApiSettings, ApiState};
use monitorflow_server::grpc::Registry;
use sqlx::PgPool;
use tower::ServiceExt;

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

fn app(pool: PgPool) -> Router {
    api::router(ApiState::new(
        pool,
        Registry::default(),
        ApiSettings::default(),
    ))
}

async fn call(app: Router, request: Request<Body>) -> (StatusCode, serde_json::Value) {
    let response = app.oneshot(request).await.unwrap();
    let status = response.status();
    let bytes = axum::body::to_bytes(response.into_body(), 64 * 1024)
        .await
        .unwrap();
    let body = if bytes.is_empty() {
        serde_json::Value::Null
    } else {
        serde_json::from_slice(&bytes).unwrap()
    };
    (status, body)
}

db_tests! {
    #[sqlx::test]
    async fn readiness_is_ok_with_database(pool: PgPool) {
        let request = Request::get("/api/health/ready").body(Body::empty()).unwrap();
        let (status, body) = call(app(pool), request).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body, serde_json::json!({ "status": "ready" }));
    }
}
