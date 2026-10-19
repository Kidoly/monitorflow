//! Mirror endpoints (ADR-0030) against a real PostgreSQL and a fake tool
//! runner: global administrators only, jobs started in the background,
//! busy and never-synced mirrors refused. Ignored without `DATABASE_URL`.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use monitorflow_common::redact::Secret;
use monitorflow_server::api::auth::CSRF_HEADER;
use monitorflow_server::api::users::{self, GrantScope};
use monitorflow_server::api::{self, ApiSettings, ApiState, Role};
use monitorflow_server::grpc::Registry;
use monitorflow_server::mirror::{
    store, Family, Invocation, Layout, MirrorSpec, Mirrors, Output, RunError, Runner,
};
use serde_json::{json, Value};
use sqlx::PgPool;
use tower::ServiceExt;
use uuid::Uuid;

const PASSWORD: &str = "correct horse battery";

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

/// Succeeds at everything, knows one signing key, and waits while `hold`
/// is set.
#[derive(Default)]
struct FakeTools {
    hold: AtomicBool,
}

#[async_trait::async_trait]
impl Runner for FakeTools {
    async fn run(&self, invocation: &Invocation) -> Result<Output, RunError> {
        while self.hold.load(Ordering::SeqCst) {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        let line = invocation.to_string();
        let stdout = if line.contains("--list-secret-keys") {
            "sec:u:4096:1:X:1::::::scESC:\nfpr:::::::::FPR1:\n".into()
        } else {
            format!("ran {line}\n")
        };
        Ok(Output {
            success: true,
            status: "exit status: 0".into(),
            stdout,
            stderr: String::new(),
        })
    }
}

struct Harness {
    pool: PgPool,
    tools: Arc<FakeTools>,
    mirrors: Mirrors,
    _dir: tempfile::TempDir,
}

impl Harness {
    fn new(pool: &PgPool) -> Self {
        let dir = tempfile::tempdir().unwrap();
        let tools = Arc::new(FakeTools::default());
        let mirrors = Mirrors::new(pool.clone(), tools.clone(), Layout::new(dir.path()));
        Self {
            pool: pool.clone(),
            tools,
            mirrors,
            _dir: dir,
        }
    }

    async fn call(
        &self,
        method: &str,
        uri: &str,
        cookie: &str,
        body: Option<Value>,
    ) -> (StatusCode, Value) {
        let state = ApiState::new(
            self.pool.clone(),
            Registry::default(),
            ApiSettings::default(),
        )
        .with_mirrors(self.mirrors.clone());
        let request = Request::builder()
            .method(method)
            .uri(uri)
            .header(CSRF_HEADER, "1")
            .header("content-type", "application/json")
            .header("cookie", cookie)
            .body(body.map_or_else(Body::empty, |b| Body::from(b.to_string())))
            .unwrap();
        let response = api::router(state).oneshot(request).await.unwrap();
        let status = response.status();
        let bytes = axum::body::to_bytes(response.into_body(), 1 << 22)
            .await
            .unwrap();
        (
            status,
            serde_json::from_slice(&bytes).unwrap_or(Value::Null),
        )
    }

    async fn user(&self, name: &str, role: Role, scope: GrantScope) -> String {
        users::create_user_with(
            &self.pool,
            name,
            &Secret::new(PASSWORD),
            role,
            scope,
            "test",
        )
        .await
        .unwrap();
        let state = ApiState::new(
            self.pool.clone(),
            Registry::default(),
            ApiSettings::default(),
        );
        let request = Request::builder()
            .method("POST")
            .uri("/api/auth/login")
            .header(CSRF_HEADER, "1")
            .header("content-type", "application/json")
            .body(Body::from(
                json!({ "username": name, "password": PASSWORD }).to_string(),
            ))
            .unwrap();
        let response = api::router(state).oneshot(request).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let cookie = response
            .headers()
            .get("set-cookie")
            .unwrap()
            .to_str()
            .unwrap();
        cookie.split(';').next().unwrap().to_owned()
    }

    /// Waits until the mirror has no running run.
    async fn settle(&self, mirror: Uuid) {
        for _ in 0..200 {
            let runs = store::runs(&self.pool, mirror, 1).await.unwrap();
            if runs.first().is_none_or(|r| r.finished_at.is_some()) {
                return;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        panic!("the mirror job did not finish");
    }
}

fn dnf(name: &str) -> MirrorSpec {
    MirrorSpec {
        name: name.into(),
        family: Family::Dnf,
        upstream_url: "https://dl.rockylinux.org/pub/rocky/9/BaseOS/x86_64/os/".into(),
        distribution: String::new(),
        components: vec![],
        architectures: vec![],
        sync_interval_s: Some(86_400),
        auto_snapshot: true,
    }
}

db_tests! {
    #[sqlx::test]
    async fn global_administrators_sync_and_snapshot(pool: PgPool) {
        let h = Harness::new(&pool);
        let admin = h.user("root", Role::Administrator, GrantScope::Global).await;
        let mirror = store::create(&pool, &dnf("rocky"), ("system", "test")).await.unwrap();
        let path = format!("/api/mirrors/{}", mirror.id);

        let (status, body) = h.call("GET", "/api/mirrors", &admin, None).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["mirrors"][0]["name"], "rocky");
        assert_eq!(body["mirrors"][0]["family"], "dnf");
        assert_eq!(body["mirrors"][0]["current_snapshot"], Value::Null);
        assert_eq!(body["mirrors"][0]["last_run"], Value::Null);

        let (status, body) = h.call("POST", &format!("{path}/snapshots"), &admin, Some(json!({}))).await;
        assert_eq!((status, body["error"].as_str()), (StatusCode::CONFLICT, Some("the mirror has no successful sync to snapshot")));

        // While a sync runs, another job of the mirror is refused.
        h.tools.hold.store(true, Ordering::SeqCst);
        let (status, run) = h.call("POST", &format!("{path}/sync"), &admin, None).await;
        assert_eq!(status, StatusCode::ACCEPTED, "{run}");
        assert_eq!((run["kind"].as_str(), run["state"].as_str(), run["trigger"].as_str()), (Some("sync"), Some("running"), Some("manual")));
        assert!(run["requested_by"].as_str().unwrap().starts_with("user:"));
        let (status, _) = h.call("POST", &format!("{path}/sync"), &admin, None).await;
        assert_eq!(status, StatusCode::CONFLICT);
        h.tools.hold.store(false, Ordering::SeqCst);
        h.settle(mirror.id).await;

        let (status, body) = h.call("GET", &path, &admin, None).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["runs"][0]["state"], "succeeded", "{body}");
        assert!(body["runs"][0]["output"].as_str().unwrap().contains("$ reposync"));
        let auto = body["snapshots"][0]["name"].as_str().unwrap().to_owned();
        assert_eq!(body["current_snapshot"], auto.as_str());
        assert_eq!(body["last_run"]["id"], run["id"]);
        assert!(body["last_run"].get("output").is_none());

        let (status, run) = h.call("POST", &format!("{path}/snapshots"), &admin, Some(json!({ "name": "pinned" }))).await;
        assert_eq!((status, run["kind"].as_str()), (StatusCode::ACCEPTED, Some("snapshot")));
        h.settle(mirror.id).await;
        let (_, body) = h.call("GET", &path, &admin, None).await;
        assert_eq!(body["current_snapshot"], "pinned");
        assert_eq!(body["snapshots"][0]["state"], "published");
        for bad in [json!({ "name": "current" }), json!({ "name": "../x" }), json!({ "nom": "x" })] {
            let (status, _) = h.call("POST", &format!("{path}/snapshots"), &admin, Some(bad.clone())).await;
            assert_eq!(status, StatusCode::BAD_REQUEST, "{bad}");
        }
        let actions: Vec<String> = sqlx::query_scalar(
            "SELECT action FROM audit_log WHERE target_kind = 'mirror' AND actor_kind = 'user' ORDER BY id",
        )
        .fetch_all(&pool)
        .await
        .unwrap();
        assert_eq!(actions, ["mirror.sync", "mirror.snapshot"]);

        let unknown = format!("/api/mirrors/{}", Uuid::from_u128(7));
        assert_eq!(h.call("GET", &unknown, &admin, None).await.0, StatusCode::NOT_FOUND);
        assert_eq!(h.call("GET", "/api/mirrors/nope", &admin, None).await.0, StatusCode::NOT_FOUND);
        assert_eq!(h.call("POST", &format!("{unknown}/sync"), &admin, None).await.0, StatusCode::NOT_FOUND);
    }

    #[sqlx::test]
    async fn everyone_else_is_forbidden(pool: PgPool) {
        let h = Harness::new(&pool);
        let mirror = store::create(&pool, &dnf("rocky"), ("system", "test")).await.unwrap();
        let customer = Uuid::from_u128(1);
        let users = [
            h.user("viewer", Role::Viewer, GrantScope::Global).await,
            h.user("operator", Role::Operator, GrantScope::Global).await,
            h.user("customer-admin", Role::Administrator, GrantScope::Customer(customer)).await,
        ];
        let path = format!("/api/mirrors/{}", mirror.id);
        for cookie in &users {
            for (method, uri, body) in [
                ("GET", "/api/mirrors".to_owned(), None),
                ("GET", path.clone(), None),
                ("POST", format!("{path}/sync"), None),
                ("POST", format!("{path}/snapshots"), Some(json!({}))),
            ] {
                assert_eq!(h.call(method, &uri, cookie, body).await.0, StatusCode::FORBIDDEN, "{method} {uri}");
            }
        }
        assert!(store::runs(&pool, mirror.id, 10).await.unwrap().is_empty());
    }
}
