//! Network maps through the API against a real PostgreSQL (ADR-0185):
//! making and saving them, what they show now from relays, dependencies
//! and neighbours, who sees and changes them, backgrounds, and a map as a
//! dashboard panel. Ignored without `DATABASE_URL`.

use axum::body::Body;
use axum::http::{Request, StatusCode};
use axum::Router;
use base64::Engine;
use monitorflow_common::redact::Secret;
use monitorflow_server::api::auth::CSRF_HEADER;
use monitorflow_server::api::users::{self, GrantScope};
use monitorflow_server::api::{v1, ApiSettings, ApiState, Role};
use monitorflow_server::grpc::Registry;
use serde_json::{json, Value};
use sqlx::PgPool;
use tower::ServiceExt;
use uuid::Uuid;

const PASSWORD: &str = "correct horse battery";
const PNG: &[u8] = b"\x89PNG\r\n\x1a\n\0\0\0\rIHDR\0\0\0\x01\0\0\0\x01\x08\x06\0\0\0";

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

fn app(pool: &PgPool) -> Router {
    v1::app(ApiState::new(
        pool.clone(),
        Registry::default(),
        ApiSettings::default(),
    ))
}

struct Answer {
    status: StatusCode,
    body: Value,
    bytes: Vec<u8>,
    content_type: Option<String>,
    cookie: Option<String>,
}

async fn send(pool: &PgPool, request: Request<Body>) -> Answer {
    let response = app(pool).oneshot(request).await.unwrap();
    let status = response.status();
    let cookie = response
        .headers()
        .get("set-cookie")
        .map(|v| v.to_str().unwrap().split(';').next().unwrap().to_owned());
    let content_type = response
        .headers()
        .get("content-type")
        .map(|v| v.to_str().unwrap().to_owned());
    let bytes = axum::body::to_bytes(response.into_body(), 4 << 20)
        .await
        .unwrap()
        .to_vec();
    let body = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
    Answer {
        status,
        body,
        bytes,
        content_type,
        cookie,
    }
}

async fn session(pool: &PgPool, username: &str, role: Role, scope: GrantScope) -> String {
    users::create_user_with(pool, username, &Secret::new(PASSWORD), role, scope, "test")
        .await
        .unwrap();
    let body = json!({ "username": username, "password": PASSWORD }).to_string();
    let request = Request::post("/api/auth/login")
        .header("content-type", "application/json")
        .header(CSRF_HEADER, "1")
        .body(Body::from(body))
        .unwrap();
    let answer = send(pool, request).await;
    assert_eq!(answer.status, StatusCode::OK);
    answer.cookie.unwrap()
}

async fn call(
    pool: &PgPool,
    cookie: &str,
    method: &str,
    uri: &str,
    body: Option<Value>,
) -> (StatusCode, Value) {
    let answer = raw(pool, cookie, method, uri, body).await;
    (answer.status, answer.body)
}

async fn raw(pool: &PgPool, cookie: &str, method: &str, uri: &str, body: Option<Value>) -> Answer {
    let request = Request::builder()
        .method(method)
        .uri(uri)
        .header("cookie", cookie)
        .header(CSRF_HEADER, "1")
        .header("content-type", "application/json")
        .body(body.map_or_else(Body::empty, |b| Body::from(b.to_string())))
        .unwrap();
    send(pool, request).await
}

async fn one(pool: &PgPool, sql: &'static str, binds: &[String]) -> Uuid {
    let mut query = sqlx::query_scalar(sql);
    for bind in binds {
        query = query.bind(bind.clone());
    }
    query.fetch_one(pool).await.unwrap()
}

struct Fleet {
    customer: Uuid,
    site: Uuid,
    other_site: Uuid,
    relay: Uuid,
    web: Uuid,
    switch: Uuid,
    firewall: Uuid,
    db: Uuid,
}

/// paris: a relay gw-1, web-1 behind it, sw-core (a switch that sees
/// web-1 and an unmonitored access point over LLDP) and fw-1, a firewall
/// web-1 depends on. lyon, of the same customer: db-1.
async fn fleet(pool: &PgPool) -> Fleet {
    let site = one(
        pool,
        "INSERT INTO sites (name) VALUES ('paris') RETURNING id",
        &[],
    )
    .await;
    let customer: Uuid = sqlx::query_scalar("SELECT customer_id FROM sites WHERE id = $1")
        .bind(site)
        .fetch_one(pool)
        .await
        .unwrap();
    let other_site = one(
        pool,
        "INSERT INTO sites (name, customer_id) VALUES ('lyon', $1::uuid) RETURNING id",
        &[customer.to_string()],
    )
    .await;
    let relay = one(
        pool,
        "INSERT INTO relays (peer_id, site_id, name) VALUES ('gw-1', $1::uuid, 'gw-1') RETURNING id",
        &[site.to_string()],
    )
    .await;
    let web = one(
        pool,
        "INSERT INTO servers (peer_id, site_id, hostname, os_family, relay_id, last_seen_at)
         VALUES ('web-1', $1::uuid, 'web-1', 'debian', $2::uuid, now()) RETURNING id",
        &[site.to_string(), relay.to_string()],
    )
    .await;
    let db = one(
        pool,
        "INSERT INTO servers (peer_id, site_id, hostname, os_family, last_seen_at)
         VALUES ('db-1', $1::uuid, 'db-1', 'debian', now()) RETURNING id",
        &[other_site.to_string()],
    )
    .await;
    let device = |name: &'static str, template: &'static str, address: &'static str| async move {
        one(
            pool,
            "INSERT INTO servers (peer_id, site_id, hostname, os_family, kind, address,
                                  package_updates, device_template)
             VALUES ($2, $1::uuid, $2, 'device', 'device', $4, false, $3) RETURNING id",
            &[
                site.to_string(),
                name.to_owned(),
                template.to_owned(),
                address.to_owned(),
            ],
        )
        .await
    };
    let switch = device("sw-core", "switch", "192.0.2.2").await;
    let firewall = device("fw-1", "firewall", "192.0.2.1").await;
    sqlx::query("INSERT INTO dependencies (server_id, parent_server_id) VALUES ($1, $2)")
        .bind(web)
        .bind(firewall)
        .execute(pool)
        .await
        .unwrap();
    for (port, name, remote_port) in [
        ("Gi0/1", "WEB-1.example.net", "eth0"),
        ("Gi0/7", "ap-hall(FOC1234X0AB)", "Gi0"),
    ] {
        sqlx::query(
            "INSERT INTO network_neighbors (server_id, protocol, local_port, remote_name, remote_port)
             VALUES ($1, 'lldp', $2, $3, $4)",
        )
        .bind(switch)
        .bind(port)
        .bind(name)
        .bind(remote_port)
        .execute(pool)
        .await
        .unwrap();
    }
    sqlx::query("INSERT INTO network_neighbor_reads (server_id) VALUES ($1)")
        .bind(switch)
        .execute(pool)
        .await
        .unwrap();
    Fleet {
        customer,
        site,
        other_site,
        relay,
        web,
        switch,
        firewall,
        db,
    }
}

fn node<'a>(live: &'a Value, key: &str) -> &'a Value {
    live["nodes"]
        .as_array()
        .unwrap()
        .iter()
        .find(|n| n["key"] == key)
        .unwrap_or_else(|| panic!("no node {key} in {live}"))
}

fn link<'a>(live: &'a Value, a: &str, b: &str) -> &'a Value {
    live["links"]
        .as_array()
        .unwrap()
        .iter()
        .find(|l| (l["from"] == a && l["to"] == b) || (l["from"] == b && l["to"] == a))
        .unwrap_or_else(|| panic!("no link {a} {b} in {live}"))
}

fn audited(actions: &[Value]) -> Vec<String> {
    actions
        .iter()
        .map(|a| a.as_str().unwrap().to_owned())
        .collect()
}

db_tests! {
    #[sqlx::test]
    async fn a_map_draws_relays_dependencies_and_neighbours(pool: PgPool) {
        let f = fleet(&pool).await;
        let admin = session(&pool, "alice", Role::Administrator, GrantScope::Global).await;
        let body = json!({"name": "Paris network", "place": {"kind": "site", "id": f.site}});
        let (status, map) = call(&pool, &admin, "POST", "/api/maps", Some(body)).await;
        assert_eq!(status, StatusCode::CREATED, "{map}");
        assert_eq!(map["place"]["name"], "paris");
        assert_eq!(map["document"]["sources"]["relays"], true);
        assert_eq!(map["can_edit"], true);
        let id = map["id"].as_str().unwrap().to_owned();

        let (status, live) = call(&pool, &admin, "GET", &format!("/api/maps/{id}/live"), None).await;
        assert_eq!(status, StatusCode::OK, "{live}");
        let (web, switch, firewall, relay) = (
            format!("server:{}", f.web),
            format!("server:{}", f.switch),
            format!("server:{}", f.firewall),
            format!("relay:{}", f.relay),
        );
        // Every node of the site, the central server above its relay, and
        // the access point the switch sees; nothing of lyon.
        assert_eq!(live["nodes"].as_array().unwrap().len(), 6, "{live}");
        assert_eq!(node(&live, "central")["kind"], "central");
        assert_eq!(node(&live, &relay)["state"], "offline");
        assert_eq!(node(&live, &switch)["template"], "switch");
        assert_eq!(node(&live, &switch)["kind"], "device");
        let ap = node(&live, "neighbor:ap-hall");
        assert_eq!(ap["state"], "unknown");
        assert_eq!(ap["name"], "ap-hall(FOC1234X0AB)");
        assert!(!live.to_string().contains(&f.db.to_string()));
        assert!(!live["neighbors_read_at"].is_null());
        // Links: the relay's, the dependency, the neighbours.
        assert_eq!(link(&live, "central", &relay)["source"], "relay");
        assert_eq!(link(&live, "central", &relay)["state"], "critical", "the relay is offline");
        assert_eq!(link(&live, &relay, &web)["source"], "relay");
        let dependency = link(&live, &firewall, &web);
        assert_eq!(dependency["source"], "dependency");
        assert_eq!(dependency["from"], firewall, "from the parent");
        let lldp = link(&live, &switch, &web);
        assert_eq!(lldp["source"], "neighbor");
        assert_eq!(lldp["from_port"], "Gi0/1");
        assert_eq!(lldp["to_port"], "eth0");
        assert_eq!(lldp["interface"]["instance"], "Gi0/1");
        assert_eq!(link(&live, &switch, "neighbor:ap-hall")["state"], "ok");
        assert_eq!(live["links"].as_array().unwrap().len(), 5, "{live}");

        // Sources can be turned off.
        let document = json!({"version": 1, "sources": {"relays": false, "dependencies": false,
                                                        "neighbors": false}});
        let body = json!({"name": "Paris network", "place": {"kind": "site", "id": f.site},
                          "document": document, "revision": map["revision"]});
        let (status, saved) = call(&pool, &admin, "PUT", &format!("/api/maps/{id}"), Some(body)).await;
        assert_eq!(status, StatusCode::OK, "{saved}");
        let (_, live) = call(&pool, &admin, "GET", &format!("/api/maps/{id}/live"), None).await;
        assert_eq!(live["links"], json!([]), "{live}");
        assert_eq!(live["nodes"].as_array().unwrap().len(), 4, "{live}");
    }

    #[sqlx::test]
    async fn people_place_nodes_and_draw_on_a_map(pool: PgPool) {
        let f = fleet(&pool).await;
        let admin = session(&pool, "alice", Role::Administrator, GrantScope::Global).await;
        let web = format!("server:{}", f.web);
        let switch = format!("server:{}", f.switch);
        let document = json!({"version": 1,
            "nodes": [{"key": web, "x": 100.0, "y": 200.0},
                      {"key": "neighbor:ap-hall", "x": 0.0, "y": 0.0, "hidden": true}],
            "zones": [{"id": "rack", "label": "Rack A", "x": 0, "y": 0, "width": 300, "height": 400}],
            "links": [{"id": "wan", "from": switch, "to": web, "label": "Patch 12",
                       "interface": {"server_id": f.switch, "instance": "Gi0/2"}}],
            "labels": [{"id": "t", "text": "Ground floor", "x": 10, "y": 10}]});
        let body = json!({"name": "Rack", "place": {"kind": "site", "id": f.site}, "document": document});
        let (status, map) = call(&pool, &admin, "POST", "/api/maps", Some(body.clone())).await;
        assert_eq!(status, StatusCode::CREATED, "{map}");
        let id = map["id"].as_str().unwrap().to_owned();
        let (_, live) = call(&pool, &admin, "GET", &format!("/api/maps/{id}/live"), None).await;
        assert_eq!(node(&live, &web)["x"], 100.0);
        assert_eq!(node(&live, &web)["y"], 200.0);
        assert!(node(&live, &switch)["x"].is_null(), "laid out by the client");
        assert_eq!(node(&live, "neighbor:ap-hall")["hidden"], true);
        assert_eq!(live["zones"][0]["label"], "Rack A");
        assert_eq!(live["labels"][0]["text"], "Ground floor");
        // A link drawn by hand shows beside the neighbour one.
        let drawn: Vec<&Value> = live["links"].as_array().unwrap().iter()
            .filter(|l| l["source"] == "manual").collect();
        assert_eq!(drawn.len(), 1, "{live}");
        assert_eq!(drawn[0]["id"], "wan");
        assert_eq!(drawn[0]["label"], "Patch 12");

        // Saving from an old revision is refused.
        let mut stale = body.clone();
        stale["revision"] = json!(0);
        let (status, error) = call(&pool, &admin, "PUT", &format!("/api/maps/{id}"), Some(stale)).await;
        assert_eq!(status, StatusCode::CONFLICT, "{error}");
        // A link showing an interface of another site's server is refused.
        let mut outside = body.clone();
        outside["revision"] = map["revision"].clone();
        outside["document"]["links"][0]["interface"]["server_id"] = json!(f.db);
        outside["document"]["links"][0]["to"] = json!(format!("server:{}", f.db));
        let (status, error) = call(&pool, &admin, "PUT", &format!("/api/maps/{id}"), Some(outside)).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{error}");
        // A document that does not hold is refused with a sentence.
        let mut bad = body;
        bad["document"]["zones"][0]["color"] = json!(12);
        let (status, error) = call(&pool, &admin, "POST", "/api/maps", Some(bad)).await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert_eq!(error["error"], "A zone's colour is 1 to 8.");

        let (status, _) = call(&pool, &admin, "DELETE", &format!("/api/maps/{id}"), None).await;
        assert_eq!(status, StatusCode::NO_CONTENT);
        let actions: Vec<Value> = sqlx::query_scalar(
            "SELECT to_jsonb(action) FROM audit_log WHERE action LIKE 'network_map.%' ORDER BY id",
        )
        .fetch_all(&pool)
        .await
        .unwrap();
        assert_eq!(audited(&actions), ["network_map.create", "network_map.delete"]);
    }

    #[sqlx::test]
    async fn maps_follow_the_roles_of_their_place(pool: PgPool) {
        let f = fleet(&pool).await;
        let admin = session(&pool, "alice", Role::Administrator, GrantScope::Global).await;
        let viewer = session(&pool, "victor", Role::Viewer, GrantScope::Site(f.site)).await;
        let lyon = session(&pool, "lea", Role::Operator, GrantScope::Site(f.other_site)).await;
        let whole = session(&pool, "carl", Role::Viewer, GrantScope::Customer(f.customer)).await;
        let site_map = json!({"name": "Paris", "place": {"kind": "site", "id": f.site}});
        let (_, map) = call(&pool, &admin, "POST", "/api/maps", Some(site_map.clone())).await;
        let id = map["id"].as_str().unwrap().to_owned();
        let customer_map = json!({"name": "Everything", "place": {"kind": "customer", "id": f.customer}});
        let (status, whole_map) = call(&pool, &admin, "POST", "/api/maps", Some(customer_map.clone())).await;
        assert_eq!(status, StatusCode::CREATED, "{whole_map}");
        let whole_id = whole_map["id"].as_str().unwrap().to_owned();

        // A viewer of paris reads its map, but changes nothing.
        let (status, list) = call(&pool, &viewer, "GET", "/api/maps", None).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(list["maps"].as_array().unwrap().len(), 1, "{list}");
        assert_eq!(list["maps"][0]["can_edit"], false);
        let (status, _) = call(&pool, &viewer, "GET", &format!("/api/maps/{id}/live"), None).await;
        assert_eq!(status, StatusCode::OK);
        let (status, _) = call(&pool, &viewer, "POST", "/api/maps", Some(site_map)).await;
        assert_eq!(status, StatusCode::FORBIDDEN);
        let (status, _) = call(&pool, &viewer, "DELETE", &format!("/api/maps/{id}"), None).await;
        assert_eq!(status, StatusCode::FORBIDDEN);
        let (status, _) = call(&pool, &viewer, "POST", &format!("/api/maps/{id}/neighbors"), None).await;
        assert_eq!(status, StatusCode::FORBIDDEN);
        // The customer's map is not theirs to see; lyon's operator sees
        // neither paris's map nor, through the customer's, paris.
        let (status, _) = call(&pool, &viewer, "GET", &format!("/api/maps/{whole_id}"), None).await;
        assert_eq!(status, StatusCode::NOT_FOUND);
        let (status, _) = call(&pool, &lyon, "GET", &format!("/api/maps/{id}/live"), None).await;
        assert_eq!(status, StatusCode::NOT_FOUND);
        let (status, _) = call(&pool, &lyon, "POST", "/api/maps", Some(customer_map)).await;
        assert_eq!(status, StatusCode::NOT_FOUND);
        // A viewer of the whole customer sees both sites on its map.
        let (status, live) = call(&pool, &whole, "GET", &format!("/api/maps/{whole_id}/live"), None).await;
        assert_eq!(status, StatusCode::OK, "{live}");
        assert_eq!(live["sites"].as_array().unwrap().len(), 2, "{live}");
        assert!(live.to_string().contains(&f.db.to_string()));
        let (_, list) = call(&pool, &whole, "GET", &format!("/api/maps?site_id={}", f.other_site), None).await;
        assert_eq!(list["maps"], json!([]), "{list}");
        let (_, list) = call(&pool, &whole, "GET", &format!("/api/maps?customer_id={}", f.customer), None).await;
        assert_eq!(list["maps"].as_array().unwrap().len(), 2, "{list}");
        // No SNMP credentials anywhere: nothing to ask.
        let (status, read) = call(&pool, &admin, "POST", &format!("/api/maps/{id}/neighbors"), None).await;
        assert_eq!(status, StatusCode::OK, "{read}");
        assert_eq!(read, json!({"devices": 0, "neighbors": 0, "failed": []}));
    }

    #[sqlx::test]
    async fn a_background_is_a_picture(pool: PgPool) {
        let f = fleet(&pool).await;
        let admin = session(&pool, "alice", Role::Administrator, GrantScope::Global).await;
        let viewer = session(&pool, "victor", Role::Viewer, GrantScope::Site(f.site)).await;
        let body = json!({"name": "Floor", "place": {"kind": "site", "id": f.site}});
        let (_, map) = call(&pool, &admin, "POST", "/api/maps", Some(body)).await;
        let uri = format!("/api/maps/{}/background", map["id"].as_str().unwrap());
        let (status, _) = call(&pool, &admin, "GET", &uri, None).await;
        assert_eq!(status, StatusCode::NOT_FOUND);
        let encode = |bytes: &[u8]| base64::engine::general_purpose::STANDARD.encode(bytes);
        let svg = json!({"image": encode(b"<svg xmlns=\"http://www.w3.org/2000/svg\"/>")});
        let (status, error) = call(&pool, &admin, "PUT", &uri, Some(svg)).await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert_eq!(error["error"], "The background is a PNG, JPEG or WebP image.");
        let big = json!({"image": encode(&[0u8; (1 << 20) + 1])});
        let (status, _) = call(&pool, &admin, "PUT", &uri, Some(big)).await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        let png = json!({"image": encode(PNG)});
        let (status, _) = call(&pool, &viewer, "PUT", &uri, Some(png.clone())).await;
        assert_eq!(status, StatusCode::FORBIDDEN);
        let (status, _) = call(&pool, &admin, "PUT", &uri, Some(png)).await;
        assert_eq!(status, StatusCode::NO_CONTENT);
        let image = raw(&pool, &viewer, "GET", &uri, None).await;
        assert_eq!(image.status, StatusCode::OK);
        assert_eq!(image.content_type.as_deref(), Some("image/png"));
        assert_eq!(image.bytes, PNG);
        let (_, list) = call(&pool, &viewer, "GET", "/api/maps", None).await;
        assert!(!list["maps"][0]["background_updated_at"].is_null());
        let (status, _) = call(&pool, &admin, "DELETE", &uri, None).await;
        assert_eq!(status, StatusCode::NO_CONTENT);
        let (status, _) = call(&pool, &admin, "GET", &uri, None).await;
        assert_eq!(status, StatusCode::NOT_FOUND);
    }

    #[sqlx::test]
    async fn a_dashboard_shows_a_map_within_its_sharing(pool: PgPool) {
        let f = fleet(&pool).await;
        let admin = session(&pool, "alice", Role::Administrator, GrantScope::Global).await;
        let (_, map) = call(&pool, &admin, "POST", "/api/maps",
            Some(json!({"name": "Paris", "place": {"kind": "site", "id": f.site}}))).await;
        let (_, whole) = call(&pool, &admin, "POST", "/api/maps",
            Some(json!({"name": "All", "place": {"kind": "customer", "id": f.customer}}))).await;
        let panel = |map: &Value| json!({"id": "m", "title": "Network", "kind": "map", "width": 3,
                                         "map_id": map["id"]});
        let spec = |map: &Value| json!({"version": 1, "range": "6h", "panels": [panel(map)]});
        let body = json!({"name": "Wall", "shared_with": {"kind": "site", "id": f.site},
                          "spec": spec(&map)});
        let (status, dashboard) = call(&pool, &admin, "POST", "/api/dashboards", Some(body)).await;
        assert_eq!(status, StatusCode::CREATED, "{dashboard}");
        // A customer's map does not fit on a site's dashboard.
        let body = json!({"name": "Wall", "shared_with": {"kind": "site", "id": f.site},
                          "spec": spec(&whole)});
        let (status, error) = call(&pool, &admin, "POST", "/api/dashboards", Some(body)).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{error}");
        // A map panel takes nothing but its map.
        let mut extra = panel(&map);
        extra["count"] = json!(5);
        let body = json!({"name": "Wall", "shared_with": {"kind": "personal"},
                          "spec": {"version": 1, "range": "6h", "panels": [extra]}});
        let (status, error) = call(&pool, &admin, "POST", "/api/dashboards", Some(body)).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{error}");

        // The data, within the dashboard's site: the customer's map shows
        // paris only on a personal dashboard narrowed to it.
        let body = json!({"range": "6h", "within": {"kind": "site", "id": f.site},
                          "panels": [panel(&whole)]});
        let (status, data) = call(&pool, &admin, "POST", "/api/dashboard-data", Some(body)).await;
        assert_eq!(status, StatusCode::OK, "{data}");
        assert_eq!(data["panels"][0]["kind"], "map", "{data}");
        let shown = &data["panels"][0]["map"];
        assert_eq!(shown["sites"].as_array().unwrap().len(), 1, "{shown}");
        assert!(!shown.to_string().contains(&f.db.to_string()));
        assert!(shown["nodes"].as_array().unwrap().len() >= 4);
        // A map deleted since says so in the panel.
        let (status, _) = call(&pool, &admin, "DELETE", &format!("/api/maps/{}", whole["id"].as_str().unwrap()), None).await;
        assert_eq!(status, StatusCode::NO_CONTENT);
        let body = json!({"range": "6h", "panels": [panel(&whole)]});
        let (_, data) = call(&pool, &admin, "POST", "/api/dashboard-data", Some(body)).await;
        assert_eq!(data["panels"][0]["kind"], "error", "{data}");
    }
}
