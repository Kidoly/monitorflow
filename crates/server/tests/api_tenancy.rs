//! Exhaustive tenancy test (ADR-0026): every route of the API router has a
//! case asserting that an administrator of customer A gets 404, 403 or an
//! empty answer for customer B's resources, and that nothing about B
//! reaches VictoriaMetrics.
//!
//! Routes are read from the `.route(...)` calls of `src/api/mod.rs`, and
//! the router itself is probed to confirm that list: a route added without
//! a case here fails [`every_route_keeps_customers_apart`]. Ignored without
//! `DATABASE_URL`.

use std::collections::BTreeSet;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::body::Body;
use axum::extract::{OriginalUri, State};
use axum::http::{Request, StatusCode};
use axum::Router;
use monitorflow_common::redact::Secret;
use monitorflow_server::api::auth::CSRF_HEADER;
use monitorflow_server::api::users::{self, GrantScope};
use monitorflow_server::api::{self, ApiSettings, ApiState, EnrollmentSettings, Role};
use monitorflow_server::grpc::Registry;
use monitorflow_server::vm_query::VmQuery;
use serde_json::{json, Value};
use sqlx::PgPool;
use tokio::net::TcpListener;
use tower::ServiceExt;
use uuid::Uuid;

const PASSWORD: &str = "correct horse battery";
const ROUTER_SOURCE: &str = include_str!("../src/api/mod.rs");
const METHODS: [&str; 5] = ["get", "post", "put", "delete", "patch"];

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

/// `(METHOD, path)` of every `.route(...)` call in the router source.
fn source_routes() -> BTreeSet<(String, String)> {
    for other in [".nest(", ".merge(", ".route_service(", ".nest_service("] {
        assert!(
            !ROUTER_SOURCE.contains(other),
            "the router uses `{other}`; teach source_routes() to read it"
        );
    }
    let mut routes = BTreeSet::new();
    for (start, _) in ROUTER_SOURCE.match_indices(".route(") {
        let call = &ROUTER_SOURCE[start + ".route(".len()..];
        let mut depth = 1;
        let end = call
            .char_indices()
            .find(|&(_, c)| {
                depth += match c {
                    '(' => 1,
                    ')' => -1,
                    _ => 0,
                };
                depth == 0
            })
            .unwrap()
            .0;
        let call = &call[..end];
        let path = call.split('"').nth(1).unwrap().to_owned();
        let mut found = false;
        for method in METHODS {
            let token = format!("{method}(");
            let used = call.match_indices(&token).any(|(i, _)| {
                !call[..i]
                    .chars()
                    .next_back()
                    .is_some_and(|c| c.is_alphanumeric() || c == '_' || c == ':')
            });
            if used {
                routes.insert((method.to_uppercase(), path.clone()));
                found = true;
            }
        }
        assert!(found, "no method found in route {path}");
    }
    assert!(routes.len() > 20, "suspiciously few routes: {routes:?}");
    routes
}

/// A path template with `{id}` and `{peer_id}` filled in.
fn concrete(path: &str) -> String {
    path.replace("{id}", &Uuid::from_u128(42).to_string())
        .replace("{peer_id}", "nobody")
}

/// Fake VictoriaMetrics: records the query string of every request and
/// answers an empty success.
#[derive(Clone, Default)]
struct FakeVm {
    seen: Arc<Mutex<Vec<String>>>,
}

impl FakeVm {
    async fn start(&self) -> String {
        async fn answer(State(vm): State<FakeVm>, OriginalUri(uri): OriginalUri) -> String {
            let decoded = reqwest::Url::parse(&format!("http://fake{uri}"))
                .unwrap()
                .query_pairs()
                .map(|(k, v)| format!("{k}={v}"))
                .collect::<Vec<_>>()
                .join("&");
            vm.seen.lock().unwrap().push(decoded);
            let data = if uri.path().ends_with("/series") {
                json!([])
            } else {
                json!({ "resultType": "matrix", "result": [] })
            };
            json!({ "status": "success", "data": data }).to_string()
        }
        let app = Router::new().fallback(answer).with_state(self.clone());
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        url
    }

    fn seen(&self) -> Vec<String> {
        self.seen.lock().unwrap().clone()
    }
}

/// One customer's resources.
#[derive(Debug, Clone, Copy)]
struct Tenant {
    customer: Uuid,
    site: Uuid,
    server: Uuid,
    site_sensor: Uuid,
    server_sensor: Uuid,
    site_threshold: Uuid,
    server_threshold: Uuid,
    incident: Uuid,
    /// A remediation of `incident` (ADR-0042), waiting for approval.
    remediation: Uuid,
    /// Its pending command (ADR-0045).
    pending: Uuid,
    relay: Uuid,
    token: Uuid,
    /// An install key with a host, `pending-<name>`, waiting for approval
    /// (ADR-0058).
    install_key: Uuid,
    command: Uuid,
    /// A draft campaign of one wave with the server (ADR-0033).
    campaign: Uuid,
    /// A customer-wide deny rule; the customer also has a remediation
    /// policy (ADR-0041).
    remediation_rule: Uuid,
}

async fn tenant(pool: &PgPool, name: &str) -> Tenant {
    let one = |sql: &'static str| sqlx::query_scalar::<_, Uuid>(sql);
    let customer = one("INSERT INTO customers (name) VALUES ($1) RETURNING id")
        .bind(name)
        .fetch_one(pool)
        .await
        .unwrap();
    let site = one("INSERT INTO sites (name, customer_id) VALUES ($1, $2) RETURNING id")
        .bind(format!("site-{name}"))
        .bind(customer)
        .fetch_one(pool)
        .await
        .unwrap();
    let server = one("INSERT INTO servers (peer_id, site_id, hostname, os_family)
         VALUES ($1, $2, $1, 'debian') RETURNING id")
    .bind(format!("agent-{name}"))
    .bind(site)
    .fetch_one(pool)
    .await
    .unwrap();
    let sensor = "INSERT INTO sensor_configs (site_id, server_id, kind, name, interval_ms)
                  VALUES ($1, $2, 'cpu', 'cpu', 10000) RETURNING id";
    let threshold = "INSERT INTO thresholds (site_id, server_id, metric, comparison, warning)
                     VALUES ($1, $2, 'monitorflow_cpu_usage_ratio', '>', 0.9) RETURNING id";
    let mut ids = Vec::new();
    for sql in [sensor, threshold] {
        for (site_id, server_id) in [(Some(site), None), (None, Some(server))] {
            ids.push(
                one(sql)
                    .bind(site_id)
                    .bind(server_id)
                    .fetch_one(pool)
                    .await
                    .unwrap(),
            );
        }
    }
    let incident = one(
        "INSERT INTO incidents (server_id, metric, instance, severity, opened_at, last_value,
                                last_value_at, peak_value, threshold)
         VALUES ($1, 'monitorflow_cpu_usage_ratio', 'total', 'warning', now(), 0.95, now(),
                 0.95, '{}') RETURNING id",
    )
    .bind(server)
    .fetch_one(pool)
    .await
    .unwrap();
    let remediation = one(
        "INSERT INTO remediation_incidents (customer_id, server_id, alert_incident_id, metric,
                                            max_commands, max_duration_s)
         VALUES ($1, $2, $3, 'monitorflow_cpu_usage_ratio', 5, 600) RETURNING id",
    )
    .bind(customer)
    .bind(server)
    .bind(incident)
    .fetch_one(pool)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO remediation_incident_events
             (incident_id, seq, kind, actor_kind, actor_id, payload, prev_hash, hash)
         VALUES ($1, 1, 'ai_proposal', 'ai', 'mock/m', '{\"command\": \"uptime\"}',
                 decode(repeat('00', 32), 'hex'), decode(repeat('00', 32), 'hex'))",
    )
    .bind(remediation)
    .execute(pool)
    .await
    .unwrap();
    let pending = one("INSERT INTO remediation_pending_commands
             (incident_id, proposal_seq, command, diagnosis, reasoning, tier, rule, reason)
         VALUES ($1, 1, 'systemctl restart nginx', 'd', 'r', 'low_impact', '{}', 'restart')
         RETURNING id")
    .bind(remediation)
    .fetch_one(pool)
    .await
    .unwrap();
    sqlx::query("UPDATE remediation_incidents SET state = 'awaiting_approval' WHERE id = $1")
        .bind(remediation)
        .execute(pool)
        .await
        .unwrap();
    let relay = one("INSERT INTO relays (peer_id, site_id, name) VALUES ($1, $2, $1) RETURNING id")
        .bind(format!("relay-{name}"))
        .bind(site)
        .fetch_one(pool)
        .await
        .unwrap();
    let token = one(
        "INSERT INTO enrollment_tokens (token_hash, role, peer_id, site_id, expires_at)
         VALUES (sha256(convert_to($1, 'UTF8')), 'agent', $1, $2, now() + interval '1 hour') RETURNING id",
    )
    .bind(format!("agent-{name}-2"))
    .bind(site)
    .fetch_one(pool)
    .await
    .unwrap();
    let install_key = one(
        "INSERT INTO install_keys (key_hash, site_id, expires_at, require_approval)
         VALUES (sha256(convert_to($1, 'UTF8')), $2, now() + interval '1 day', true) RETURNING id",
    )
    .bind(format!("key-{name}"))
    .bind(site)
    .fetch_one(pool)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO pending_hosts (peer_id, site_id, install_key_id, hostname)
         VALUES ($1, $2, $3, $1)",
    )
    .bind(format!("pending-{name}"))
    .bind(site)
    .bind(install_key)
    .execute(pool)
    .await
    .unwrap();
    let command = one("INSERT INTO commands (server_id, kind, payload, expires_at)
         VALUES ($1, 'apply_updates', '', now() + interval '1 hour') RETURNING id")
    .bind(server)
    .fetch_one(pool)
    .await
    .unwrap();
    let campaign = one("INSERT INTO campaigns (customer_id, name) VALUES ($1, $2) RETURNING id")
        .bind(customer)
        .bind(format!("campaign-{name}"))
        .fetch_one(pool)
        .await
        .unwrap();
    sqlx::query(
        "WITH w AS (INSERT INTO campaign_waves (campaign_id, position) VALUES ($1, 0))
         INSERT INTO campaign_targets (campaign_id, server_id, wave) VALUES ($1, $2, 0)",
    )
    .bind(campaign)
    .bind(server)
    .execute(pool)
    .await
    .unwrap();
    let remediation_rule = one(
        "INSERT INTO remediation_rules (customer_id, kind, pattern) VALUES ($1, 'deny', 'reboot')
         RETURNING id",
    )
    .bind(customer)
    .fetch_one(pool)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO remediation_policies (customer_id, low_impact_auto) VALUES ($1, true)",
    )
    .bind(customer)
    .execute(pool)
    .await
    .unwrap();
    Tenant {
        remediation,
        pending,
        campaign,
        command,
        remediation_rule,
        customer,
        site,
        server,
        relay,
        token,
        install_key,
        site_sensor: ids[0],
        server_sensor: ids[1],
        site_threshold: ids[2],
        server_threshold: ids[3],
        incident,
    }
}

struct Ctx {
    pool: PgPool,
    vm: FakeVm,
    vm_url: String,
    cookie: String,
    a: Tenant,
    b: Tenant,
    /// A global mirror (ADR-0030).
    mirror: Uuid,
}

impl Ctx {
    fn app(&self) -> Router {
        let vm = VmQuery::with_timeout(&self.vm_url, Duration::from_millis(500)).unwrap();
        api::router(
            ApiState::new(
                self.pool.clone(),
                Registry::default(),
                ApiSettings::default(),
            )
            .with_metrics(vm)
            .with_enrollment(EnrollmentSettings {
                ca_fingerprint: [7; 32],
                server_names: vec!["mf.example".into()],
                grpc_port: 7443,
                enroll_port: 7445,
            }),
        )
    }

    async fn send(
        &self,
        method: &str,
        uri: &str,
        cookie: Option<&str>,
        body: Option<Value>,
    ) -> (StatusCode, Value) {
        let mut request = Request::builder()
            .method(method)
            .uri(uri)
            .header(CSRF_HEADER, "1")
            .header("content-type", "application/json");
        if let Some(cookie) = cookie {
            request = request.header("cookie", cookie);
        }
        let request = request
            .body(body.map_or_else(Body::empty, |b| Body::from(b.to_string())))
            .unwrap();
        let response = self.app().oneshot(request).await.unwrap();
        let status = response.status();
        let bytes = axum::body::to_bytes(response.into_body(), 1 << 20)
            .await
            .unwrap();
        let body = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
        (status, body)
    }

    /// A request as the customer A administrator.
    async fn call(&self, method: &str, uri: &str, body: Option<Value>) -> (StatusCode, Value) {
        self.send(method, uri, Some(&self.cookie), body).await
    }

    async fn status(&self, method: &str, uri: &str, body: Option<Value>) -> StatusCode {
        self.call(method, uri, body).await.0
    }

    /// The last query string VictoriaMetrics received.
    fn vm_seen_last(&self) -> String {
        self.vm.seen().pop().unwrap_or_default()
    }

    /// Whether `id` appears anywhere in `body`.
    fn mentions(body: &Value, id: Uuid) -> bool {
        body.to_string().contains(&id.to_string())
    }
}

fn sensor_body() -> Value {
    json!({ "kind": "cpu", "interval_ms": 10000 })
}

fn threshold_body() -> Value {
    json!({ "metric": "monitorflow_cpu_usage_ratio", "comparison": ">", "warning": 0.5 })
}

/// The tenancy case of one route. A route without one panics.
async fn check(ctx: &Ctx, method: &str, path: &str) {
    let (a, b) = (ctx.a, ctx.b);
    let not_found = StatusCode::NOT_FOUND;
    let metric = "metric=monitorflow_cpu_usage_ratio";
    match (method, path) {
        // Tenant-free: health and session handling.
        ("GET", "/api/health") | ("GET", "/api/health/ready") => {
            let (status, body) = ctx.call(method, path, None).await;
            assert_eq!(status, StatusCode::OK);
            assert!(!Ctx::mentions(&body, b.customer));
        }
        ("POST", "/api/auth/login") => {
            let body = json!({ "username": "admin-a", "password": PASSWORD });
            let (status, body) = ctx.send(method, path, None, Some(body)).await;
            assert_eq!(status, StatusCode::OK);
            assert!(!Ctx::mentions(&body, b.customer) && !Ctx::mentions(&body, b.site));
        }
        // First-run setup (ADR-0057): users exist, so it is done and says
        // nothing else.
        ("GET", "/api/setup") => {
            let (status, body) = ctx.call(method, path, None).await;
            assert_eq!(status, StatusCode::OK);
            assert_eq!(body, json!({ "pending": false }));
        }
        ("POST", "/api/setup") => {
            let body = json!({ "token": "0".repeat(64), "username": "eve", "password": PASSWORD });
            let (status, body) = ctx.send(method, path, None, Some(body)).await;
            assert_eq!(status, StatusCode::CONFLICT);
            assert!(!Ctx::mentions(&body, b.customer) && !Ctx::mentions(&body, b.site));
        }
        ("POST", "/api/auth/logout") => {
            // On a throwaway session, so the shared one stays valid.
            let (_, cookie) = login(&ctx.app(), "admin-a").await;
            let (status, _) = ctx.send(method, path, Some(&cookie), None).await;
            assert_eq!(status, StatusCode::NO_CONTENT);
        }
        ("GET", "/api/auth/me") => {
            let (status, body) = ctx.call(method, path, None).await;
            assert_eq!(status, StatusCode::OK);
            assert_eq!(
                body["roles"],
                json!([{ "role": "administrator", "customer_id": a.customer, "site_id": null }])
            );
        }
        // Global: the agent releases the server holds (ADR-0053).
        ("GET", "/api/agent-releases") => {
            let (status, body) = ctx.call(method, path, None).await;
            assert_eq!(status, StatusCode::OK);
            assert!(!Ctx::mentions(&body, b.customer));
        }
        // Global and public: agent release files (ADR-0054), none cached
        // here.
        ("GET", "/agent/{version}/{file}") => {
            let (status, _) = ctx.call(method, "/agent/latest/install.sh", None).await;
            assert_eq!(status, StatusCode::NOT_FOUND);
        }
        // Server-wide install source (ADR-0054): read by customer
        // administrators, changed by global ones only.
        ("GET", "/api/settings/agent-install") => {
            let (status, body) = ctx.call(method, path, None).await;
            assert_eq!(status, StatusCode::OK);
            assert!(!Ctx::mentions(&body, b.customer));
        }
        ("PUT", "/api/settings/agent-install") => {
            let body = json!({ "source": "central" });
            assert_eq!(
                ctx.status(method, path, Some(body)).await,
                StatusCode::FORBIDDEN
            );
        }
        // Server-wide public address: the same rule.
        ("GET", "/api/settings/public-address") => {
            let (status, body) = ctx.call(method, path, None).await;
            assert_eq!(status, StatusCode::OK);
            assert!(!Ctx::mentions(&body, b.customer));
        }
        ("PUT", "/api/settings/public-address") => {
            let body = json!({ "address": "evil.example" });
            assert_eq!(
                ctx.status(method, path, Some(body)).await,
                StatusCode::FORBIDDEN
            );
        }
        // Listings: B never shows.
        ("GET", "/api/servers") => {
            let (status, body) = ctx.call(method, path, None).await;
            assert_eq!(status, StatusCode::OK);
            assert!(
                Ctx::mentions(&body, a.server) && !Ctx::mentions(&body, b.server),
                "{body}"
            );
        }
        ("GET", "/api/sites") => {
            let (status, body) = ctx.call(method, path, None).await;
            assert_eq!(status, StatusCode::OK);
            assert!(
                Ctx::mentions(&body, a.site) && !Ctx::mentions(&body, b.site),
                "{body}"
            );
        }
        ("GET", "/api/customers") => {
            let (status, body) = ctx.call(method, path, None).await;
            assert_eq!(status, StatusCode::OK);
            assert!(
                Ctx::mentions(&body, a.customer) && !Ctx::mentions(&body, b.customer),
                "{body}"
            );
        }
        ("GET", "/api/relays") | ("GET", "/api/tokens") => {
            let (status, body) = ctx.call(method, path, None).await;
            assert_eq!(status, StatusCode::OK);
            let (mine, theirs) = if path == "/api/relays" {
                (a.relay, b.relay)
            } else {
                (a.token, b.token)
            };
            assert!(
                Ctx::mentions(&body, mine) && !Ctx::mentions(&body, theirs),
                "{body}"
            );
        }
        ("GET", "/api/servers/{id}/packages") | ("GET", "/api/servers/{id}/commands") => {
            let uri = path.replace("{id}", &b.server.to_string());
            assert_eq!(ctx.status(method, &uri, None).await, not_found, "{uri}");
            let uri = path.replace("{id}", &a.server.to_string());
            let (status, body) = ctx.call(method, &uri, None).await;
            assert_eq!(status, StatusCode::OK, "{uri}");
            assert!(!Ctx::mentions(&body, b.command), "{body}");
        }
        ("POST", "/api/servers/{id}/commands") => {
            let uri = path.replace("{id}", &b.server.to_string());
            let body = json!({ "kind": "apply_updates" });
            assert_eq!(ctx.status(method, &uri, Some(body)).await, not_found);
            let queued: i64 =
                sqlx::query_scalar("SELECT count(*) FROM commands WHERE server_id = $1")
                    .bind(b.server)
                    .fetch_one(&ctx.pool)
                    .await
                    .unwrap();
            assert_eq!(queued, 1, "only B's own fixture command");
        }
        ("POST", "/api/servers/{id}/sources") | ("DELETE", "/api/servers/{id}/sources") => {
            let uri = path.replace("{id}", &b.server.to_string());
            let body = method
                .eq("POST")
                .then(|| json!({ "mirrors": [{ "name": "debian" }], "mode": "replace" }));
            assert_eq!(ctx.status(method, &uri, body).await, not_found);
            let queued: i64 =
                sqlx::query_scalar("SELECT count(*) FROM commands WHERE server_id = $1")
                    .bind(b.server)
                    .fetch_one(&ctx.pool)
                    .await
                    .unwrap();
            assert_eq!(queued, 1, "only B's own fixture command");
        }
        ("GET", "/api/commands/{id}") => {
            let uri = format!("/api/commands/{}", b.command);
            assert_eq!(ctx.status(method, &uri, None).await, not_found);
        }
        ("GET", "/api/campaigns") => {
            let (status, body) = ctx.call(method, path, None).await;
            assert_eq!(status, StatusCode::OK);
            assert!(
                Ctx::mentions(&body, a.campaign) && !Ctx::mentions(&body, b.campaign),
                "{body}"
            );
            let uri = format!("{path}?customer_id={}", b.customer);
            let (status, body) = ctx.call(method, &uri, None).await;
            assert_eq!(status, StatusCode::OK);
            assert_eq!(body["campaigns"], json!([]));
        }
        ("POST", "/api/campaigns") => {
            let body = json!({ "customer_id": b.customer, "name": "x", "site_id": b.site });
            assert_eq!(ctx.status(method, path, Some(body)).await, not_found);
            // A's campaign cannot reach B's servers or sites.
            for target in [
                json!({ "server_ids": [b.server] }),
                json!({ "site_id": b.site }),
            ] {
                let mut body = json!({ "customer_id": a.customer, "name": "x" });
                body.as_object_mut()
                    .unwrap()
                    .extend(target.as_object().unwrap().clone());
                let (status, answer) = ctx.call(method, path, Some(body)).await;
                assert_eq!(status, StatusCode::BAD_REQUEST, "{answer}");
            }
            let count: i64 = sqlx::query_scalar("SELECT count(*) FROM campaigns")
                .fetch_one(&ctx.pool)
                .await
                .unwrap();
            assert_eq!(count, 2, "only the fixtures");
        }
        ("GET", "/api/campaigns/{id}") => {
            let uri = format!("/api/campaigns/{}", b.campaign);
            assert_eq!(ctx.status(method, &uri, None).await, not_found);
            let uri = format!("/api/campaigns/{}", a.campaign);
            let (status, body) = ctx.call(method, &uri, None).await;
            assert_eq!(status, StatusCode::OK);
            assert!(
                Ctx::mentions(&body, a.server) && !Ctx::mentions(&body, b.server),
                "{body}"
            );
        }
        ("POST", "/api/campaigns/{id}/start")
        | ("POST", "/api/campaigns/{id}/pause")
        | ("POST", "/api/campaigns/{id}/resume")
        | ("POST", "/api/campaigns/{id}/stop") => {
            let uri = path.replace("{id}", &b.campaign.to_string());
            assert_eq!(ctx.status(method, &uri, None).await, not_found);
            let state: String = sqlx::query_scalar("SELECT state FROM campaigns WHERE id = $1")
                .bind(b.campaign)
                .fetch_one(&ctx.pool)
                .await
                .unwrap();
            assert_eq!(state, "draft");
        }
        ("GET", "/api/remediation/rules") => {
            let (status, body) = ctx.call(method, path, None).await;
            assert_eq!(status, StatusCode::OK);
            assert!(
                Ctx::mentions(&body, a.remediation_rule)
                    && !Ctx::mentions(&body, b.remediation_rule),
                "{body}"
            );
            let uri = format!("{path}?customer_id={}", b.customer);
            let (_, body) = ctx.call(method, &uri, None).await;
            assert_eq!(body["rules"], json!([]));
        }
        ("POST", "/api/remediation/rules") => {
            let body = json!({ "customer_id": b.customer, "kind": "deny", "pattern": "x" });
            assert_eq!(ctx.status(method, path, Some(body)).await, not_found);
            let body = json!({
                "customer_id": a.customer, "site_id": b.site, "kind": "deny", "pattern": "x"
            });
            assert_eq!(
                ctx.status(method, path, Some(body)).await,
                StatusCode::BAD_REQUEST
            );
            let count: i64 = sqlx::query_scalar("SELECT count(*) FROM remediation_rules")
                .fetch_one(&ctx.pool)
                .await
                .unwrap();
            assert_eq!(count, 2, "only the fixtures");
        }
        ("DELETE", "/api/remediation/rules/{id}") => {
            let uri = format!("/api/remediation/rules/{}", b.remediation_rule);
            assert_eq!(ctx.status(method, &uri, None).await, not_found);
            let kept: i64 =
                sqlx::query_scalar("SELECT count(*) FROM remediation_rules WHERE id = $1")
                    .bind(b.remediation_rule)
                    .fetch_one(&ctx.pool)
                    .await
                    .unwrap();
            assert_eq!(kept, 1);
        }
        ("GET", "/api/remediation/policies") => {
            let (status, body) = ctx.call(method, path, None).await;
            assert_eq!(status, StatusCode::OK);
            assert!(
                Ctx::mentions(&body, a.customer) && !Ctx::mentions(&body, b.customer),
                "{body}"
            );
            let uri = format!("{path}?customer_id={}", b.customer);
            let (_, body) = ctx.call(method, &uri, None).await;
            assert_eq!(body["policies"], json!([]));
        }
        ("PUT", "/api/remediation/policies") => {
            let body = json!({ "customer_id": b.customer, "low_impact_auto": false });
            assert_eq!(ctx.status(method, path, Some(body)).await, not_found);
            let body = json!({
                "customer_id": a.customer, "site_id": b.site, "low_impact_auto": false
            });
            assert_eq!(
                ctx.status(method, path, Some(body)).await,
                StatusCode::BAD_REQUEST
            );
            let auto: bool = sqlx::query_scalar(
                "SELECT low_impact_auto FROM remediation_policies WHERE customer_id = $1",
            )
            .bind(b.customer)
            .fetch_one(&ctx.pool)
            .await
            .unwrap();
            assert!(auto);
        }
        ("POST", "/api/remediation/classify") => {
            let body = json!({ "customer_id": b.customer, "command": "reboot" });
            assert_eq!(ctx.status(method, path, Some(body)).await, not_found);
            let body = json!({ "customer_id": a.customer, "site_id": b.site, "command": "x" });
            assert_eq!(
                ctx.status(method, path, Some(body)).await,
                StatusCode::BAD_REQUEST
            );
        }
        ("GET", "/api/relays/{id}") => {
            let uri = format!("/api/relays/{}", b.relay);
            assert_eq!(ctx.status(method, &uri, None).await, not_found);
        }
        ("POST", "/api/servers/{id}/revoke") | ("POST", "/api/relays/{id}/revoke") => {
            let id = if path.starts_with("/api/servers") {
                b.server
            } else {
                b.relay
            };
            let uri = path.replace("{id}", &id.to_string());
            assert_eq!(ctx.status(method, &uri, Some(json!({}))).await, not_found);
        }
        ("POST", "/api/relays/{id}/update") => {
            let uri = path.replace("{id}", &b.relay.to_string());
            let body = Some(json!({ "version": "0.2.0" }));
            assert_eq!(ctx.status(method, &uri, body).await, not_found);
        }
        ("GET", "/api/servers/{id}/proxmox")
        | ("PUT", "/api/servers/{id}/proxmox")
        | ("DELETE", "/api/servers/{id}/proxmox") => {
            let uri = path.replace("{id}", &b.server.to_string());
            let body = method
                .eq("PUT")
                .then(|| json!({ "cluster": "pve", "node": "pve1", "vmid": 101 }));
            assert_eq!(ctx.status(method, &uri, body).await, not_found);
            let mapped: i64 = sqlx::query_scalar("SELECT count(*) FROM server_proxmox_vms")
                .fetch_one(&ctx.pool)
                .await
                .unwrap();
            assert_eq!(mapped, 0);
        }
        ("POST", "/api/tokens") => {
            let body = json!({ "role": "agent", "peer_id": "intruder", "site_id": b.site });
            assert_eq!(ctx.status(method, path, Some(body)).await, not_found);
        }
        ("GET", "/api/install-keys") => {
            let (status, body) = ctx.call(method, path, None).await;
            assert_eq!(status, StatusCode::OK);
            let text = body.to_string();
            assert!(
                Ctx::mentions(&body, a.install_key) && !Ctx::mentions(&body, b.install_key),
                "{body}"
            );
            assert!(
                text.contains("pending-a") && !text.contains("pending-b"),
                "{body}"
            );
        }
        ("POST", "/api/install-keys") => {
            let body = json!({ "site_id": b.site });
            assert_eq!(ctx.status(method, path, Some(body)).await, not_found);
        }
        ("DELETE", "/api/install-keys/{id}") => {
            let uri = format!("/api/install-keys/{}", b.install_key);
            assert_eq!(ctx.status(method, &uri, None).await, not_found);
        }
        ("POST", "/api/pending-hosts/{peer_id}/approve")
        | ("POST", "/api/pending-hosts/{peer_id}/reject") => {
            let uri = path.replace("{peer_id}", "pending-b");
            assert_eq!(ctx.status(method, &uri, None).await, not_found);
        }
        ("DELETE", "/api/tokens/{id}") => {
            let uri = format!("/api/tokens/{}", b.token);
            assert_eq!(ctx.status(method, &uri, None).await, not_found);
        }
        ("GET", "/api/incidents") => {
            for query in [
                String::new(),
                format!("?server_id={}", b.server),
                format!("?site_id={}", b.site),
                format!("?state=open&severity=warning&server_id={}", b.server),
            ] {
                let (status, body) = ctx.call(method, &format!("{path}{query}"), None).await;
                assert_eq!(status, StatusCode::OK, "{query}");
                assert!(!Ctx::mentions(&body, b.incident), "{query}: {body}");
            }
            let (_, body) = ctx.call(method, path, None).await;
            assert!(Ctx::mentions(&body, a.incident));
        }
        // Mirrors are global: a customer administrator sees none of them.
        ("GET", "/api/mirrors")
        | ("GET", "/api/mirrors/{id}")
        | ("POST", "/api/mirrors/{id}/sync")
        | ("POST", "/api/mirrors/{id}/snapshots") => {
            let uri = path.replace("{id}", &ctx.mirror.to_string());
            let body = method.eq("POST").then(|| json!({}));
            assert_eq!(ctx.status(method, &uri, body).await, StatusCode::FORBIDDEN);
        }
        // The server is shared: updating it is for global administrators.
        ("GET", "/api/server-update")
        | ("POST", "/api/server-update")
        | ("POST", "/api/server-update/check") => {
            let body = method.eq("POST").then(|| json!({ "version": "99.0.0" }));
            assert_eq!(ctx.status(method, path, body).await, StatusCode::FORBIDDEN);
        }
        // The release cache is shared too.
        ("POST", "/api/agent-releases/import") | ("POST", "/api/agent-releases/import-url") => {
            let body = Some(json!({ "url": "http://files.example/" }));
            assert_eq!(ctx.status(method, path, body).await, StatusCode::FORBIDDEN);
        }
        // Backups hold every customer's data: global administrators only.
        ("GET", "/api/backups") | ("POST", "/api/backups") | ("GET", "/api/backups/{name}") => {
            let uri = path.replace("{name}", "monitorflow-backup-20261029T030000Z-daily.tar.gz");
            assert_eq!(ctx.status(method, &uri, None).await, StatusCode::FORBIDDEN);
        }
        // Automatic updates are server-wide: global administrators only.
        ("GET", "/api/auto-update")
        | ("GET", "/api/settings/auto-update")
        | ("PUT", "/api/settings/auto-update")
        | ("POST", "/api/auto-update/rollouts/{kind}/{version}/resume") => {
            let uri = path
                .replace("{kind}", "agent")
                .replace("{version}", "0.1.0");
            let body = method.ne("GET").then(|| json!({}));
            assert_eq!(ctx.status(method, &uri, body).await, StatusCode::FORBIDDEN);
        }
        // Server-wide, and no customer's data: every user reads whether AI
        // is on, for Approvals; only global administrators change it.
        ("GET", "/api/settings/ai") => {
            assert_eq!(ctx.status(method, path, None).await, StatusCode::OK);
        }
        ("PUT", "/api/settings/ai")
        | ("DELETE", "/api/settings/ai")
        | ("POST", "/api/settings/ai/test") => {
            let body = match method {
                "PUT" => Some(json!({ "model": "claude-opus-5-5", "enabled": false })),
                "POST" => Some(json!({})),
                _ => None,
            };
            assert_eq!(ctx.status(method, path, body).await, StatusCode::FORBIDDEN);
        }
        // Notification channels may cover every customer: global
        // administrators only (ADR-0060).
        ("GET", "/api/notifications/channels")
        | ("POST", "/api/notifications/channels")
        | ("PUT", "/api/notifications/channels/{id}")
        | ("DELETE", "/api/notifications/channels/{id}")
        | ("POST", "/api/notifications/channels/{id}/test") => {
            let uri = path.replace("{id}", &Uuid::new_v4().to_string());
            let body = matches!(method, "POST" | "PUT")
                .then(|| json!({ "kind": "webhook", "settings": { "url": "https://x.example/" } }));
            assert_eq!(ctx.status(method, &uri, body).await, StatusCode::FORBIDDEN);
        }
        // Creation: a customer admin cannot create customers, nor sites of B.
        ("POST", "/api/customers") => {
            assert_eq!(
                ctx.status(method, path, Some(json!({ "name": "new" })))
                    .await,
                StatusCode::FORBIDDEN
            );
        }
        ("PUT", "/api/customers/{id}") => {
            let uri = format!("/api/customers/{}", b.customer);
            assert_eq!(
                ctx.status(method, &uri, Some(json!({ "name": "mine" })))
                    .await,
                StatusCode::FORBIDDEN
            );
        }
        // Users (ADR-0059): global administrators only.
        ("GET", "/api/users")
        | ("POST", "/api/users")
        | ("DELETE", "/api/users/{id}")
        | ("POST", "/api/users/{id}/link")
        | ("PUT", "/api/users/{id}/disabled")
        | ("POST", "/api/users/{id}/roles")
        | ("DELETE", "/api/users/{id}/roles") => {
            let body = json!({ "username": "eve", "role": "administrator", "disabled": true });
            let body = method.ne("GET").then_some(body);
            assert_eq!(
                ctx.status(method, &concrete(path), body).await,
                StatusCode::FORBIDDEN
            );
        }
        // Password links: no session, and no valid link.
        ("POST", "/api/password-link") | ("POST", "/api/password-link/use") => {
            let body = json!({ "token": "0".repeat(64), "password": PASSWORD });
            let (status, body) = ctx.send(method, path, None, Some(body)).await;
            assert_eq!(status, StatusCode::GONE);
            assert!(!Ctx::mentions(&body, b.customer) && !Ctx::mentions(&body, b.site));
        }
        ("POST", "/api/customers/{id}/sites") => {
            let uri = format!("/api/customers/{}/sites", b.customer);
            assert_eq!(
                ctx.status(method, &uri, Some(json!({ "name": "intruder" })))
                    .await,
                not_found
            );
        }
        // One resource of B: 404, as if it did not exist.
        ("GET", "/api/servers/{id}") => {
            assert_eq!(
                ctx.status(method, &format!("/api/servers/{}", b.server), None)
                    .await,
                not_found
            );
        }
        ("GET", "/api/remediation/incidents") => {
            for query in [
                String::new(),
                format!("?customer_id={}", b.customer),
                format!("?server_id={}", b.server),
                format!("?site_id={}", b.site),
                format!("?alert_incident_id={}", b.incident),
            ] {
                let (status, body) = ctx.call(method, &format!("{path}{query}"), None).await;
                assert_eq!(status, StatusCode::OK, "{query}");
                assert!(!Ctx::mentions(&body, b.remediation), "{query}: {body}");
            }
            let (_, body) = ctx.call(method, path, None).await;
            assert!(Ctx::mentions(&body, a.remediation));
        }
        ("GET", "/api/remediation/approvals") => {
            for query in [
                String::new(),
                format!("?customer_id={}", b.customer),
                format!("?server_id={}", b.server),
                format!("?site_id={}", b.site),
            ] {
                let (status, body) = ctx.call(method, &format!("{path}{query}"), None).await;
                assert_eq!(status, StatusCode::OK, "{query}");
                assert!(!Ctx::mentions(&body, b.pending), "{query}: {body}");
            }
            let (_, body) = ctx.call(method, path, None).await;
            assert!(Ctx::mentions(&body, a.pending), "{body}");
        }
        ("POST", "/api/remediation/approvals/{id}/approve")
        | ("POST", "/api/remediation/approvals/{id}/reject") => {
            let uri = path.replace("{id}", &b.pending.to_string());
            let body = if path.ends_with("reject") {
                json!({ "reason": "no" })
            } else {
                json!({})
            };
            assert_eq!(
                ctx.status(method, &uri, Some(body)).await,
                not_found,
                "{uri}"
            );
            let state: String =
                sqlx::query_scalar("SELECT state FROM remediation_pending_commands WHERE id = $1")
                    .bind(b.pending)
                    .fetch_one(&ctx.pool)
                    .await
                    .unwrap();
            assert_eq!(state, "pending");
        }
        ("GET", "/api/remediation/events") => {
            for query in [
                String::new(),
                format!("?customer_id={}", b.customer),
                format!("?server_id={}", b.server),
                format!("?site_id={}", b.site),
                format!("?incident_id={}", b.remediation),
            ] {
                let (status, body) = ctx.call(method, &format!("{path}{query}"), None).await;
                assert_eq!(status, StatusCode::OK, "{query}");
                assert!(!Ctx::mentions(&body, b.remediation), "{query}: {body}");
            }
            let (_, body) = ctx.call(method, path, None).await;
            assert!(Ctx::mentions(&body, a.remediation), "{body}");
        }
        ("GET", "/api/remediation/incidents/{id}") => {
            let uri = format!("/api/remediation/incidents/{}", b.remediation);
            assert_eq!(ctx.status(method, &uri, None).await, not_found);
        }
        ("GET", "/api/incidents/{id}") => {
            assert_eq!(
                ctx.status(method, &format!("/api/incidents/{}", b.incident), None)
                    .await,
                not_found
            );
        }
        ("GET", "/api/servers/{id}/sensors")
        | ("GET", "/api/servers/{id}/thresholds/effective")
        | ("GET", "/api/servers/{id}/sensor_configs")
        | ("GET", "/api/servers/{id}/thresholds") => {
            let uri = path.replace("{id}", &b.server.to_string());
            assert_eq!(ctx.status(method, &uri, None).await, not_found, "{uri}");
        }
        ("GET", "/api/servers/{id}/metrics") => {
            let uri = format!("/api/servers/{}/metrics", b.server);
            assert_eq!(ctx.status(method, &uri, None).await, not_found);
            let uri = format!("/api/servers/{}/metrics", a.server);
            assert_eq!(ctx.status(method, &uri, None).await, StatusCode::OK);
            let last = ctx.vm_seen_last();
            assert!(
                last.contains("site=\"site-a\"") && last.contains("extra_label=site=site-a"),
                "{last}"
            );
        }
        ("PUT", "/api/sites/{id}/agent-auto-update") => {
            let uri = path.replace("{id}", &b.site.to_string());
            let body = Some(json!({ "enabled": true }));
            assert_eq!(ctx.status(method, &uri, body).await, not_found, "{uri}");
        }
        ("PUT", "/api/sites/{id}/offline-alerts")
        | ("PUT", "/api/servers/{id}/offline-alerts")
        | ("PUT", "/api/relays/{id}/offline-alerts") => {
            let id = match path.split('/').nth(2) {
                Some("sites") => b.site,
                Some("servers") => b.server,
                _ => b.relay,
            };
            let uri = path.replace("{id}", &id.to_string());
            let body = Some(json!({ "enabled": false }));
            assert_eq!(ctx.status(method, &uri, body).await, not_found, "{uri}");
        }
        ("GET", "/api/sites/{id}/sensor_configs")
        | ("GET", "/api/sites/{id}/thresholds")
        | ("GET", "/api/sites/{id}/sensors")
        | ("GET", "/api/sites/{id}/thresholds/effective") => {
            let uri = path.replace("{id}", &b.site.to_string());
            assert_eq!(ctx.status(method, &uri, None).await, not_found, "{uri}");
        }
        ("POST", "/api/servers/{id}/sensor_configs")
        | ("POST", "/api/sites/{id}/sensor_configs") => {
            for id in [b.server, b.site] {
                let uri = path.replace("{id}", &id.to_string());
                assert_eq!(
                    ctx.status(method, &uri, Some(sensor_body())).await,
                    not_found,
                    "{uri}"
                );
            }
        }
        ("POST", "/api/servers/{id}/thresholds") | ("POST", "/api/sites/{id}/thresholds") => {
            for id in [b.server, b.site] {
                let uri = path.replace("{id}", &id.to_string());
                assert_eq!(
                    ctx.status(method, &uri, Some(threshold_body())).await,
                    not_found,
                    "{uri}"
                );
            }
        }
        ("PUT", "/api/sensor_configs/{id}") | ("DELETE", "/api/sensor_configs/{id}") => {
            for id in [b.site_sensor, b.server_sensor] {
                let uri = format!("/api/sensor_configs/{id}");
                assert_eq!(
                    ctx.status(method, &uri, Some(sensor_body())).await,
                    not_found,
                    "{uri}"
                );
            }
        }
        ("PUT", "/api/thresholds/{id}") | ("DELETE", "/api/thresholds/{id}") => {
            for id in [b.site_threshold, b.server_threshold] {
                let uri = format!("/api/thresholds/{id}");
                assert_eq!(
                    ctx.status(method, &uri, Some(threshold_body())).await,
                    not_found,
                    "{uri}"
                );
            }
        }
        // Metric proxy: B is 404 and never queried; A is queried with A's
        // site only.
        ("GET", "/api/metrics/query") | ("GET", "/api/metrics/query_range") => {
            let uri = format!("{path}?server_id={}&{metric}", b.server);
            assert_eq!(ctx.status(method, &uri, None).await, not_found);
            let uri = format!("{path}?server_id={}&{metric}&agg=sum", a.server);
            assert_eq!(ctx.status(method, &uri, None).await, StatusCode::OK);
            let last = ctx.vm_seen_last();
            assert!(
                last.contains("site=\"site-a\"") && last.contains("extra_label=site=site-a"),
                "{last}"
            );
        }
        _ => panic!(
            "no tenancy case for {method} {path}; add one to check() in tests/api_tenancy.rs"
        ),
    }
}

/// Logs `username` in; returns the login body and the session cookie.
async fn login(app: &Router, username: &str) -> (Value, String) {
    let body = json!({ "username": username, "password": PASSWORD }).to_string();
    let request = Request::post("/api/auth/login")
        .header("content-type", "application/json")
        .header(CSRF_HEADER, "1")
        .body(Body::from(body))
        .unwrap();
    let response = app.clone().oneshot(request).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let cookie = response.headers()["set-cookie"]
        .to_str()
        .unwrap()
        .split(';')
        .next()
        .unwrap()
        .to_owned();
    let bytes = axum::body::to_bytes(response.into_body(), 1 << 20)
        .await
        .unwrap();
    (serde_json::from_slice(&bytes).unwrap(), cookie)
}

#[test]
fn route_source_parsing_finds_methods() {
    let routes = source_routes();
    for expected in [
        ("GET", "/api/servers"),
        ("PUT", "/api/sensor_configs/{id}"),
        ("DELETE", "/api/thresholds/{id}"),
        ("POST", "/api/customers"),
    ] {
        assert!(
            routes.contains(&(expected.0.into(), expected.1.into())),
            "{expected:?}"
        );
    }
}

db_tests! {
    #[sqlx::test]
    async fn every_route_keeps_customers_apart(pool: PgPool) {
        let vm = FakeVm::default();
        let vm_url = vm.start().await;
        let a = tenant(&pool, "a").await;
        let b = tenant(&pool, "b").await;
        users::create_user_with(&pool, "admin-a", &Secret::new(PASSWORD), Role::Administrator, GrantScope::Customer(a.customer), "test")
            .await
            .unwrap();
        let mirror = sqlx::query_scalar(
            "INSERT INTO mirrors (name, family, upstream_url, distribution, components)
             VALUES ('debian', 'apt', 'https://deb.debian.org/debian', 'bookworm', '{main}') RETURNING id",
        )
        .fetch_one(&pool)
        .await
        .unwrap();
        let mut ctx = Ctx { pool: pool.clone(), vm: vm.clone(), vm_url, cookie: String::new(), a, b, mirror };
        ctx.cookie = login(&ctx.app(), "admin-a").await.1;

        // The parsed list is the router's: listed methods are routed, the
        // others on the same paths are 405.
        let routes = source_routes();
        let paths: BTreeSet<&String> = routes.iter().map(|(_, p)| p).collect();
        for path in paths {
            for method in METHODS.map(str::to_uppercase) {
                let status = ctx.send(&method, &concrete(path), None, None).await.0;
                let listed = routes.contains(&(method.clone(), path.clone()));
                assert_eq!(status == StatusCode::METHOD_NOT_ALLOWED, !listed, "{method} {path}: {status}");
            }
        }

        let before = |table: &'static str| {
            let pool = pool.clone();
            async move {
                sqlx::query_scalar::<_, String>(&format!(
                    "SELECT string_agg(t::text, ',' ORDER BY id) FROM {table} t"
                ))
                .fetch_one(&pool)
                .await
                .unwrap()
            }
        };
        let rows = (before("sensor_configs").await, before("thresholds").await, before("sites").await, before("enrollment_tokens").await, before("relays").await, before("servers").await);

        for (method, path) in &routes {
            check(&ctx, method, path).await;
        }

        let untouched: (i64, i64) = sqlx::query_as(
            "SELECT (SELECT count(*) FROM pending_hosts),
                    (SELECT count(*) FROM install_keys WHERE revoked_at IS NULL)",
        )
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(untouched, (2, 2), "an administrator of A decided on B's hosts or keys");
        let runs: i64 = sqlx::query_scalar("SELECT count(*) FROM mirror_runs").fetch_one(&pool).await.unwrap();
        assert_eq!(runs, 0, "a customer administrator started a mirror job");
        // Writes against B changed nothing (A's rows were not touched either).
        assert_eq!(rows, (before("sensor_configs").await, before("thresholds").await, before("sites").await, before("enrollment_tokens").await, before("relays").await, before("servers").await));
        // Nothing about B ever reached VictoriaMetrics, while A's queries did.
        let seen = vm.seen();
        assert!(!seen.is_empty());
        for query in seen {
            assert!(!query.contains("site-b") && !query.contains("agent-b"), "{query}");
        }
    }
}
