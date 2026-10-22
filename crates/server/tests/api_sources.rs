//! Source configuration through the API (ADR-0035): who may queue it,
//! the spec built from the mirrors table and the signing key, what is
//! refused, audited and stored. Ignored without `DATABASE_URL`.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use monitorflow_common::proto::v1::configure_sources::{Family, Mode};
use monitorflow_common::proto::v1::{ConfigureSources, RevertSources};
use monitorflow_common::redact::Secret;
use monitorflow_server::api::auth::CSRF_HEADER;
use monitorflow_server::api::users::{self, GrantScope};
use monitorflow_server::api::{self, ApiSettings, ApiState, Role};
use monitorflow_server::grpc::Registry;
use monitorflow_server::mirror::{Invocation, Layout, Mirrors, Output, RunError, Runner};
use prost::Message;
use serde_json::{json, Value};
use sqlx::PgPool;
use tower::ServiceExt;
use uuid::Uuid;

const PASSWORD: &str = "correct horse battery";
const ARMORED: &str =
    "-----BEGIN PGP PUBLIC KEY BLOCK-----\n\nxx\n-----END PGP PUBLIC KEY BLOCK-----\n";

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

/// gpg knowing one secret key while `has_key` is set.
struct FakeGpg {
    has_key: AtomicBool,
}

#[async_trait::async_trait]
impl Runner for FakeGpg {
    async fn run(&self, invocation: &Invocation) -> Result<Output, RunError> {
        let line = invocation.to_string();
        assert!(line.contains("--list-secret-keys"), "{line}");
        let stdout = if self.has_key.load(Ordering::SeqCst) {
            "sec:u:4096:1:X:1::::::scESC:\nfpr:::::::::FPR1:\n".into()
        } else {
            String::new()
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
    gpg: Arc<FakeGpg>,
    mirrors: Mirrors,
    customer: Uuid,
    site: Uuid,
    debian: Uuid,
    rhel: Uuid,
    _dir: tempfile::TempDir,
}

impl Harness {
    async fn new(pool: &PgPool) -> Self {
        let one = |sql: &'static str| sqlx::query_scalar::<_, Uuid>(sql);
        let customer = one("INSERT INTO customers (name) VALUES ('acme') RETURNING id")
            .fetch_one(pool)
            .await
            .unwrap();
        let site = one("INSERT INTO sites (name, customer_id) VALUES ('paris', $1) RETURNING id")
            .bind(customer)
            .fetch_one(pool)
            .await
            .unwrap();
        let server = |peer: &'static str, family: &'static str| {
            one("INSERT INTO servers (peer_id, site_id, hostname, os_family)
                 VALUES ($1, $2, $1, $3) RETURNING id")
            .bind(peer)
            .bind(site)
            .bind(family)
            .fetch_one(pool)
        };
        let debian = server("agent-deb", "debian").await.unwrap();
        let rhel = server("agent-rhel", "rhel").await.unwrap();
        let apt = one(
            "INSERT INTO mirrors (name, family, upstream_url, distribution, components)
             VALUES ('debian-12', 'apt', 'https://deb.debian.org/debian', 'bookworm', '{main,contrib}')
             RETURNING id",
        )
        .fetch_one(pool)
        .await
        .unwrap();
        one("INSERT INTO mirrors (name, family, upstream_url, distribution, components)
             VALUES ('rocky-9', 'dnf', 'https://dl.rockylinux.org/pub/rocky/9/BaseOS/x86_64/os', '', '{}')
             RETURNING id")
        .fetch_one(pool)
        .await
        .unwrap();
        one(
            "INSERT INTO mirror_snapshots (mirror_id, name, state, published_at)
             VALUES ($1, 's1', 'published', now()) RETURNING id",
        )
        .bind(apt)
        .fetch_one(pool)
        .await
        .unwrap();
        let dir = tempfile::tempdir().unwrap();
        let layout = Layout::new(dir.path());
        std::fs::create_dir_all(layout.keys()).unwrap();
        std::fs::write(layout.public_key(), ARMORED).unwrap();
        let gpg = Arc::new(FakeGpg {
            has_key: AtomicBool::new(true),
        });
        let mirrors = Mirrors::new(pool.clone(), gpg.clone(), layout);
        Self {
            pool: pool.clone(),
            gpg,
            mirrors,
            customer,
            site,
            debian,
            rhel,
            _dir: dir,
        }
    }

    fn state(&self, public_url: Option<&str>) -> ApiState {
        let settings = ApiSettings {
            mirror_public_url: public_url.map(str::to_owned),
            ..ApiSettings::default()
        };
        ApiState::new(self.pool.clone(), Registry::default(), settings)
            .with_mirrors(self.mirrors.clone())
    }

    async fn send_to(
        &self,
        state: ApiState,
        method: &str,
        uri: &str,
        cookie: &str,
        body: Option<Value>,
    ) -> (StatusCode, Value) {
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

    async fn send(
        &self,
        method: &str,
        uri: &str,
        cookie: &str,
        body: Option<Value>,
    ) -> (StatusCode, Value) {
        let state = self.state(Some("http://mf.example:7480/"));
        self.send_to(state, method, uri, cookie, body).await
    }

    async fn login(&self, name: &str, role: Role, scope: GrantScope) -> String {
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
        let body = json!({ "username": name, "password": PASSWORD });
        let request = Request::post("/api/auth/login")
            .header(CSRF_HEADER, "1")
            .header("content-type", "application/json")
            .body(Body::from(body.to_string()))
            .unwrap();
        let response = api::router(self.state(None))
            .oneshot(request)
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        response.headers()["set-cookie"]
            .to_str()
            .unwrap()
            .split(';')
            .next()
            .unwrap()
            .to_owned()
    }

    async fn payload(&self, id: &Value) -> Vec<u8> {
        let id: Uuid = id.as_str().unwrap().parse().unwrap();
        sqlx::query_scalar("SELECT payload FROM commands WHERE id = $1")
            .bind(id)
            .fetch_one(&self.pool)
            .await
            .unwrap()
    }

    async fn commands(&self) -> i64 {
        sqlx::query_scalar("SELECT count(*) FROM commands")
            .fetch_one(&self.pool)
            .await
            .unwrap()
    }
}

db_tests! {
    #[sqlx::test]
    async fn administrators_point_servers_at_mirror_snapshots(pool: PgPool) {
        let h = Harness::new(&pool).await;
        let admin = h.login("admin", Role::Administrator, GrantScope::Customer(h.customer)).await;
        let site_admin = h.login("siteadmin", Role::Administrator, GrantScope::Site(h.site)).await;
        let uri = format!("/api/servers/{}/sources", h.debian);
        let body = json!({"mirrors": [{"name": "debian-12", "snapshot": "s1"}], "mode": "replace", "ttl_s": 600});

        assert_eq!(h.send("POST", &uri, &site_admin, Some(body.clone())).await.0, StatusCode::FORBIDDEN);
        assert_eq!(h.send("DELETE", &uri, &site_admin, None).await.0, StatusCode::FORBIDDEN);
        let missing = format!("/api/servers/{}/sources", Uuid::new_v4());
        assert_eq!(h.send("POST", &missing, &admin, Some(body.clone())).await.0, StatusCode::NOT_FOUND);

        let (status, created) = h.send("POST", &uri, &admin, Some(body.clone())).await;
        assert_eq!(status, StatusCode::CREATED, "{created}");
        assert_eq!(created["kind"], "configure_sources");
        assert_eq!(created["state"], "queued");
        assert_eq!(
            created["parameters"],
            json!({
                "mirror_base_url": "http://mf.example:7480",
                "family": "FAMILY_APT",
                "mode": "MODE_REPLACE",
                "repositories": [{"mirror": "debian-12", "snapshot": "s1"}],
                "key_fingerprints": ["FPR1"],
            })
        );
        let spec = ConfigureSources::decode(h.payload(&created["id"]).await.as_slice()).unwrap();
        assert_eq!(spec.family(), Family::Apt);
        assert_eq!(spec.mode(), Mode::Replace);
        assert_eq!(spec.apt[0].suite, "bookworm");
        assert_eq!(spec.apt[0].components, ["main", "contrib"]);
        assert_eq!(spec.keys[0].armored, ARMORED);
        assert_eq!(spec.keys[0].fingerprint, "FPR1");

        let audited: Value = sqlx::query_scalar(
            "SELECT details FROM audit_log WHERE action = 'command.create'",
        )
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(audited["kind"], "configure_sources");
        assert!(!audited.to_string().contains("PGP PUBLIC KEY"), "{audited}");

        // `current` is the default, once a snapshot is published.
        let current = json!({"mirrors": [{"name": "debian-12"}], "mode": "add"});
        let (status, created) = h.send("POST", &uri, &admin, Some(current)).await;
        assert_eq!(status, StatusCode::CREATED, "{created}");
        assert_eq!(created["parameters"]["repositories"][0]["snapshot"], "current");

        let (status, reverted) = h.send("DELETE", &uri, &admin, None).await;
        assert_eq!(status, StatusCode::ACCEPTED, "{reverted}");
        assert_eq!(reverted["kind"], "revert_sources");
        assert_eq!(reverted["parameters"], json!({}));
        assert!(RevertSources::decode(h.payload(&reverted["id"]).await.as_slice()).is_ok());
        assert_eq!(h.commands().await, 3);

        // A revoked server takes no new command.
        sqlx::query("UPDATE servers SET revoked_at = now() WHERE id = $1").bind(h.debian).execute(&pool).await.unwrap();
        assert_eq!(h.send("POST", &uri, &admin, Some(body)).await.0, StatusCode::CONFLICT);
        assert_eq!(h.send("DELETE", &uri, &admin, None).await.0, StatusCode::CONFLICT);
    }

    #[sqlx::test]
    async fn unusable_mirrors_and_missing_settings_are_refused(pool: PgPool) {
        let h = Harness::new(&pool).await;
        let admin = h.login("admin", Role::Administrator, GrantScope::Customer(h.customer)).await;
        let deb = format!("/api/servers/{}/sources", h.debian);
        let rhel = format!("/api/servers/{}/sources", h.rhel);
        let refused = |uri: &str, body: Value| {
            let (uri, admin) = (uri.to_owned(), admin.clone());
            let h = &h;
            async move { h.send("POST", &uri, &admin, Some(body)).await }
        };
        for (uri, body) in [
            (&deb, json!({"mirrors": [], "mode": "replace"})),
            (&deb, json!({"mirrors": [{"name": "nope"}], "mode": "replace"})),
            (&deb, json!({"mirrors": [{"name": "debian-12", "snapshot": "s2"}], "mode": "replace"})),
            // Never snapshotted: no `current`.
            (&rhel, json!({"mirrors": [{"name": "rocky-9"}], "mode": "replace"})),
            (&rhel, json!({"mirrors": [{"name": "debian-12"}], "mode": "replace"})),
            (&deb, json!({"mirrors": [{"name": "debian-12"}, {"name": "debian-12"}], "mode": "add"})),
            (&deb, json!({"mirrors": [{"name": "debian-12"}], "mode": "add", "ttl_s": 1})),
            (&deb, json!({"mirrors": [{"name": "debian-12"}], "mode": "merge"})),
        ] {
            let (status, answer) = refused(uri, body.clone()).await;
            assert_eq!(status, StatusCode::BAD_REQUEST, "{body}: {answer}");
        }

        let body = json!({"mirrors": [{"name": "debian-12"}], "mode": "add"});
        let no_url = h.state(None);
        assert_eq!(h.send_to(no_url, "POST", &deb, &admin, Some(body.clone())).await.0, StatusCode::CONFLICT);
        let no_mirrors = ApiState::new(pool.clone(), Registry::default(), ApiSettings {
            mirror_public_url: Some("http://mf.example".into()),
            ..ApiSettings::default()
        });
        assert_eq!(h.send_to(no_mirrors, "POST", &deb, &admin, Some(body.clone())).await.0, StatusCode::CONFLICT);
        h.gpg.has_key.store(false, Ordering::SeqCst);
        let (status, answer) = refused(&deb, body).await;
        assert_eq!(status, StatusCode::CONFLICT);
        assert!(answer.to_string().contains("no signing key"), "{answer}");
        assert_eq!(h.commands().await, 0);
    }

    #[sqlx::test]
    async fn servers_behind_a_mirroring_relay_get_its_url(pool: PgPool) {
        let h = Harness::new(&pool).await;
        let admin = h.login("admin", Role::Administrator, GrantScope::Customer(h.customer)).await;
        let relay: Uuid = sqlx::query_scalar(
            "INSERT INTO relays (peer_id, site_id, name, mirror_url, mirror_reported_at)
             VALUES ('relay-1', $1, 'relay-1', 'http://relay.example:7480', now()) RETURNING id",
        )
        .bind(h.site)
        .fetch_one(&pool)
        .await
        .unwrap();
        sqlx::query(
            "INSERT INTO relay_mirrors (relay_id, family, name, current, snapshots)
             VALUES ($1, 'apt', 'debian-12', 's1', '{s1}')",
        )
        .bind(relay)
        .execute(&pool)
        .await
        .unwrap();
        sqlx::query(
            "INSERT INTO mirror_snapshots (mirror_id, name, state, published_at)
             SELECT id, 's2', 'published', now() FROM mirrors WHERE name = 'debian-12'",
        )
        .execute(&pool)
        .await
        .unwrap();
        let uri = format!("/api/servers/{}/sources", h.debian);
        let base = |snapshot: &'static str, public: Option<&'static str>| {
            let (uri, admin, h) = (uri.clone(), admin.clone(), &h);
            async move {
                let body = json!({"mirrors": [{"name": "debian-12", "snapshot": snapshot}], "mode": "add"});
                let (status, created) = h.send_to(h.state(public), "POST", &uri, &admin, Some(body)).await;
                (status, created["parameters"]["mirror_base_url"].as_str().map(str::to_owned))
            }
        };
        let public = Some("http://mf.example:7480/");
        let created = |url: &str| (StatusCode::CREATED, Some(url.to_owned()));

        // Not attached to the relay yet: the server's URL.
        assert_eq!(base("s1", public).await, created("http://mf.example:7480"));
        sqlx::query("UPDATE servers SET relay_id = $1 WHERE id = $2")
            .bind(relay)
            .bind(h.debian)
            .execute(&pool)
            .await
            .unwrap();
        assert_eq!(base("s1", public).await, created("http://relay.example:7480"));
        assert_eq!(base("current", public).await, created("http://relay.example:7480"));
        // Without a server URL, the relay's is enough.
        assert_eq!(base("s1", None).await, created("http://relay.example:7480"));
        // A snapshot the relay does not hold: the server's URL, or none.
        assert_eq!(base("s2", public).await, created("http://mf.example:7480"));
        assert_eq!(base("s2", None).await, (StatusCode::CONFLICT, None));

        // A revoked relay is not used.
        sqlx::query("UPDATE relays SET revoked_at = now() WHERE id = $1")
            .bind(relay)
            .execute(&pool)
            .await
            .unwrap();
        assert_eq!(base("s1", public).await, created("http://mf.example:7480"));
    }
}
