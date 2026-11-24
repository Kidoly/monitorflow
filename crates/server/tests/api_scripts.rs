//! The script library, its runs and automation rules (ADR-0189): who may
//! manage and run scripts, versions, classification and approval, the
//! signed command each server gets, servers that refuse scripts, rules
//! fired by events with their limits, and scripts the AI proposes.
//! Ignored without `DATABASE_URL`.

use axum::body::Body;
use axum::http::{Request, StatusCode};
use monitorflow_common::proto::v1::RunScript;
use monitorflow_common::redact::Secret;
use monitorflow_server::api::auth::CSRF_HEADER;
use monitorflow_server::api::users::{self, GrantScope};
use monitorflow_server::api::{self, ApiSettings, ApiState, Role};
use monitorflow_server::grpc::Registry;
use monitorflow_server::remediation::risk::{RuleSet, Shell, Tier};
use monitorflow_server::scripts::{ai, automation};
use prost::Message;
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

struct Harness {
    pool: PgPool,
    customer: Uuid,
    site: Uuid,
    /// Reports scripts accepted.
    web1: Uuid,
    /// Reports scripts off.
    web2: Uuid,
}

impl Harness {
    async fn new(pool: &PgPool) -> Self {
        monitorflow_server::secrets::install_for_tests();
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
        let mut servers = Vec::new();
        for (name, word) in [("web-1", "accepted"), ("web-2", "off")] {
            servers.push(
                one(
                    "INSERT INTO servers (peer_id, site_id, hostname, os_family, command_kinds)
                     VALUES ($1, $2, $1, 'debian', jsonb_build_object('run_script', $3::text))
                     RETURNING id",
                )
                .bind(name)
                .bind(site)
                .bind(word)
                .fetch_one(pool)
                .await
                .unwrap(),
            );
        }
        Self {
            pool: pool.clone(),
            customer,
            site,
            web1: servers[0],
            web2: servers[1],
        }
    }

    async fn send(
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
        );
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
        let state = ApiState::new(
            self.pool.clone(),
            Registry::default(),
            ApiSettings::default(),
        );
        let body = json!({ "username": name, "password": PASSWORD }).to_string();
        let request = Request::post("/api/auth/login")
            .header(CSRF_HEADER, "1")
            .header("content-type", "application/json")
            .body(Body::from(body))
            .unwrap();
        let response = api::router(state).oneshot(request).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        response.headers()["set-cookie"]
            .to_str()
            .unwrap()
            .split(';')
            .next()
            .unwrap()
            .to_owned()
    }

    fn script(&self, name: &str, body: &str, run_role: &str) -> Value {
        json!({
            "customer_id": self.customer, "name": name, "interpreter": "sh", "run_role": run_role,
            "body": body, "ai_allowed": true,
            "parameters": [{"name": "days", "type": "integer", "required": true}],
        })
    }

    /// The `run_script` commands of `server`.
    async fn commands(&self, server: Uuid) -> Vec<RunScript> {
        let payloads: Vec<Vec<u8>> = sqlx::query_scalar(
            "SELECT payload FROM commands WHERE server_id = $1 AND kind = 'run_script' ORDER BY issued_at",
        )
        .bind(server)
        .fetch_all(&self.pool)
        .await
        .unwrap();
        payloads
            .iter()
            .map(|p| RunScript::decode(p.as_slice()).unwrap())
            .collect()
    }

    async fn audited(&self, action: &str) -> i64 {
        sqlx::query_scalar("SELECT count(*) FROM audit_log WHERE action = $1")
            .bind(action)
            .fetch_one(&self.pool)
            .await
            .unwrap()
    }

    /// An `incident.opened` event of `metric` on `server`, as the trigger
    /// of integrations writes it.
    async fn incident_opened(&self, server: Uuid, metric: &str) {
        sqlx::query(
            "SELECT integration_emit('incident.opened', 'incident:' || gen_random_uuid(), $1, NULL,
                jsonb_build_object('incident', jsonb_build_object('id', gen_random_uuid(),
                                                                  'metric', $2::text)))",
        )
        .bind(server)
        .bind(metric)
        .execute(&self.pool)
        .await
        .unwrap();
    }
}

db_tests! {
    #[sqlx::test]
    async fn scripts_are_managed_by_administrators_and_versioned(pool: PgPool) {
        let h = Harness::new(&pool).await;
        let admin = h.login("alice", Role::Administrator, GrantScope::Customer(h.customer)).await;
        let operator = h.login("bob", Role::Operator, GrantScope::Site(h.site)).await;
        let other = sqlx::query_scalar::<_, Uuid>("INSERT INTO customers (name) VALUES ('other') RETURNING id")
            .fetch_one(&pool).await.unwrap();
        let outsider = h.login("eve", Role::Administrator, GrantScope::Customer(other)).await;
        let body = h.script("disk", "df -h\n", "operator");

        assert_eq!(h.send("POST", "/api/remediation/scripts", &operator, Some(body.clone())).await.0, StatusCode::FORBIDDEN);
        assert_eq!(h.send("POST", "/api/remediation/scripts", &outsider, Some(body.clone())).await.0, StatusCode::NOT_FOUND);
        let mut global = body.clone();
        global["customer_id"] = Value::Null;
        assert_eq!(h.send("POST", "/api/remediation/scripts", &admin, Some(global)).await.0, StatusCode::FORBIDDEN);
        let (status, created) = h.send("POST", "/api/remediation/scripts", &admin, Some(body.clone())).await;
        assert_eq!(status, StatusCode::CREATED, "{created}");
        let id = created["script"]["id"].as_str().unwrap().to_owned();
        assert_eq!(h.send("POST", "/api/remediation/scripts", &admin, Some(body.clone())).await.0, StatusCode::CONFLICT);

        // Readers of the customer see it, others do not.
        let (_, list) = h.send("GET", "/api/remediation/scripts", &operator, None).await;
        assert_eq!(list["scripts"][0]["can_manage"], false);
        let (_, list) = h.send("GET", "/api/remediation/scripts", &outsider, None).await;
        assert_eq!(list["scripts"], json!([]));
        let uri = format!("/api/remediation/scripts/{id}");
        assert_eq!(h.send("GET", &uri, &outsider, None).await.0, StatusCode::NOT_FOUND);

        // Only a new text or new parameters make a version.
        let mut change = body.clone();
        change["description"] = json!("Disk usage");
        let (_, same) = h.send("PUT", &uri, &admin, Some(change.clone())).await;
        assert_eq!(same["script"]["version"], 1);
        change["body"] = json!("df -h\nfree -m\n");
        change["note"] = json!("memory too");
        let (status, changed) = h.send("PUT", &uri, &admin, Some(change)).await;
        assert_eq!(status, StatusCode::OK, "{changed}");
        assert_eq!(changed["script"]["version"], 2);
        assert_eq!(changed["versions"][0]["note"], "memory too");
        assert_eq!(changed["versions"][1]["body"], "df -h\n");
        assert_eq!(h.audited("script.update").await, 2);

        assert_eq!(h.send("DELETE", &uri, &operator, None).await.0, StatusCode::FORBIDDEN);
        assert_eq!(h.send("DELETE", &uri, &admin, None).await.0, StatusCode::NO_CONTENT);
        let (_, list) = h.send("GET", "/api/remediation/scripts", &admin, None).await;
        assert_eq!(list["scripts"], json!([]));
        let (_, kept) = h.send("GET", &uri, &admin, None).await;
        assert!(kept["script"]["deleted_at"].is_string());
    }

    #[sqlx::test]
    async fn runs_are_classified_approved_and_sent(pool: PgPool) {
        let h = Harness::new(&pool).await;
        let admin = h.login("alice", Role::Administrator, GrantScope::Customer(h.customer)).await;
        let approver = h.login("carol", Role::Approver, GrantScope::Site(h.site)).await;
        let operator = h.login("bob", Role::Operator, GrantScope::Site(h.site)).await;
        let viewer = h.login("vic", Role::Viewer, GrantScope::Site(h.site)).await;
        let hr = &h;
        let admin_cookie = admin.as_str();
        let create = |b: Value| async move {
            hr.send("POST", "/api/remediation/scripts", admin_cookie, Some(b)).await.1["script"]["id"]
                .as_str()
                .unwrap()
                .to_owned()
        };
        let read_only = create(h.script("disk", "# usage\ndf -h\n", "operator")).await;
        let risky = create(h.script("clean", "for d in /tmp/x; do rm -r \"$d\"; done\n", "operator")).await;
        let forbidden = create(h.script("wipe", "df -h\nrm -rf /\n", "operator")).await;
        let site = json!({"kind": "site", "site_id": h.site});
        let run = |id: &str, cookie: &str, body: Value| {
            let uri = format!("/api/remediation/scripts/{id}/runs");
            let cookie = cookie.to_owned();
            async move { hr.send("POST", &uri, &cookie, Some(body)).await }
        };

        assert_eq!(run(&read_only, &viewer, json!({"target": site, "parameters": {"days": "3"}})).await.0, StatusCode::FORBIDDEN);
        assert_eq!(run(&read_only, &operator, json!({"target": site})).await.0, StatusCode::BAD_REQUEST);
        assert_eq!(run(&read_only, &operator, json!({"target": site, "parameters": {"days": "x"}})).await.0, StatusCode::BAD_REQUEST);
        let (status, refused) = run(&forbidden, &admin, json!({"target": site, "parameters": {"days": "1"}})).await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert!(refused["error"].as_str().unwrap().contains("line 2"), "{refused}");

        // An operator may not approve, so even a read-only run waits.
        let (status, waiting) = run(&read_only, &operator, json!({"target": site, "parameters": {"days": "3"}})).await;
        assert_eq!(status, StatusCode::CREATED, "{waiting}");
        assert_eq!(waiting["run"]["state"], "awaiting_approval");
        assert_eq!(waiting["run"]["tier"], "read_only");
        assert!(h.commands(h.web1).await.is_empty());

        // An approver approves it; web-1 gets it, web-2 refuses scripts.
        let approve = format!("/api/remediation/script-runs/{}/approve", waiting["run"]["id"].as_str().unwrap());
        let (status, sent) = h.send("POST", &approve, &approver, None).await;
        assert_eq!(status, StatusCode::OK, "{sent}");
        assert_eq!(sent["run"]["state"], "dispatched");
        let by_host: Vec<(&str, &str)> = sent["servers"].as_array().unwrap().iter()
            .map(|s| (s["hostname"].as_str().unwrap(), s["status"].as_str().unwrap())).collect();
        assert_eq!(by_host, [("web-1", "queued"), ("web-2", "skipped")]);
        assert!(sent["servers"][1]["error"].as_str().unwrap().contains("MF_ALLOW_SCRIPTS=1"));
        assert!(h.commands(h.web2).await.is_empty(), "web-2 refuses scripts");
        let commands = h.commands(h.web1).await;
        assert_eq!(commands.len(), 1);
        assert_eq!((commands[0].interpreter.as_str(), commands[0].body.as_str()), ("sh", "# usage\ndf -h\n"));
        assert_eq!(commands[0].parameters[0].name, "days");
        assert_eq!(commands[0].parameters[0].value, "3");
        assert_eq!(h.send("POST", &approve, &approver, None).await.0, StatusCode::CONFLICT);

        // High impact: an approver may reject but not approve; an administrator runs it directly.
        let (_, high) = run(&risky, &operator, json!({"target": {"kind": "servers", "server_ids": [h.web1]}, "parameters": {"days": "1"}})).await;
        assert_eq!(high["run"]["tier"], "high_impact");
        let id = high["run"]["id"].as_str().unwrap();
        assert_eq!(h.send("POST", &format!("/api/remediation/script-runs/{id}/approve"), &approver, None).await.0, StatusCode::FORBIDDEN);
        let reject = format!("/api/remediation/script-runs/{id}/reject");
        assert_eq!(h.send("POST", &reject, &approver, Some(json!({"reason": " "}))).await.0, StatusCode::BAD_REQUEST);
        let (_, rejected) = h.send("POST", &reject, &approver, Some(json!({"reason": "not now"}))).await;
        assert_eq!(rejected["run"]["state"], "rejected");
        let (_, direct) = run(&risky, &admin, json!({"target": {"kind": "servers", "server_ids": [h.web1]}, "parameters": {"days": "1"}})).await;
        assert_eq!(direct["run"]["state"], "dispatched");
        assert_eq!(h.commands(h.web1).await.len(), 2);

        // Scheduled later: nothing sent, cancelled by its requester.
        let later = json!({"target": site, "parameters": {"days": "3"}, "scheduled_at": "2099-01-01T00:00:00Z"});
        assert_eq!(run(&read_only, &admin, later).await.0, StatusCode::BAD_REQUEST);
        let later = json!({"target": site, "parameters": {"days": "3"},
                           "scheduled_at": (chrono::Utc::now() + chrono::Duration::hours(2)).to_rfc3339()});
        let (_, scheduled) = run(&read_only, &admin, later).await;
        assert_eq!(scheduled["run"]["state"], "scheduled");
        let cancel = format!("/api/remediation/script-runs/{}/cancel", scheduled["run"]["id"].as_str().unwrap());
        assert_eq!(h.send("POST", &cancel, &operator, None).await.0, StatusCode::FORBIDDEN);
        assert_eq!(h.send("POST", &cancel, &admin, None).await.1["run"]["state"], "cancelled");
        assert_eq!(h.commands(h.web1).await.len(), 2);

        // Listings: the approval queue, and runs of one script.
        let (_, queue) = h.send("GET", "/api/remediation/script-runs?state=awaiting_approval", &admin, None).await;
        assert_eq!(queue["runs"], json!([]));
        let (_, of) = h.send("GET", &format!("/api/remediation/script-runs?script_id={risky}"), &viewer, None).await;
        assert_eq!(of["runs"].as_array().unwrap().len(), 2);
        assert_eq!(of["runs"][0]["can_cancel"], false);
        assert_eq!(h.audited("script.run").await, 4);
        assert_eq!(h.audited("script_run.approve").await, 1);
    }

    #[sqlx::test]
    async fn rules_run_scripts_when_something_happens(pool: PgPool) {
        let h = Harness::new(&pool).await;
        let admin = h.login("alice", Role::Administrator, GrantScope::Customer(h.customer)).await;
        let operator = h.login("bob", Role::Operator, GrantScope::Customer(h.customer)).await;
        let (_, script) = h.send("POST", "/api/remediation/scripts", &admin, Some(h.script("disk", "df -h\n", "operator"))).await;
        let script = script["script"]["id"].as_str().unwrap().to_owned();
        let rule = json!({
            "name": "Disk report", "customer_id": h.customer, "event": "incident_opened",
            "metric": "monitorflow_disk_used_ratio", "script_id": script, "parameters": {"days": "2"},
            "cooldown_minutes": 30,
        });
        assert_eq!(h.send("POST", "/api/remediation/automation-rules", &operator, Some(rule.clone())).await.0, StatusCode::FORBIDDEN);
        let mut bad = rule.clone();
        bad["event"] = json!("check_failed");
        bad["metric"] = Value::Null;
        assert_eq!(h.send("POST", "/api/remediation/automation-rules", &admin, Some(bad)).await.0, StatusCode::BAD_REQUEST);
        let mut no_values = rule.clone();
        no_values["parameters"] = json!({});
        assert_eq!(h.send("POST", "/api/remediation/automation-rules", &admin, Some(no_values)).await.0, StatusCode::BAD_REQUEST);
        let (status, created) = h.send("POST", "/api/remediation/automation-rules", &admin, Some(rule)).await;
        assert_eq!(status, StatusCode::CREATED, "{created}");
        let rule_id = created["rule"]["id"].as_str().unwrap().to_owned();

        // Another metric, then the rule's: one run, read-only, sent at once.
        h.incident_opened(h.web1, "monitorflow_cpu_usage_ratio").await;
        h.incident_opened(h.web1, "monitorflow_disk_used_ratio").await;
        assert_eq!(automation::fire(&pool, None).await.unwrap(), 1);
        assert_eq!(automation::fire(&pool, None).await.unwrap(), 0, "an event is taken once");
        let commands = h.commands(h.web1).await;
        assert_eq!(commands.len(), 1);
        assert_eq!(commands[0].parameters[0].value, "2");

        // Within the cooldown: skipped, and said why.
        h.incident_opened(h.web1, "monitorflow_disk_used_ratio").await;
        assert_eq!(automation::fire(&pool, None).await.unwrap(), 0);
        let (_, detail) = h.send("GET", &format!("/api/remediation/automation-rules/{rule_id}"), &operator, None).await;
        let firings = detail["firings"].as_array().unwrap();
        assert_eq!(firings.len(), 2);
        assert!(firings[0]["skipped"].as_str().unwrap().contains("30 minutes"), "{detail}");
        assert!(firings[1]["run_id"].is_string());
        let (_, runs) = h.send("GET", &format!("/api/remediation/script-runs?rule_id={rule_id}"), &operator, None).await;
        assert_eq!(runs["runs"][0]["rule_name"], "Disk report");
        assert_eq!(runs["runs"][0]["requested_by"], Value::Null);

        // A rule's script cannot be deleted; the rule can.
        let script_uri = format!("/api/remediation/scripts/{script}");
        assert_eq!(h.send("DELETE", &script_uri, &admin, None).await.0, StatusCode::CONFLICT);
        let rule_uri = format!("/api/remediation/automation-rules/{rule_id}");
        assert_eq!(h.send("DELETE", &rule_uri, &operator, None).await.0, StatusCode::FORBIDDEN);
        assert_eq!(h.send("DELETE", &rule_uri, &admin, None).await.0, StatusCode::NO_CONTENT);
        assert_eq!(h.send("DELETE", &script_uri, &admin, None).await.0, StatusCode::NO_CONTENT);
    }

    #[sqlx::test]
    async fn the_ai_proposes_library_scripts_by_name(pool: PgPool) {
        let h = Harness::new(&pool).await;
        let admin = h.login("alice", Role::Administrator, GrantScope::Customer(h.customer)).await;
        h.send("POST", "/api/remediation/scripts", &admin, Some(h.script("disk", "df -h\n", "operator"))).await;
        let mut hidden = h.script("hidden", "df -h\n", "operator");
        hidden["ai_allowed"] = json!(false);
        h.send("POST", "/api/remediation/scripts", &admin, Some(hidden)).await;

        let offered = ai::offered(&pool, h.customer, Shell::Posix).await.unwrap();
        assert_eq!(offered.iter().map(|s| s.name.as_str()).collect::<Vec<_>>(), ["disk"]);
        assert!(ai::offered(&pool, h.customer, Shell::PowerShell).await.unwrap().is_empty());
        assert!(ai::describe(&offered).contains("- disk: "));

        let rules = RuleSet::default();
        let (pool_ref, rules_ref, customer) = (&pool, &rules, h.customer);
        let classify = move |command: &'static str| {
            ai::classify_command(pool_ref, customer, Shell::Posix, command, rules_ref)
        };
        let (decision, resolved) = classify("mf-script disk days=4").await.unwrap();
        assert_eq!(decision.tier, Tier::ReadOnly);
        assert_eq!(resolved.unwrap().values, [("days".to_owned(), "4".to_owned())]);
        for refused in ["mf-script hidden days=1", "mf-script disk", "mf-script nothing"] {
            let (decision, resolved) = classify(refused).await.unwrap();
            assert_eq!(decision.tier, Tier::Forbidden, "{refused}");
            assert!(resolved.is_none());
        }
        let (decision, resolved) = classify("df -h").await.unwrap();
        assert_eq!((decision.tier, resolved.is_none()), (Tier::ReadOnly, true));
    }
}
