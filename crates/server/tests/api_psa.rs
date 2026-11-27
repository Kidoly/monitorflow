//! The PSA connection (ADR-0164, ADR-0166, ADR-0191): who manages it, the
//! secret sealed and never answered, the test against a stand-in for
//! HaloPSA, customers and sites matched, the priority map, the asset
//! settings, and the audit. Never a real instance. Ignored without `DATABASE_URL`.

mod fake_halo;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use fake_halo::{FakeHalo, AGENT, CLIENT_ID, CLIENT_SECRET};
use monitorflow_common::redact::Secret;
use monitorflow_server::api::auth::CSRF_HEADER;
use monitorflow_server::api::users::{self, GrantScope};
use monitorflow_server::api::{self, ApiSettings, ApiState, Role};
use monitorflow_server::grpc::Registry;
use monitorflow_server::secrets::{self, Column};
use serde_json::{json, Value};
use sqlx::PgPool;
use tower::ServiceExt;

const PASSWORD: &str = "correct horse battery";
const CONNECTION: &str = "/api/psa/connection";
const TEST: &str = "/api/psa/connection/test";

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

async fn call(
    pool: &PgPool,
    method: &str,
    uri: &str,
    cookie: &str,
    body: Option<Value>,
) -> (StatusCode, Value) {
    let state = ApiState::new(pool.clone(), Registry::default(), ApiSettings::default());
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
    let bytes = axum::body::to_bytes(response.into_body(), 1 << 20)
        .await
        .unwrap();
    (
        status,
        serde_json::from_slice(&bytes).unwrap_or(Value::Null),
    )
}

async fn login(pool: &PgPool, name: &str, role: Role, scope: GrantScope) -> String {
    users::create_user_with(pool, name, &Secret::new(PASSWORD), role, scope, "test")
        .await
        .unwrap();
    let state = ApiState::new(pool.clone(), Registry::default(), ApiSettings::default());
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
    let cookie = response.headers()["set-cookie"].to_str().unwrap();
    cookie.split(';').next().unwrap().to_owned()
}

async fn admin(pool: &PgPool) -> String {
    login(pool, "root", Role::Administrator, GrantScope::Global).await
}

fn settings(halo: &FakeHalo) -> Value {
    json!({
        "base_url": format!("{}/api/", halo.url),
        "client_id": CLIENT_ID,
        "client_secret": CLIENT_SECRET,
    })
}

async fn actions(pool: &PgPool) -> Vec<(String, Value)> {
    sqlx::query_as("SELECT action, details FROM audit_log WHERE action LIKE 'psa.%' ORDER BY id")
        .fetch_all(pool)
        .await
        .unwrap()
}

db_tests! {
    #[sqlx::test]
    async fn only_global_administrators_see_or_change_it(pool: PgPool) {
        secrets::install_for_tests();
        let halo = FakeHalo::start().await;
        let customer: uuid::Uuid =
            sqlx::query_scalar("INSERT INTO customers (name) VALUES ('acme') RETURNING id")
                .fetch_one(&pool)
                .await
                .unwrap();
        for (name, role, scope) in [
            ("viewer", Role::Viewer, GrantScope::Global),
            ("operator", Role::Operator, GrantScope::Global),
            ("acme-admin", Role::Administrator, GrantScope::Customer(customer)),
        ] {
            let cookie = login(&pool, name, role, scope).await;
            assert_eq!(call(&pool, "GET", CONNECTION, &cookie, None).await.0, StatusCode::FORBIDDEN);
            assert_eq!(
                call(&pool, "PUT", CONNECTION, &cookie, Some(settings(&halo))).await.0,
                StatusCode::FORBIDDEN
            );
            assert_eq!(
                call(&pool, "POST", TEST, &cookie, Some(settings(&halo))).await.0,
                StatusCode::FORBIDDEN
            );
        }
        assert!(halo.token_requests().is_empty());
        let cookie = admin(&pool).await;
        let (status, body) = call(&pool, "GET", CONNECTION, &cookie, None).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body, json!({ "connection": null }));
    }

    #[sqlx::test]
    async fn the_test_signs_in_reads_the_agent_and_saves_nothing(pool: PgPool) {
        secrets::install_for_tests();
        let halo = FakeHalo::start().await;
        let cookie = admin(&pool).await;
        let (status, body) = call(&pool, "POST", TEST, &cookie, Some(settings(&halo))).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(body, json!({ "ok": true, "agent_name": AGENT, "error": null }));
        let form = &halo.token_requests()[0];
        assert_eq!(form["grant_type"], "client_credentials");
        assert_eq!(form["scope"], "all");
        assert_eq!(halo.requests(), ["GET /api/Agent/me"]);
        let (_, body) = call(&pool, "GET", CONNECTION, &cookie, None).await;
        assert_eq!(body["connection"], Value::Null);

        // A wrong secret: Halo's words, never the secret.
        let mut wrong = settings(&halo);
        wrong["client_secret"] = "not-the-secret".into();
        let (_, body) = call(&pool, "POST", TEST, &cookie, Some(wrong)).await;
        assert_eq!(body["ok"], false);
        let error = body["error"].as_str().unwrap();
        assert!(error.contains("401") && error.contains("invalid_client"), "{error}");
        assert!(error.contains("Client Credentials"), "{error}");
        assert!(!error.contains("not-the-secret"));

        // Nothing listening.
        let gone = json!({ "base_url": "http://127.0.0.1:9", "client_id": CLIENT_ID, "client_secret": CLIENT_SECRET });
        let (_, body) = call(&pool, "POST", TEST, &cookie, Some(gone)).await;
        assert_eq!(body["ok"], false);
        assert!(body["error"].as_str().unwrap().contains("127.0.0.1"), "{body}");
    }

    #[sqlx::test]
    async fn the_secret_is_sealed_never_answered_and_kept_for_the_same_address(pool: PgPool) {
        secrets::install_for_tests();
        let halo = FakeHalo::start().await;
        let cookie = admin(&pool).await;
        let (status, body) = call(&pool, "PUT", CONNECTION, &cookie, Some(settings(&halo))).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(body["base_url"], halo.url.as_str());
        assert_eq!(body["client_id"], CLIENT_ID);
        assert_eq!(body["enabled"], true);
        assert!(!body.to_string().contains(CLIENT_SECRET));
        let stored: String = sqlx::query_scalar("SELECT client_secret FROM psa_connections")
            .fetch_one(&pool)
            .await
            .unwrap();
        assert!(!stored.contains(CLIENT_SECRET));
        assert_eq!(
            secrets::open(Column::PsaConnections, &stored).unwrap().expose(),
            CLIENT_SECRET
        );

        // Testing the stored connection records its agent.
        let (_, body) = call(&pool, "POST", TEST, &cookie, Some(json!({}))).await;
        assert_eq!(body["ok"], true, "{body}");
        let (_, body) = call(&pool, "GET", CONNECTION, &cookie, None).await;
        assert_eq!(body["connection"]["agent_name"], AGENT);
        assert!(body["connection"]["last_success_at"].is_string());

        // An edit without the secret keeps it for the same address...
        let mut edit = settings(&halo);
        edit["client_secret"] = "".into();
        edit["enabled"] = false.into();
        let (status, body) = call(&pool, "PUT", CONNECTION, &cookie, Some(edit.clone())).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(body["enabled"], false);
        assert_eq!(body["agent_name"], AGENT);
        let (_, body) = call(&pool, "POST", TEST, &cookie, Some(json!({}))).await;
        assert_eq!(body["ok"], true, "{body}");

        // ...but never sends it to another one.
        edit["base_url"] = "https://elsewhere.example.com".into();
        let (status, body) = call(&pool, "PUT", CONNECTION, &cookie, Some(edit.clone())).await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert_eq!(body["error"], "Enter the Client Secret.");
        let (_, body) = call(&pool, "POST", TEST, &cookie, Some(edit)).await;
        assert_eq!(body["error"], "Enter the Client Secret.");

        let (status, _) = call(&pool, "DELETE", CONNECTION, &cookie, None).await;
        assert_eq!(status, StatusCode::NO_CONTENT);
        let (status, _) = call(&pool, "DELETE", CONNECTION, &cookie, None).await;
        assert_eq!(status, StatusCode::NOT_FOUND);

        let audit = actions(&pool).await;
        let names: Vec<&str> = audit.iter().map(|(a, _)| a.as_str()).collect();
        assert_eq!(names, ["psa.connection.create", "psa.connection.update", "psa.connection.delete"]);
        assert_eq!(audit[0].1["secret_changed"], true);
        assert_eq!(audit[1].1["secret_changed"], false);
        for (_, details) in &audit {
            assert!(!details.to_string().contains(CLIENT_SECRET));
        }
    }

    #[sqlx::test]
    async fn bad_values_are_refused_in_plain_words(pool: PgPool) {
        secrets::install_for_tests();
        let cookie = admin(&pool).await;
        for (body, error) in [
            (json!({"base_url": "halo.example.com", "client_id": "x", "client_secret": "y"}),
             "Enter a full address, like https://halo.example.com."),
            (json!({"base_url": "https://u:p@halo.example.com", "client_id": "x", "client_secret": "y"}),
             "Put no user or password in the address: use the Client ID and Secret."),
            (json!({"base_url": "https://halo.example.com", "client_secret": "y"}),
             "Enter the Client ID."),
            (json!({"base_url": "https://halo.example.com", "client_id": "x"}),
             "Enter the Client Secret."),
            (json!({"kind": "other", "base_url": "https://halo.example.com", "client_id": "x", "client_secret": "y"}),
             "`kind` is `halopsa`."),
        ] {
            let (status, answer) = call(&pool, "PUT", CONNECTION, &cookie, Some(body)).await;
            assert_eq!(status, StatusCode::BAD_REQUEST);
            assert_eq!(answer["error"], error);
        }
    }

    #[sqlx::test]
    async fn sites_are_matched_to_clients_read_from_the_psa(pool: PgPool) {
        secrets::install_for_tests();
        let halo = FakeHalo::start().await;
        let cookie = admin(&pool).await;
        let (status, body) = call(&pool, "GET", "/api/psa/clients?search=example", &cookie, None).await;
        assert_eq!(status, StatusCode::CONFLICT);
        assert_eq!(body["error"], "Set up the HaloPSA connection first.");
        call(&pool, "PUT", CONNECTION, &cookie, Some(settings(&halo))).await;

        let (status, body) = call(&pool, "GET", "/api/psa/clients?search=example", &cookie, None).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(
            body["records"],
            json!([{"id": 10, "name": "Example Industries"}, {"id": 11, "name": "Example Logistics"}])
        );
        let (_, body) = call(&pool, "GET", "/api/psa/clients/10/sites", &cookie, None).await;
        assert_eq!(body["records"], json!([{"id": 100, "name": "Main office"}, {"id": 101, "name": "Warehouse"}]));

        let customer: uuid::Uuid =
            sqlx::query_scalar("INSERT INTO customers (name) VALUES ('acme') RETURNING id")
                .fetch_one(&pool).await.unwrap();
        let paris: uuid::Uuid =
            sqlx::query_scalar("INSERT INTO sites (name, customer_id) VALUES ('paris', $1) RETURNING id")
                .bind(customer).fetch_one(&pool).await.unwrap();
        let lyon: uuid::Uuid =
            sqlx::query_scalar("INSERT INTO sites (name, customer_id) VALUES ('lyon', $1) RETURNING id")
                .bind(customer).fetch_one(&pool).await.unwrap();
        let link = |site: uuid::Uuid| format!("/api/psa/site-links/{site}");

        let (status, body) = call(&pool, "PUT", &link(paris), &cookie, Some(json!({"client_id": 10, "psa_site_id": 100}))).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(body["client_name"], "Example Industries");
        assert_eq!(body["psa_site_name"], "Main office");

        // A site of another client, an unknown client, a site taken.
        let (status, body) = call(&pool, "PUT", &link(lyon), &cookie, Some(json!({"client_id": 10, "psa_site_id": 110}))).await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert_eq!(body["error"], "That HaloPSA site belongs to another client than Example Industries.");
        let (status, body) = call(&pool, "PUT", &link(lyon), &cookie, Some(json!({"client_id": 99}))).await;
        assert_eq!((status, body["error"].as_str()), (StatusCode::BAD_REQUEST, Some("HaloPSA has no such client.")));
        let (status, body) = call(&pool, "PUT", &link(lyon), &cookie, Some(json!({"client_id": 10, "psa_site_id": 100}))).await;
        assert_eq!((status, body["error"].as_str()), (StatusCode::CONFLICT, Some("That HaloPSA site is already matched to paris.")));
        let (status, _) = call(&pool, "PUT", &link(lyon), &cookie, Some(json!({"client_id": 10}))).await;
        assert_eq!(status, StatusCode::OK);

        let (_, body) = call(&pool, "GET", "/api/psa/site-links", &cookie, None).await;
        let sites = body["sites"].as_array().unwrap();
        assert_eq!(sites.len(), 2);
        assert_eq!(sites[0]["site_name"], "lyon");
        assert_eq!(sites[0]["client_id"], 10);
        assert_eq!(sites[0]["psa_site_id"], Value::Null);

        let (status, _) = call(&pool, "DELETE", &link(lyon), &cookie, None).await;
        assert_eq!(status, StatusCode::NO_CONTENT);
        let (status, _) = call(&pool, "DELETE", &link(lyon), &cookie, None).await;
        assert_eq!(status, StatusCode::NOT_FOUND);

        // Another instance: its ids mean nothing, so the matches go.
        let mut moved = settings(&halo);
        moved["base_url"] = format!("{}/other", halo.url).into();
        call(&pool, "PUT", CONNECTION, &cookie, Some(moved)).await;
        let count: i64 = sqlx::query_scalar("SELECT count(*) FROM psa_site_links").fetch_one(&pool).await.unwrap();
        assert_eq!(count, 0);

        let names: Vec<String> = actions(&pool).await.into_iter().map(|(a, _)| a).filter(|a| a.starts_with("psa.link")).collect();
        assert_eq!(names, ["psa.link.update", "psa.link.update", "psa.link.delete"]);
    }

    #[sqlx::test]
    async fn the_psa_s_refusal_is_a_502_in_its_words(pool: PgPool) {
        secrets::install_for_tests();
        let halo = FakeHalo::start().await;
        let cookie = admin(&pool).await;
        let mut wrong = settings(&halo);
        wrong["client_secret"] = "not-the-secret".into();
        call(&pool, "PUT", CONNECTION, &cookie, Some(wrong)).await;
        let (status, body) = call(&pool, "GET", "/api/psa/clients", &cookie, None).await;
        assert_eq!(status, StatusCode::BAD_GATEWAY);
        assert_eq!(body["code"], "psa_unavailable");
        assert!(body["error"].as_str().unwrap().contains("invalid_client"), "{body}");
        let (_, body) = call(&pool, "GET", CONNECTION, &cookie, None).await;
        assert!(body["connection"]["last_error"].as_str().unwrap().contains("invalid_client"));
    }

    #[sqlx::test]
    async fn ticket_settings_come_from_the_psa_s_lists(pool: PgPool) {
        secrets::install_for_tests();
        let halo = FakeHalo::start().await;
        let cookie = admin(&pool).await;
        let (_, body) = call(&pool, "PUT", CONNECTION, &cookie, Some(settings(&halo))).await;
        assert_eq!(
            body["tickets"],
            json!({"ticket_type_id": null, "ticket_type_name": null, "category": null,
                   "min_severity": "critical", "resolved_status_id": 9, "resolved_status_name": "Closed",
                   "warning_priority_id": null, "warning_priority_name": null,
                   "critical_priority_id": null, "critical_priority_name": null,
                   "closed_status_ids": [9], "acknowledge_on_assign": true})
        );
        let (_, body) = call(&pool, "GET", "/api/psa/ticket-types", &cookie, None).await;
        assert_eq!(body["records"], json!([{"id": 3, "name": "Change Request"}, {"id": 1, "name": "Incident"}]));
        let (_, body) = call(&pool, "GET", "/api/psa/categories", &cookie, None).await;
        assert_eq!(body["categories"], json!(["Hardware>Server", "Network"]));
        let (_, body) = call(&pool, "GET", "/api/psa/statuses", &cookie, None).await;
        assert_eq!(body["records"][0], json!({"id": 1, "name": "New"}));

        let wanted = json!({"ticket_type_id": 1, "category": "Hardware>Server", "min_severity": "warning", "resolved_status_id": null});
        let (status, body) = call(&pool, "PUT", "/api/psa/ticket-settings", &cookie, Some(wanted)).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(
            body["tickets"],
            json!({"ticket_type_id": 1, "ticket_type_name": "Incident", "category": "Hardware>Server",
                   "min_severity": "warning", "resolved_status_id": null, "resolved_status_name": null,
                   "warning_priority_id": null, "warning_priority_name": null,
                   "critical_priority_id": null, "critical_priority_name": null,
                   "closed_status_ids": [9], "acknowledge_on_assign": true})
        );
        // Saving the connection again keeps them.
        let (_, body) = call(&pool, "PUT", CONNECTION, &cookie, Some(settings(&halo))).await;
        assert_eq!(body["tickets"]["category"], "Hardware>Server");

        for (bad, error) in [
            (json!({"ticket_type_id": 20}), "HaloPSA has no such ticket type."),
            (json!({"category": "Resolved remotely"}), "HaloPSA has no such category."),
            (json!({"resolved_status_id": 77}), "HaloPSA has no such status."),
            (json!({"min_severity": "info"}), "`min_severity` is `warning` or `critical`."),
        ] {
            let (status, body) = call(&pool, "PUT", "/api/psa/ticket-settings", &cookie, Some(bad)).await;
            assert_eq!((status, body["error"].as_str()), (StatusCode::BAD_REQUEST, Some(error)));
        }
        let audited = actions(&pool).await;
        assert_eq!(audited.iter().filter(|(a, _)| a == "psa.connection.tickets").count(), 1);
    }

    #[sqlx::test]
    async fn tickets_are_listed_with_their_incident(pool: PgPool) {
        secrets::install_for_tests();
        let halo = FakeHalo::start().await;
        let cookie = admin(&pool).await;
        call(&pool, "PUT", CONNECTION, &cookie, Some(settings(&halo))).await;
        let customer: uuid::Uuid =
            sqlx::query_scalar("INSERT INTO customers (name) VALUES ('acme') RETURNING id")
                .fetch_one(&pool).await.unwrap();
        let site: uuid::Uuid =
            sqlx::query_scalar("INSERT INTO sites (name, customer_id) VALUES ('paris', $1) RETURNING id")
                .bind(customer).fetch_one(&pool).await.unwrap();
        let server: uuid::Uuid = sqlx::query_scalar(
            "INSERT INTO servers (peer_id, site_id, hostname, os_family)
             VALUES ('web-01', $1, 'web-01', 'debian') RETURNING id",
        )
        .bind(site).fetch_one(&pool).await.unwrap();
        let incident: uuid::Uuid = sqlx::query_scalar(
            "INSERT INTO incidents (server_id, metric, instance, severity, opened_at, last_value,
                                    last_value_at, peak_value, threshold)
             VALUES ($1, 'disk_used_percent', '', 'critical', now(), 97, now(), 97, '{}') RETURNING id",
        )
        .bind(server).fetch_one(&pool).await.unwrap();
        sqlx::query(
            "INSERT INTO psa_tickets (incident_id, connection_id, ticket_id, state)
             SELECT $1, id, 5001, 'open' FROM psa_connections",
        )
        .bind(incident).execute(&pool).await.unwrap();
        let (status, body) = call(&pool, "GET", "/api/psa/tickets", &cookie, None).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        let ticket = &body["tickets"][0];
        assert_eq!(ticket["ticket_id"], 5001);
        assert_eq!(ticket["state"], "open");
        assert_eq!(ticket["server_name"], "web-01");
        assert_eq!(ticket["site_name"], "paris");
        assert_eq!(ticket["severity"], "critical");
        let viewer = login(&pool, "viewer", Role::Viewer, GrantScope::Global).await;
        assert_eq!(call(&pool, "GET", "/api/psa/tickets", &viewer, None).await.0, StatusCode::FORBIDDEN);
    }

    #[sqlx::test]
    async fn priorities_come_from_the_ticket_type_s_sla(pool: PgPool) {
        secrets::install_for_tests();
        let halo = FakeHalo::start().await;
        let cookie = admin(&pool).await;
        call(&pool, "PUT", CONNECTION, &cookie, Some(settings(&halo))).await;
        let (status, body) = call(&pool, "GET", "/api/psa/priorities?ticket_type_id=1", &cookie, None).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(
            body["records"],
            json!([{"id": 1, "name": "Critical"}, {"id": 2, "name": "High"}, {"id": 3, "name": "Medium"}])
        );
        let (_, body) = call(&pool, "GET", "/api/psa/priorities?ticket_type_id=3", &cookie, None).await;
        assert_eq!(body["records"], json!([{"id": 1, "name": "P1"}]));

        let wanted = json!({"ticket_type_id": 1, "warning_priority_id": 3, "critical_priority_id": 1,
                            "closed_status_ids": [9, 2, 9], "acknowledge_on_assign": false});
        let (status, body) = call(&pool, "PUT", "/api/psa/ticket-settings", &cookie, Some(wanted)).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        let tickets = &body["tickets"];
        assert_eq!(tickets["warning_priority_name"], "Medium");
        assert_eq!(tickets["critical_priority_name"], "Critical");
        assert_eq!(tickets["closed_status_ids"], json!([2, 9]));
        assert_eq!(tickets["acknowledge_on_assign"], false);
        for (bad, error) in [
            (json!({"ticket_type_id": 3, "warning_priority_id": 3}), "HaloPSA has no such priority for that ticket type."),
            (json!({"closed_status_ids": [77]}), "HaloPSA has no such status."),
        ] {
            let (status, body) = call(&pool, "PUT", "/api/psa/ticket-settings", &cookie, Some(bad)).await;
            assert_eq!((status, body["error"].as_str()), (StatusCode::BAD_REQUEST, Some(error)));
        }
    }

    #[sqlx::test]
    async fn customers_are_matched_suggested_and_bind_their_sites(pool: PgPool) {
        secrets::install_for_tests();
        let halo = FakeHalo::start().await;
        let cookie = admin(&pool).await;
        call(&pool, "PUT", CONNECTION, &cookie, Some(settings(&halo))).await;
        let customer = |name: &'static str| {
            let pool = pool.clone();
            async move {
                sqlx::query_scalar::<_, uuid::Uuid>("INSERT INTO customers (name) VALUES ($1) RETURNING id")
                    .bind(name).fetch_one(&pool).await.unwrap()
            }
        };
        let industries = customer("Example Industries SAS").await;
        let sample = customer("SAMPLE").await;
        let other = customer("Nothing Alike").await;
        let paris: uuid::Uuid =
            sqlx::query_scalar("INSERT INTO sites (name, customer_id) VALUES ('paris', $1) RETURNING id")
                .bind(industries).fetch_one(&pool).await.unwrap();

        let (status, body) = call(&pool, "GET", "/api/psa/customer-links/suggestions", &cookie, None).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        let suggested: Vec<(String, i64)> = body["suggestions"].as_array().unwrap().iter()
            .map(|s| (s["customer_name"].as_str().unwrap().to_owned(), s["client_id"].as_i64().unwrap()))
            .collect();
        assert_eq!(suggested, [("Example Industries SAS".to_owned(), 10), ("SAMPLE".to_owned(), 12)]);

        let link = |id: uuid::Uuid| format!("/api/psa/customer-links/{id}");
        let (status, body) = call(&pool, "PUT", &link(industries), &cookie, Some(json!({"client_id": 10}))).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(body["client_name"], "Example Industries");
        assert_eq!(body["site_count"], 1);
        let (status, body) = call(&pool, "PUT", &link(other), &cookie, Some(json!({"client_id": 10}))).await;
        assert_eq!((status, body["error"].as_str()), (StatusCode::CONFLICT, Some("That HaloPSA client is already matched to Example Industries SAS.")));
        let (status, _) = call(&pool, "PUT", &link(sample), &cookie, Some(json!({"client_id": 99}))).await;
        assert_eq!(status, StatusCode::BAD_REQUEST);

        // A site of a matched customer only takes the customer's client.
        let site_link = format!("/api/psa/site-links/{paris}");
        let (status, body) = call(&pool, "PUT", &site_link, &cookie, Some(json!({"client_id": 11}))).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
        assert!(body["error"].as_str().unwrap().contains("choose that client"));
        let (status, _) = call(&pool, "PUT", &site_link, &cookie, Some(json!({"client_id": 10, "psa_site_id": 101}))).await;
        assert_eq!(status, StatusCode::OK);
        // And the customer cannot move away from its sites' client.
        let (status, body) = call(&pool, "PUT", &link(industries), &cookie, Some(json!({"client_id": 11}))).await;
        assert_eq!(status, StatusCode::CONFLICT);
        assert!(body["error"].as_str().unwrap().contains("The site paris"));

        let (_, body) = call(&pool, "GET", "/api/psa/customer-links", &cookie, None).await;
        let rows = body["customers"].as_array().unwrap();
        let row = rows.iter().find(|r| r["customer_id"] == json!(industries)).unwrap();
        assert_eq!((row["matched_sites"].as_i64(), row["client_id"].as_i64()), (Some(1), Some(10)));
        let (status, _) = call(&pool, "DELETE", &link(industries), &cookie, None).await;
        assert_eq!(status, StatusCode::NO_CONTENT);
        let (status, _) = call(&pool, "DELETE", &link(industries), &cookie, None).await;
        assert_eq!(status, StatusCode::NOT_FOUND);

        // The default client, and a site of it.
        let (status, body) = call(&pool, "PUT", "/api/psa/default-client", &cookie, Some(json!({"client_id": 11, "psa_site_id": 110}))).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(body["default_client"]["default_client_name"], "Example Logistics");
        assert_eq!(body["default_client"]["default_site_name"], "Head office");
        let (status, _) = call(&pool, "PUT", "/api/psa/default-client", &cookie, Some(json!({"client_id": 11, "psa_site_id": 100}))).await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        let (_, body) = call(&pool, "PUT", "/api/psa/default-client", &cookie, Some(json!({"client_id": null}))).await;
        assert_eq!(body["default_client"]["default_client_id"], Value::Null);

        let names: Vec<String> = actions(&pool).await.into_iter().map(|(a, _)| a).collect();
        assert!(names.contains(&"psa.connection.default".to_owned()));
        assert_eq!(names.iter().filter(|a| *a == "psa.link.delete").count(), 1);
        let viewer = login(&pool, "viewer", Role::Viewer, GrantScope::Global).await;
        assert_eq!(call(&pool, "GET", "/api/psa/customer-links", &viewer, None).await.0, StatusCode::FORBIDDEN);
    }

    #[sqlx::test]
    async fn asset_settings_come_from_the_psa_s_types_and_fields(pool: PgPool) {
        secrets::install_for_tests();
        let halo = FakeHalo::start().await;
        let cookie = admin(&pool).await;
        call(&pool, "PUT", CONNECTION, &cookie, Some(settings(&halo))).await;
        let (_, body) = call(&pool, "GET", "/api/psa/asset-types", &cookie, None).await;
        assert_eq!(body["records"], json!([{"id": 3, "name": "Server"}, {"id": 4, "name": "Workstation"}]));
        let (_, body) = call(&pool, "GET", "/api/psa/asset-types/3/fields", &cookie, None).await;
        assert_eq!(body["records"][0], json!({"id": 13, "name": "Manufacturer"}));
        // A type without fields in its details: read from one of its assets.
        halo.add_asset(json!({"id": 950, "client_id": 10, "assettype_id": 4,
                              "fields": [{"id": 21, "name": "Service Tag", "value": "x"}]}));
        let (_, body) = call(&pool, "GET", "/api/psa/asset-types/4/fields", &cookie, None).await;
        assert_eq!(body["records"], json!([{"id": 21, "name": "Service Tag"}]));

        let (_, body) = call(&pool, "GET", "/api/psa/asset-settings", &cookie, None).await;
        assert_eq!(body, json!({"asset_sync": false, "asset_inactive_on_removal": false, "types": []}));
        let wanted = json!({"asset_sync": true, "types": [{"family": "linux", "type_id": 3,
            "hostname_field": "inventory_number", "serial_field": "12", "manufacturer_field": "13"}]});
        let (status, body) = call(&pool, "PUT", "/api/psa/asset-settings", &cookie, Some(wanted)).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(body["types"][0]["type_name"], "Server");
        assert_eq!(body["types"][0]["model_field"], Value::Null);
        let (_, body) = call(&pool, "GET", CONNECTION, &cookie, None).await;
        assert_eq!(body["connection"]["assets"]["asset_sync"], true);
        for (bad, error) in [
            (json!({"asset_sync": true}), "Choose an asset type for Linux or Windows servers before turning the sync on."),
            (json!({"asset_sync": false, "types": [{"family": "mac", "type_id": 3}]}), "`family` is `linux` or `windows`."),
            (json!({"asset_sync": false, "types": [{"family": "linux", "type_id": 9}]}), "HaloPSA has no such asset type."),
            (json!({"asset_sync": false, "types": [{"family": "linux", "type_id": 3, "serial_field": "99"}]}),
             "The asset type has no field 99: choose `inventory_number` or one of its fields."),
        ] {
            let (status, body) = call(&pool, "PUT", "/api/psa/asset-settings", &cookie, Some(bad)).await;
            assert_eq!((status, body["error"].as_str()), (StatusCode::BAD_REQUEST, Some(error)));
        }

        // A proposed asset confirmed, another chosen, or a new one asked for.
        let customer: uuid::Uuid =
            sqlx::query_scalar("INSERT INTO customers (name) VALUES ('acme') RETURNING id")
                .fetch_one(&pool).await.unwrap();
        let site: uuid::Uuid =
            sqlx::query_scalar("INSERT INTO sites (name, customer_id) VALUES ('paris', $1) RETURNING id")
                .bind(customer).fetch_one(&pool).await.unwrap();
        let server: uuid::Uuid = sqlx::query_scalar(
            "INSERT INTO servers (peer_id, site_id, hostname, os_family)
             VALUES ('web-01', $1, 'web-01', 'debian') RETURNING id",
        )
        .bind(site).fetch_one(&pool).await.unwrap();
        let path = format!("/api/psa/asset-links/{server}");
        let (status, _) = call(&pool, "PUT", &path, &cookie, Some(json!({"asset_id": 950}))).await;
        assert_eq!(status, StatusCode::CONFLICT, "no client for the server yet");
        call(&pool, "PUT", &format!("/api/psa/customer-links/{customer}"), &cookie, Some(json!({"client_id": 10}))).await;
        sqlx::query(
            "INSERT INTO psa_asset_links (server_id, connection_id, asset_id, state, matched_by, asset_name)
             SELECT $1, id, 950, 'proposed', 'serial', 'WEB-01' FROM psa_connections",
        )
        .bind(server).execute(&pool).await.unwrap();
        let (_, body) = call(&pool, "GET", "/api/psa/asset-links", &cookie, None).await;
        assert_eq!((body["proposed"].as_i64(), body["servers"][0]["state"].as_str()), (Some(1), Some("proposed")));
        let (status, body) = call(&pool, "PUT", &path, &cookie, Some(json!({"asset_id": 950}))).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!((body["state"].as_str(), body["matched_by"].as_str()), (Some("linked"), Some("serial")));
        let (status, body) = call(&pool, "PUT", &path, &cookie, Some(json!({"asset_id": null}))).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!((body["state"].as_str(), body["asset_id"].as_i64()), (Some("create"), None));
        let (status, body) = call(&pool, "PUT", &path, &cookie, Some(json!({"asset_id": 12345}))).await;
        assert_eq!((status, body["error"].as_str()), (StatusCode::BAD_REQUEST, Some("HaloPSA has no such asset.")));
        let (status, body) = call(&pool, "POST", "/api/psa/asset-links/sync", &cookie, None).await;
        assert_eq!((status, body["queued"].as_u64()), (StatusCode::OK, Some(0)));
        let (status, _) = call(&pool, "DELETE", &path, &cookie, None).await;
        assert_eq!(status, StatusCode::NO_CONTENT);
        let names: Vec<String> = actions(&pool).await.into_iter().map(|(a, _)| a).collect();
        assert_eq!(names.iter().filter(|a| a.starts_with("psa.asset.")).count(), 3);
        assert!(names.contains(&"psa.connection.assets".to_owned()));
    }
}
