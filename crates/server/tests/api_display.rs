//! Display links (ADR-0099) against a real PostgreSQL and a fake
//! VictoriaMetrics: who may make and turn off links, what a screen reads
//! and nothing else, expiry, the maker losing their rights or account,
//! tenancy between customers, rate limits and audit. Ignored without
//! `DATABASE_URL`.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::body::Body;
use axum::extract::{OriginalUri, State};
use axum::http::{Request, StatusCode};
use axum::Router;
use monitorflow_common::redact::Secret;
use monitorflow_server::api::auth::CSRF_HEADER;
use monitorflow_server::api::display::{REQUESTS_PER_MINUTE, STARTS_PER_HOUR};
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

/// Records decoded queries and answers a series for every peer of the
/// test, as if the backend ignored every filter.
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
            let result: Vec<Value> = ["agent-a1", "agent-b1"]
                .iter()
                .map(|peer| json!({"metric": {"peer": peer}, "value": [1, "0.5"], "values": [[1, "0.5"]]}))
                .collect();
            json!({ "status": "success", "data": { "resultType": "vector", "result": result } })
                .to_string()
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
    customer_b: Uuid,
    site_a1: Uuid,
}

async fn world(pool: &PgPool) -> World {
    let vm = FakeVm::default();
    let url = vm.start().await;
    let client = VmQuery::with_timeout(&url, Duration::from_millis(500)).unwrap();
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
    let site_b1 = site("b1", customer_b).await.unwrap();
    for (peer, site) in [("agent-a1", site_a1), ("agent-b1", site_b1)] {
        sqlx::query(
            "INSERT INTO servers (peer_id, site_id, hostname, os_family)
             VALUES ($1, $2, 'host-' || $1, 'debian')",
        )
        .bind(peer)
        .bind(site)
        .execute(pool)
        .await
        .unwrap();
    }
    for (name, role, scope) in [
        (
            "admin_a",
            Role::Administrator,
            GrantScope::Customer(customer_a),
        ),
        (
            "admin_b",
            Role::Administrator,
            GrantScope::Customer(customer_b),
        ),
        ("viewer_a", Role::Viewer, GrantScope::Customer(customer_a)),
        ("operator1", Role::Operator, GrantScope::Site(site_a1)),
        (
            "site_admin1",
            Role::Administrator,
            GrantScope::Site(site_a1),
        ),
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
        customer_b,
        site_a1,
    }
}

struct Answer {
    status: StatusCode,
    body: Value,
    cookie: Option<String>,
}

async fn call(
    app: &Router,
    cookie: &str,
    method: &str,
    uri: &str,
    body: Option<Value>,
    source: Option<&str>,
) -> Answer {
    let mut request = Request::builder()
        .method(method)
        .uri(uri)
        .header(CSRF_HEADER, "1")
        .header("content-type", "application/json")
        .header("cookie", cookie);
    if let Some(source) = source {
        request = request.header("x-forwarded-for", source);
    }
    let request = request
        .body(body.map_or_else(Body::empty, |b| Body::from(b.to_string())))
        .unwrap();
    let response = app.clone().oneshot(request).await.unwrap();
    let status = response.status();
    let cookie = response
        .headers()
        .get("set-cookie")
        .map(|v| v.to_str().unwrap().split(';').next().unwrap().to_owned());
    let bytes = axum::body::to_bytes(response.into_body(), 1 << 20)
        .await
        .unwrap();
    Answer {
        status,
        body: serde_json::from_slice(&bytes).unwrap_or(Value::Null),
        cookie,
    }
}

async fn send(
    app: &Router,
    cookie: &str,
    method: &str,
    uri: &str,
    body: Option<Value>,
) -> (StatusCode, Value) {
    let answer = call(app, cookie, method, uri, body, None).await;
    (answer.status, answer.body)
}

async fn login(app: &Router, username: &str) -> String {
    let answer = call(
        app,
        "",
        "POST",
        "/api/auth/login",
        Some(json!({ "username": username, "password": PASSWORD })),
        None,
    )
    .await;
    assert_eq!(answer.status, StatusCode::OK);
    answer.cookie.unwrap()
}

fn figure(id: &str, servers: Value) -> Value {
    json!({ "id": id, "title": "Average CPU", "kind": "figure", "metric": "monitorflow_cpu_usage_ratio",
            "combine": "avg", "servers": servers })
}

fn top(id: &str) -> Value {
    json!({ "id": id, "title": "Busiest", "kind": "top", "metric": "monitorflow_cpu_usage_ratio",
            "order": "highest", "count": 5, "servers": { "kind": "all" } })
}

async fn dashboard(
    app: &Router,
    cookie: &str,
    name: &str,
    sharing: Value,
    panels: Value,
) -> String {
    let body = json!({ "name": name, "shared_with": sharing,
                       "spec": { "version": 1, "range": "1h", "panels": panels } });
    let (status, body) = send(app, cookie, "POST", "/api/dashboards", Some(body)).await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    body["id"].as_str().unwrap().to_owned()
}

fn link_request(dashboards: &[&str], expires: &str) -> Value {
    json!({ "name": "Reception TV", "dashboard_ids": dashboards, "seconds_per_page": 30, "expires": expires })
}

/// The secret of a link's URL, after `#`.
fn secret(created: &Value) -> String {
    let url = created["url"].as_str().unwrap();
    let (page, secret) = url.split_once('#').unwrap();
    assert!(page.ends_with("/display/"), "{url}");
    secret.to_owned()
}

async fn start(app: &Router, token: &str, source: Option<&str>) -> Answer {
    call(
        app,
        "",
        "POST",
        "/api/display/start",
        Some(json!({ "token": token })),
        source,
    )
    .await
}

async fn screen_data(app: &Router, screen: &str, dashboard: &str) -> (StatusCode, Value) {
    send(
        app,
        screen,
        "POST",
        "/api/display/data",
        Some(json!({ "dashboard_id": dashboard })),
    )
    .await
}

db_tests! {
    #[sqlx::test]
    async fn a_screen_shows_its_dashboards_and_nothing_else(pool: PgPool) {
        let w = world(&pool).await;
        let admin_a = login(&w.app, "admin_a").await;
        let customer_a = json!({ "kind": "customer", "id": w.customer_a });
        // Edited by hand to name the other customer's servers too.
        let board = dashboard(&w.app, &admin_a, "NOC", customer_a.clone(),
            json!([figure("avg", json!({ "kind": "all" })), top("top")])).await;
        let other = dashboard(&w.app, &admin_a, "Other", customer_a.clone(), json!([top("top")])).await;

        let (status, created) = send(&w.app, &admin_a, "POST", "/api/display-links",
            Some(link_request(&[&board], "1y"))).await;
        assert_eq!(status, StatusCode::CREATED, "{created}");
        assert_eq!(created["dashboards"], json!([{ "id": board, "name": "NOC" }]));
        assert_eq!(created["created_by_username"], "admin_a");
        assert!(created["expires_at"].is_string());
        let token = secret(&created);
        // Only the hash is stored.
        let stored: Vec<u8> = sqlx::query_scalar("SELECT token_hash FROM display_links")
            .fetch_one(&w.pool).await.unwrap();
        assert_eq!(stored.len(), 32);
        assert_ne!(stored, token.as_bytes());

        let started = start(&w.app, &token, None).await;
        assert_eq!(started.status, StatusCode::OK, "{}", started.body);
        assert_eq!(started.body["name"], "Reception TV");
        assert_eq!(started.body["refresh_seconds"], 30);
        assert_eq!(started.body["dashboards"].as_array().unwrap().len(), 1);
        assert_eq!(started.body["dashboards"][0]["spec"]["panels"][1]["id"], "top");
        let screen = started.cookie.unwrap();
        assert!(screen.starts_with("__Host-mf_display="), "{screen}");

        let (status, data) = screen_data(&w.app, &screen, &board).await;
        assert_eq!(status, StatusCode::OK, "{data}");
        assert!(data["now"].as_i64().unwrap() > 0);
        // The other customer's server is dropped, and never asked for.
        let rows = data["panels"][1]["rows"].as_array().unwrap();
        assert_eq!(rows.len(), 1, "{data}");
        assert_eq!(rows[0]["hostname"], "host-agent-a1");
        for query in w.vm.seen() {
            assert!(query.contains("extra_filters[]={site=~\"a1\"}"), "{query}");
            assert!(!query.contains("b1"), "{query}");
        }

        // Another dashboard, even of the same customer, is not the link's.
        assert_eq!(screen_data(&w.app, &screen, &other).await.0, StatusCode::NOT_FOUND);
        // The screen session opens nothing else.
        assert_eq!(send(&w.app, &screen, "GET", "/api/dashboards", None).await.0, StatusCode::UNAUTHORIZED);
        assert_eq!(send(&w.app, &screen, "GET", "/api/servers", None).await.0, StatusCode::UNAUTHORIZED);
        let panels = json!({ "range": "1h", "panels": [top("x")] });
        assert_eq!(send(&w.app, &screen, "POST", "/api/dashboard-data", Some(panels)).await.0, StatusCode::UNAUTHORIZED);

        let (status, view) = send(&w.app, &screen, "GET", "/api/display", None).await;
        assert_eq!((status, view["dashboards"][0]["name"].clone()), (StatusCode::OK, json!("NOC")));

        // The dashboard page lists it, last use included, without secret.
        let (status, list) = send(&w.app, &admin_a, "GET", &format!("/api/dashboards/{board}/display-links"), None).await;
        assert_eq!(status, StatusCode::OK, "{list}");
        assert!(list["links"][0]["last_used_at"].is_string(), "{list}");
        assert!(!list.to_string().contains(&token));
        assert!(list["links"][0].get("url").is_none());
    }

    #[sqlx::test]
    async fn only_administrators_of_the_place_make_links(pool: PgPool) {
        let w = world(&pool).await;
        let [admin_a, admin_b, viewer_a, operator1, site_admin1, global] = [
            login(&w.app, "admin_a").await,
            login(&w.app, "admin_b").await,
            login(&w.app, "viewer_a").await,
            login(&w.app, "operator1").await,
            login(&w.app, "site_admin1").await,
            login(&w.app, "global").await,
        ];
        let customer_a = json!({ "kind": "customer", "id": w.customer_a });
        let site_a1 = json!({ "kind": "site", "id": w.site_a1 });
        let board_a = dashboard(&w.app, &admin_a, "A", customer_a, json!([top("t")])).await;
        let board_b = dashboard(&w.app, &admin_b, "B",
            json!({ "kind": "customer", "id": w.customer_b }), json!([top("t")])).await;
        let site_board = dashboard(&w.app, &operator1, "Site", site_a1, json!([top("t")])).await;
        let viewers_own = dashboard(&w.app, &viewer_a, "Mine", json!({ "kind": "personal" }), json!([top("t")])).await;
        let globals_own = dashboard(&w.app, &global, "Everything", json!({ "kind": "personal" }), json!([top("t")])).await;

        let create = |cookie: &String, ids: &[&str]| {
            let (app, cookie, body) = (w.app.clone(), cookie.clone(), link_request(ids, "30d"));
            async move { send(&app, &cookie, "POST", "/api/display-links", Some(body)).await.0 }
        };
        // A customer's administrator, for its own dashboards only.
        assert_eq!(create(&admin_a, &[&board_a, &site_board]).await, StatusCode::CREATED);
        assert_eq!(create(&admin_a, &[&board_a, &board_b]).await, StatusCode::NOT_FOUND);
        assert_eq!(create(&admin_b, &[&board_a]).await, StatusCode::NOT_FOUND);
        assert_eq!(create(&admin_a, &[&globals_own]).await, StatusCode::NOT_FOUND);
        // Seeing or editing is not enough.
        assert_eq!(create(&viewer_a, &[&board_a]).await, StatusCode::FORBIDDEN);
        assert_eq!(create(&viewer_a, &[&viewers_own]).await, StatusCode::FORBIDDEN);
        assert_eq!(create(&operator1, &[&site_board]).await, StatusCode::FORBIDDEN);
        // A site's administrator links its site's dashboards, not its customer's.
        assert_eq!(create(&site_admin1, &[&site_board]).await, StatusCode::CREATED);
        assert_eq!(create(&site_admin1, &[&board_a]).await, StatusCode::NOT_FOUND);
        assert_eq!(create(&global, &[&globals_own, &board_b]).await, StatusCode::CREATED);

        let (_, summary) = send(&w.app, &viewer_a, "GET", &format!("/api/dashboards/{board_a}"), None).await;
        assert_eq!(summary["can_link"], false);
        let (_, summary) = send(&w.app, &admin_a, "GET", &format!("/api/dashboards/{board_a}"), None).await;
        assert_eq!(summary["can_link"], true);
        let uri = format!("/api/dashboards/{board_a}/display-links");
        assert_eq!(send(&w.app, &viewer_a, "GET", &uri, None).await.0, StatusCode::FORBIDDEN);
        assert_eq!(send(&w.app, &admin_b, "GET", &uri, None).await.0, StatusCode::NOT_FOUND);

        for (body, words) in [
            (json!({ "name": " ", "dashboard_ids": [board_a], "seconds_per_page": 30, "expires": "1y" }), "where the screen is"),
            (json!({ "name": "TV", "dashboard_ids": [], "seconds_per_page": 30, "expires": "1y" }), "1 to 12"),
            (json!({ "name": "TV", "dashboard_ids": [board_a, board_a], "seconds_per_page": 30, "expires": "1y" }), "once"),
            (json!({ "name": "TV", "dashboard_ids": [board_a], "seconds_per_page": 5, "expires": "1y" }), "10 to 3600"),
            (json!({ "name": "TV", "dashboard_ids": [board_a], "seconds_per_page": 30, "expires": "2y" }), "expires"),
        ] {
            let (status, body) = send(&w.app, &admin_a, "POST", "/api/display-links", Some(body)).await;
            assert_eq!(status, StatusCode::BAD_REQUEST);
            assert!(body["error"].as_str().unwrap().contains(words), "{body}");
        }
    }

    #[sqlx::test]
    async fn turning_a_link_off_stops_its_screens_and_is_audited(pool: PgPool) {
        let w = world(&pool).await;
        let [admin_a, admin_b, viewer_a] = [
            login(&w.app, "admin_a").await,
            login(&w.app, "admin_b").await,
            login(&w.app, "viewer_a").await,
        ];
        let board = dashboard(&w.app, &admin_a, "NOC",
            json!({ "kind": "customer", "id": w.customer_a }), json!([top("t")])).await;
        let (_, created) = send(&w.app, &admin_a, "POST", "/api/display-links",
            Some(link_request(&[&board], "never"))).await;
        assert_eq!(created["expires_at"], Value::Null);
        let id = created["id"].as_str().unwrap();
        let token = secret(&created);
        let screen = start(&w.app, &token, None).await.cookie.unwrap();
        assert_eq!(screen_data(&w.app, &screen, &board).await.0, StatusCode::OK);

        let uri = format!("/api/display-links/{id}");
        assert_eq!(send(&w.app, &admin_b, "DELETE", &uri, None).await.0, StatusCode::NOT_FOUND);
        assert_eq!(send(&w.app, &viewer_a, "DELETE", &uri, None).await.0, StatusCode::FORBIDDEN);
        assert_eq!(send(&w.app, &admin_a, "DELETE", &uri, None).await.0, StatusCode::NO_CONTENT);
        assert_eq!(send(&w.app, &admin_a, "DELETE", &uri, None).await.0, StatusCode::NOT_FOUND);

        assert_eq!(screen_data(&w.app, &screen, &board).await.0, StatusCode::UNAUTHORIZED);
        let again = start(&w.app, &token, None).await;
        assert_eq!(again.status, StatusCode::GONE);
        assert!(again.body["error"].as_str().unwrap().contains("turned off or has expired"));

        let rows: Vec<(String, String, Value)> = sqlx::query_as(
            "SELECT action, target_id, details FROM audit_log WHERE target_kind = 'display_link' ORDER BY id",
        )
        .fetch_all(&w.pool)
        .await
        .unwrap();
        let actions: Vec<&str> = rows.iter().map(|r| r.0.as_str()).collect();
        assert_eq!(actions, ["display_link.create", "display_link.revoke"]);
        assert_eq!(rows[0].1, id);
        assert_eq!(rows[0].2["dashboards"], json!([board]));
        assert!(!rows.iter().any(|r| r.2.to_string().contains(&token)));
    }

    #[sqlx::test]
    async fn expired_links_and_deleted_dashboards_stop(pool: PgPool) {
        let w = world(&pool).await;
        let admin_a = login(&w.app, "admin_a").await;
        let customer_a = json!({ "kind": "customer", "id": w.customer_a });
        let one = dashboard(&w.app, &admin_a, "One", customer_a.clone(), json!([top("t")])).await;
        let two = dashboard(&w.app, &admin_a, "Two", customer_a, json!([top("t")])).await;
        let (_, created) = send(&w.app, &admin_a, "POST", "/api/display-links",
            Some(link_request(&[&one, &two], "30d"))).await;
        let token = secret(&created);
        let screen = start(&w.app, &token, None).await.cookie.unwrap();

        // Deleting a dashboard takes it out of the turn.
        assert_eq!(send(&w.app, &admin_a, "DELETE", &format!("/api/dashboards/{one}"), None).await.0, StatusCode::NO_CONTENT);
        let (_, view) = send(&w.app, &screen, "GET", "/api/display", None).await;
        assert_eq!(view["dashboards"].as_array().unwrap().len(), 1);
        assert_eq!(view["dashboards"][0]["id"], json!(two));

        sqlx::query("UPDATE display_links SET expires_at = now() - interval '1 second'")
            .execute(&w.pool).await.unwrap();
        assert_eq!(screen_data(&w.app, &screen, &two).await.0, StatusCode::UNAUTHORIZED);
        assert_eq!(start(&w.app, &token, None).await.status, StatusCode::GONE);
    }

    #[sqlx::test]
    async fn a_link_stops_when_its_maker_loses_access(pool: PgPool) {
        let w = world(&pool).await;
        let admin_a = login(&w.app, "admin_a").await;
        let board = dashboard(&w.app, &admin_a, "NOC",
            json!({ "kind": "customer", "id": w.customer_a }), json!([top("t")])).await;
        let (_, created) = send(&w.app, &admin_a, "POST", "/api/display-links",
            Some(link_request(&[&board], "1y"))).await;
        let token = secret(&created);
        let screen = start(&w.app, &token, None).await.cookie.unwrap();
        let maker: Uuid = sqlx::query_scalar("SELECT id FROM users WHERE username = 'admin_a'")
            .fetch_one(&w.pool).await.unwrap();

        // No longer an administrator: nothing to show, until it is again.
        sqlx::query("UPDATE user_roles SET role = 'viewer' WHERE user_id = $1")
            .bind(maker).execute(&w.pool).await.unwrap();
        assert_eq!(screen_data(&w.app, &screen, &board).await.0, StatusCode::FORBIDDEN);
        assert_eq!(start(&w.app, &token, None).await.status, StatusCode::FORBIDDEN);
        sqlx::query("UPDATE user_roles SET role = 'administrator' WHERE user_id = $1")
            .bind(maker).execute(&w.pool).await.unwrap();
        assert_eq!(screen_data(&w.app, &screen, &board).await.0, StatusCode::OK);

        // Moved to the other customer: it never shows that customer instead.
        sqlx::query("UPDATE user_roles SET customer_id = $2 WHERE user_id = $1")
            .bind(maker).bind(w.customer_b).execute(&w.pool).await.unwrap();
        assert_eq!(screen_data(&w.app, &screen, &board).await.0, StatusCode::FORBIDDEN);
        sqlx::query("UPDATE user_roles SET customer_id = $2 WHERE user_id = $1")
            .bind(maker).bind(w.customer_a).execute(&w.pool).await.unwrap();

        // Disabled: the link is dead.
        sqlx::query("UPDATE users SET disabled_at = now() WHERE id = $1")
            .bind(maker).execute(&w.pool).await.unwrap();
        assert_eq!(screen_data(&w.app, &screen, &board).await.0, StatusCode::UNAUTHORIZED);
        assert_eq!(start(&w.app, &token, None).await.status, StatusCode::GONE);

        // Deleted: the link goes with them.
        sqlx::query("DELETE FROM users WHERE id = $1").bind(maker).execute(&w.pool).await.unwrap();
        let left: i64 = sqlx::query_scalar("SELECT count(*) FROM display_links").fetch_one(&w.pool).await.unwrap();
        assert_eq!(left, 0);
    }

    #[sqlx::test]
    async fn a_personal_dashboard_shows_only_what_its_maker_administers(pool: PgPool) {
        let w = world(&pool).await;
        // A technician: administrator of site a1, viewer of everything.
        let technician: Uuid = sqlx::query_scalar("SELECT id FROM users WHERE username = 'site_admin1'")
            .fetch_one(&w.pool).await.unwrap();
        users::grant(&w.pool, technician, Role::Viewer, GrantScope::Global, "test").await.unwrap();
        let site_admin1 = login(&w.app, "site_admin1").await;
        let mine = json!({ "kind": "personal" });
        let everything = dashboard(&w.app, &site_admin1, "Everything", mine.clone(), json!([top("t")])).await;
        let other = dashboard(&w.app, &site_admin1, "Customer B", mine.clone(),
            json!([figure("f", json!({ "kind": "customer", "id": w.customer_b }))])).await;
        let own_site = dashboard(&w.app, &site_admin1, "a1", mine.clone(),
            json!([figure("f", json!({ "kind": "site", "id": w.site_a1 }))])).await;

        // Refused up front, rather than a screen silently showing less.
        for board in [&everything, &other] {
            let (status, body) = send(&w.app, &site_admin1, "POST", "/api/display-links",
                Some(link_request(&[board], "never"))).await;
            assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
            assert!(body["error"].as_str().unwrap().contains("only what you administer"), "{body}");
        }
        let (status, created) = send(&w.app, &site_admin1, "POST", "/api/display-links",
            Some(link_request(&[&own_site], "never"))).await;
        assert_eq!(status, StatusCode::CREATED, "{created}");
        let screen = start(&w.app, &secret(&created), None).await.cookie.unwrap();

        // Edited afterwards to name everything: the screen still reads only
        // the administered site, though its maker reads both customers.
        sqlx::query("UPDATE dashboards SET spec = (SELECT spec FROM dashboards WHERE id = $2) WHERE id = $1")
            .bind(Uuid::parse_str(&own_site).unwrap())
            .bind(Uuid::parse_str(&everything).unwrap())
            .execute(&w.pool).await.unwrap();
        let (status, data) = screen_data(&w.app, &screen, &own_site).await;
        assert_eq!(status, StatusCode::OK, "{data}");
        let rows = data["panels"][0]["rows"].as_array().unwrap();
        assert_eq!(rows.len(), 1, "{data}");
        assert_eq!(rows[0]["hostname"], "host-agent-a1");
        for query in w.vm.seen() {
            assert!(!query.contains("b1"), "{query}");
        }
        // The dashboard's own page still shows the maker all they read.
        let page = json!({ "range": "1h", "within": { "kind": "all" }, "panels": [top("t")] });
        let (status, data) = send(&w.app, &site_admin1, "POST", "/api/dashboard-data", Some(page)).await;
        assert_eq!(status, StatusCode::OK, "{data}");
        assert_eq!(data["panels"][0]["rows"].as_array().unwrap().len(), 2, "{data}");

        // A global administrator's own dashboards show everything.
        let global = login(&w.app, "global").await;
        let board = dashboard(&w.app, &global, "All", mine, json!([top("t"),
            figure("f", json!({ "kind": "customer", "id": w.customer_b }))])).await;
        let (status, created) = send(&w.app, &global, "POST", "/api/display-links",
            Some(link_request(&[&board], "1y"))).await;
        assert_eq!(status, StatusCode::CREATED, "{created}");
        let screen = start(&w.app, &secret(&created), None).await.cookie.unwrap();
        let (status, data) = screen_data(&w.app, &screen, &board).await;
        assert_eq!(status, StatusCode::OK, "{data}");
        assert_eq!(data["panels"][0]["rows"].as_array().unwrap().len(), 2, "{data}");
    }

    #[sqlx::test]
    async fn screens_are_rate_limited(pool: PgPool) {
        let w = world(&pool).await;
        let admin_a = login(&w.app, "admin_a").await;
        let board = dashboard(&w.app, &admin_a, "NOC",
            json!({ "kind": "customer", "id": w.customer_a }), json!([top("t")])).await;
        let (_, created) = send(&w.app, &admin_a, "POST", "/api/display-links",
            Some(link_request(&[&board], "1y"))).await;
        let token = secret(&created);

        // Links that do not work, per source address.
        let bad = "0".repeat(64);
        for _ in 0..10 {
            assert_eq!(start(&w.app, &bad, Some("192.0.2.1")).await.status, StatusCode::GONE);
        }
        assert_eq!(start(&w.app, &bad, Some("192.0.2.1")).await.status, StatusCode::TOO_MANY_REQUESTS);
        assert_eq!(start(&w.app, &token, Some("192.0.2.1")).await.status, StatusCode::TOO_MANY_REQUESTS);
        assert_eq!(start(&w.app, &bad, Some("192.0.2.2")).await.status, StatusCode::GONE);

        // Openings per link.
        let mut screen = String::new();
        for _ in 0..STARTS_PER_HOUR {
            let started = start(&w.app, &token, Some("192.0.2.3")).await;
            assert_eq!(started.status, StatusCode::OK);
            screen = started.cookie.unwrap();
        }
        let over = start(&w.app, &token, Some("192.0.2.4")).await;
        assert_eq!(over.status, StatusCode::TOO_MANY_REQUESTS);
        assert!(over.body["error"].as_str().unwrap().contains("tries again by itself"));

        // Requests per screen.
        for _ in 0..REQUESTS_PER_MINUTE {
            assert_eq!(screen_data(&w.app, &screen, &board).await.0, StatusCode::OK);
        }
        assert_eq!(screen_data(&w.app, &screen, &board).await.0, StatusCode::TOO_MANY_REQUESTS);
    }
}
