//! Dashboards (ADR-0095) against a real PostgreSQL and a fake
//! VictoriaMetrics that records every request: who sees and edits what,
//! concurrent edits, audit, and what panels show when the stored spec was
//! edited by hand, a server was removed, or the same dashboard is opened
//! twice. Ignored without `DATABASE_URL`.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::body::Body;
use axum::extract::{OriginalUri, State};
use axum::http::{Request, StatusCode};
use axum::Router;
use monitorflow_common::redact::Secret;
use monitorflow_server::api::auth::CSRF_HEADER;
use monitorflow_server::api::users::{self, GrantScope};
use monitorflow_server::api::{self, ApiSettings, ApiState, Role};
use monitorflow_server::grpc::Registry;
use monitorflow_server::vm_query::VmQuery;
use serde_json::{json, Value};
use sqlx::PgPool;
use tokio::net::TcpListener;
use tower::ServiceExt;
use uuid::Uuid;

const PASSWORD: &str = "correct horse battery";

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

/// Records decoded query strings and answers one series per peer named
/// in the query, so answers show which servers were asked for.
#[derive(Clone, Default)]
struct FakeVm {
    seen: Arc<Mutex<Vec<String>>>,
}

impl FakeVm {
    async fn start(&self) -> String {
        async fn answer(State(vm): State<FakeVm>, OriginalUri(uri): OriginalUri) -> String {
            let url = reqwest::Url::parse(&format!("http://fake{uri}")).unwrap();
            let decoded: Vec<String> = url.query_pairs().map(|(k, v)| format!("{k}={v}")).collect();
            vm.seen.lock().unwrap().push(decoded.join("&"));
            let data = if url.path().contains("/label/") {
                json!(["monitorflow_cpu_usage_ratio", "up"])
            } else {
                // A series for each peer of the test, as if the backend
                // ignored every filter.
                let result: Vec<Value> = ["agent-a1", "agent-a2", "agent-b1"]
                    .iter()
                    .map(|peer| {
                        json!({"metric": {"peer": peer, "instance": "total"},
                               "values": [[1, "0.5"]], "value": [1, "0.5"]})
                    })
                    .collect();
                json!({ "resultType": "matrix", "result": result })
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

struct World {
    app: Router,
    vm: FakeVm,
    pool: PgPool,
    customer_a: Uuid,
    site_a1: Uuid,
    site_a2: Uuid,
    a1: Uuid,
    a2: Uuid,
    b1: Uuid,
}

async fn world(pool: &PgPool) -> World {
    let vm = FakeVm::default();
    let url = vm.start().await;
    let client = VmQuery::with_timeout(&url, Duration::from_millis(500)).unwrap();
    // One state, so the cache is shared between requests.
    let app = api::router(
        ApiState::new(pool.clone(), Registry::default(), ApiSettings::default())
            .with_metrics(client),
    );
    let one = |sql: &'static str| sqlx::query_scalar::<_, Uuid>(sql);
    let customer = |name: &'static str| {
        one("INSERT INTO customers (name) VALUES ($1) RETURNING id")
            .bind(name)
            .fetch_one(pool)
    };
    let (customer_a, customer_b) = (customer("a").await.unwrap(), customer("b").await.unwrap());
    let site = |name: &'static str, customer: Uuid| {
        one("INSERT INTO sites (name, customer_id) VALUES ($1, $2) RETURNING id")
            .bind(name)
            .bind(customer)
            .fetch_one(pool)
    };
    let site_a1 = site("a1", customer_a).await.unwrap();
    let site_a2 = site("a2.x", customer_a).await.unwrap();
    let site_b1 = site("b1", customer_b).await.unwrap();
    let server = |peer: &'static str, site: Uuid| {
        one("INSERT INTO servers (peer_id, site_id, hostname, os_family)
             VALUES ($1, $2, 'host-' || $1, 'debian') RETURNING id")
        .bind(peer)
        .bind(site)
        .fetch_one(pool)
    };
    let a1 = server("agent-a1", site_a1).await.unwrap();
    let a2 = server("agent-a2", site_a2).await.unwrap();
    let b1 = server("agent-b1", site_b1).await.unwrap();
    for (name, role, scope) in [
        (
            "admin",
            Role::Administrator,
            GrantScope::Customer(customer_a),
        ),
        ("viewer", Role::Viewer, GrantScope::Customer(customer_a)),
        ("operator1", Role::Operator, GrantScope::Site(site_a1)),
        ("viewer1", Role::Viewer, GrantScope::Site(site_a1)),
        ("global", Role::Administrator, GrantScope::Global),
    ] {
        users::create_user_with(pool, name, &Secret::new(PASSWORD), role, scope, "test")
            .await
            .unwrap();
    }
    World {
        app,
        vm,
        pool: pool.clone(),
        customer_a,
        site_a1,
        site_a2,
        a1,
        a2,
        b1,
    }
}

async fn send(
    app: &Router,
    cookie: &str,
    method: &str,
    uri: &str,
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
    let response = app.clone().oneshot(request).await.unwrap();
    let status = response.status();
    let bytes = axum::body::to_bytes(response.into_body(), 1 << 20)
        .await
        .unwrap();
    (
        status,
        serde_json::from_slice(&bytes).unwrap_or(Value::Null),
    )
}

async fn login(app: &Router, username: &str) -> String {
    let request = Request::post("/api/auth/login")
        .header("content-type", "application/json")
        .header(CSRF_HEADER, "1")
        .body(Body::from(
            json!({ "username": username, "password": PASSWORD }).to_string(),
        ))
        .unwrap();
    let response = app.clone().oneshot(request).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    response.headers()["set-cookie"]
        .to_str()
        .unwrap()
        .split(';')
        .next()
        .unwrap()
        .to_owned()
}

fn chart(id: &str, servers: Value) -> Value {
    json!({ "id": id, "title": "CPU", "kind": "chart", "metric": "monitorflow_cpu_usage_ratio",
            "instance": "total", "combine": "each", "servers": servers })
}

fn dashboard(name: &str, sharing: Value, panels: Value) -> Value {
    json!({ "name": name, "shared_with": sharing,
            "spec": { "version": 1, "range": "6h", "panels": panels } })
}

fn ids(body: &Value) -> Vec<String> {
    body["dashboards"]
        .as_array()
        .unwrap()
        .iter()
        .map(|d| d["name"].as_str().unwrap().to_owned())
        .collect()
}

db_tests! {
    #[sqlx::test]
    async fn roles_decide_who_sees_and_edits_each_sharing(pool: PgPool) {
        let w = world(&pool).await;
        let [admin, viewer, operator1, viewer1] = [
            login(&w.app, "admin").await,
            login(&w.app, "viewer").await,
            login(&w.app, "operator1").await,
            login(&w.app, "viewer1").await,
        ];
        let all = json!([chart("c", json!({ "kind": "all" }))]);
        let customer = json!({ "kind": "customer", "id": w.customer_a });
        let site = json!({ "kind": "site", "id": w.site_a1 });

        // A viewer keeps dashboards for itself but shares none.
        let (status, mine) = send(&w.app, &viewer, "POST", "/api/dashboards",
            Some(dashboard("Viewer's", json!({ "kind": "personal" }), all.clone()))).await;
        assert_eq!(status, StatusCode::CREATED, "{mine}");
        assert_eq!(mine["shared_with"], json!({ "kind": "personal" }));
        assert_eq!((mine["can_edit"].clone(), mine["revision"].clone()), (json!(true), json!(1)));
        let status = send(&w.app, &viewer, "POST", "/api/dashboards",
            Some(dashboard("x", customer.clone(), all.clone()))).await.0;
        assert_eq!(status, StatusCode::FORBIDDEN);

        let (status, shared) = send(&w.app, &admin, "POST", "/api/dashboards",
            Some(dashboard("Customer's", customer.clone(), all.clone()))).await;
        assert_eq!(status, StatusCode::CREATED, "{shared}");
        assert_eq!(shared["shared_with"]["name"], "a");
        let (status, site_board) = send(&w.app, &operator1, "POST", "/api/dashboards",
            Some(dashboard("Site's", site.clone(), json!([chart("c", json!({ "kind": "list", "ids": [w.a1] }))])))).await;
        assert_eq!(status, StatusCode::CREATED, "{site_board}");
        // A site's dashboard names only that site's servers.
        let status = send(&w.app, &operator1, "POST", "/api/dashboards",
            Some(dashboard("x", site.clone(), json!([chart("c", json!({ "kind": "list", "ids": [w.a2] }))])))).await.0;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        // An operator of one site cannot share with the whole customer,
        // which it does not see as a whole.
        let status = send(&w.app, &operator1, "POST", "/api/dashboards",
            Some(dashboard("x", customer.clone(), all.clone()))).await.0;
        assert_eq!(status, StatusCode::NOT_FOUND);

        let list = |cookie: String| {
            let app = w.app.clone();
            async move { ids(&send(&app, &cookie, "GET", "/api/dashboards", None).await.1) }
        };
        assert_eq!(list(viewer.clone()).await, ["Customer's", "Site's", "Viewer's"]);
        assert_eq!(list(admin.clone()).await, ["Customer's", "Site's"]);
        // A role on one site does not show the customer's dashboards.
        assert_eq!(list(viewer1.clone()).await, ["Site's"]);
        assert_eq!(list(operator1.clone()).await, ["Site's"]);

        let uri = |d: &Value| format!("/api/dashboards/{}", d["id"].as_str().unwrap());
        assert_eq!(send(&w.app, &admin, "GET", &uri(&mine), None).await.0, StatusCode::NOT_FOUND);
        assert_eq!(send(&w.app, &viewer1, "GET", &uri(&shared), None).await.0, StatusCode::NOT_FOUND);
        let (status, seen) = send(&w.app, &viewer, "GET", &uri(&shared), None).await;
        assert_eq!((status, seen["can_edit"].clone()), (StatusCode::OK, json!(false)));
        let mut change = dashboard("Renamed", customer.clone(), all.clone());
        change["revision"] = json!(1);
        assert_eq!(send(&w.app, &viewer, "PUT", &uri(&shared), Some(change.clone())).await.0, StatusCode::FORBIDDEN);
        assert_eq!(send(&w.app, &viewer, "DELETE", &uri(&shared), None).await.0, StatusCode::FORBIDDEN);

        // Two editors from revision 1: the second is refused.
        let (status, saved) = send(&w.app, &admin, "PUT", &uri(&shared), Some(change.clone())).await;
        assert_eq!((status, saved["revision"].clone(), saved["name"].clone()), (StatusCode::OK, json!(2), json!("Renamed")));
        let (status, body) = send(&w.app, &admin, "PUT", &uri(&shared), Some(change)).await;
        assert_eq!(status, StatusCode::CONFLICT);
        assert!(body["error"].as_str().unwrap().contains("Someone changed"), "{body}");

        // Sharing a personal dashboard needs the right on the new sharing.
        let mut share = dashboard("Viewer's", customer.clone(), all.clone());
        share["revision"] = json!(1);
        assert_eq!(send(&w.app, &viewer, "PUT", &uri(&mine), Some(share)).await.0, StatusCode::FORBIDDEN);

        assert_eq!(send(&w.app, &operator1, "DELETE", &uri(&site_board), None).await.0, StatusCode::NO_CONTENT);
        assert_eq!(send(&w.app, &operator1, "GET", &uri(&site_board), None).await.0, StatusCode::NOT_FOUND);

        let actions: Vec<String> = sqlx::query_scalar(
            "SELECT action FROM audit_log WHERE target_kind = 'dashboard' ORDER BY id",
        )
        .fetch_all(&w.pool)
        .await
        .unwrap();
        assert_eq!(actions, ["dashboard.create", "dashboard.create", "dashboard.create", "dashboard.update", "dashboard.delete"]);
    }

    #[sqlx::test]
    async fn bad_specs_are_refused_with_a_sentence(pool: PgPool) {
        let w = world(&pool).await;
        let admin = login(&w.app, "admin").await;
        for (spec, words) in [
            (json!({ "version": 2, "range": "6h", "panels": [] }), "newer version"),
            (json!({ "version": 1, "range": "6h", "panels": [{ "id": "a", "title": "x", "kind": "chart",
                "metric": "up", "combine": "avg" }] }), "metric must be"),
            (json!({ "version": 1, "range": "6h", "panels": [{ "id": "a", "title": "x", "kind": "chart",
                "metric": "monitorflow_cpu_usage_ratio", "combine": "avg", "query": "up" }] }), "unknown field"),
        ] {
            let body = json!({ "name": "x", "shared_with": { "kind": "personal" }, "spec": spec });
            let (status, body) = send(&w.app, &admin, "POST", "/api/dashboards", Some(body)).await;
            assert_eq!(status, StatusCode::BAD_REQUEST);
            assert!(body["error"].as_str().unwrap().contains(words), "{body}");
        }
        let body = dashboard(" ", json!({ "kind": "personal" }), json!([]));
        assert_eq!(send(&w.app, &admin, "POST", "/api/dashboards", Some(body)).await.0, StatusCode::BAD_REQUEST);
    }

    #[sqlx::test]
    async fn panels_show_only_what_the_viewer_reads(pool: PgPool) {
        let w = world(&pool).await;
        let viewer1 = login(&w.app, "viewer1").await;
        // Edited by hand: a site dashboard whose panels name every server,
        // the other customer's included.
        let panels = json!([
            chart("each", json!({ "kind": "list", "ids": [w.a1, w.a2, w.b1] })),
            chart("all", json!({ "kind": "all" })),
            { "id": "top", "title": "Top", "kind": "top", "metric": "monitorflow_cpu_usage_ratio",
              "order": "highest", "count": 5, "servers": { "kind": "all" } },
        ]);
        let request = |within: Value| json!({ "range": "1h", "within": within, "panels": panels });
        let (status, body) = send(&w.app, &viewer1, "POST", "/api/dashboard-data",
            Some(request(json!({ "kind": "all" })))).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        let shown = body.to_string();
        assert!(shown.contains("host-agent-a1"), "{body}");
        assert!(!shown.contains("agent-a2") && !shown.contains("agent-b1"), "{body}");
        assert_eq!(body["panels"][0]["servers"], 1);
        assert_eq!(body["panels"][0]["unavailable"], 2);
        assert_eq!(body["panels"][0]["kind"], "chart");
        assert_eq!(body["panels"][2]["rows"].as_array().unwrap().len(), 1);
        for query in w.vm.seen() {
            assert!(query.contains(r#"extra_filters[]={site=~"a1"}"#), "{query}");
            assert!(!query.contains("a2") && !query.contains("b1"), "{query}");
        }

        // The same dashboard again is answered from the cache.
        let before = w.vm.seen().len();
        send(&w.app, &viewer1, "POST", "/api/dashboard-data", Some(request(json!({ "kind": "all" })))).await;
        assert_eq!(w.vm.seen().len(), before);

        // Within another site of the customer: nothing, and no query.
        let (status, body) = send(&w.app, &viewer1, "POST", "/api/dashboard-data",
            Some(request(json!({ "kind": "site", "id": w.site_a2 })))).await;
        assert_eq!(status, StatusCode::OK);
        assert!(body["panels"].as_array().unwrap().iter().all(|p| p["servers"] == 0), "{body}");
        assert_eq!(w.vm.seen().len(), before);
    }

    #[sqlx::test]
    async fn removed_servers_leave_panels_and_queries(pool: PgPool) {
        let w = world(&pool).await;
        let global = login(&w.app, "global").await;
        sqlx::query("UPDATE servers SET revoked_at = now() WHERE id = $1")
            .bind(w.a2)
            .execute(&w.pool)
            .await
            .unwrap();
        let request = json!({ "range": "24h", "within": { "kind": "customer", "id": w.customer_a }, "panels": [
            chart("all", json!({ "kind": "all" })),
            chart("named", json!({ "kind": "list", "ids": [w.a2] })),
            { "id": "f", "title": "Servers", "kind": "figure", "metric": "monitorflow_cpu_usage_ratio",
              "combine": "count", "servers": { "kind": "site", "id": w.site_a2 } },
        ] });
        let (status, body) = send(&w.app, &global, "POST", "/api/dashboard-data", Some(request)).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(body["panels"][0]["servers"], 1);
        assert_eq!(body["panels"][1]["servers"], 0);
        assert_eq!(body["panels"][1]["unavailable"], 1);
        assert_eq!(body["panels"][1]["series"], json!([]));
        assert_eq!(body["panels"][2]["value"], Value::Null);
        let seen = w.vm.seen();
        assert_eq!(seen.len(), 1, "{seen:?}");
        // The rollup of a day, the 8 worst, one site, its removed server left out.
        assert!(seen[0].contains(r#"topk_max(8, max_over_time(monitorflow_cpu_usage_ratio{site=~"a1",instance="total"}[300s]))"#), "{seen:?}");
        assert!(!seen[0].contains("b1"), "{seen:?}");

        // The catalogue lists only monitorflow metrics.
        let (status, body) = send(&w.app, &global, "GET",
            &format!("/api/dashboard-metrics?customer_id={}&metric=monitorflow_cpu_usage_ratio", w.customer_a), None).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["metrics"], json!(["monitorflow_cpu_usage_ratio"]));
        let last = w.vm.seen().pop().unwrap();
        assert!(last.contains(r#"site=~"a1|a2\\.x""#), "{last}");
    }
}
