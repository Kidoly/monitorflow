//! Risk classifier administration through the API (ADR-0041): rules and
//! policies, who may read and change them, validation, audit, and the
//! rule set the classifier loads for a server. Ignored without
//! `DATABASE_URL`.

use axum::body::Body;
use axum::http::{Request, StatusCode};
use monitorflow_common::redact::Secret;
use monitorflow_server::api::auth::CSRF_HEADER;
use monitorflow_server::api::users::{self, GrantScope};
use monitorflow_server::api::{self, ApiSettings, ApiState, Role};
use monitorflow_server::grpc::Registry;
use monitorflow_server::remediation::risk::{self, store, Rule, Tier};
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
    /// Sites of `customer`.
    paris: Uuid,
    lyon: Uuid,
    /// A server in paris.
    server: Uuid,
    other_customer: Uuid,
    other_site: Uuid,
}

impl Harness {
    async fn new(pool: &PgPool) -> Self {
        let customer = Self::customer(pool, "acme").await;
        let other_customer = Self::customer(pool, "globex").await;
        let paris = Self::site(pool, customer, "paris").await;
        let lyon = Self::site(pool, customer, "lyon").await;
        let other_site = Self::site(pool, other_customer, "berlin").await;
        let server = sqlx::query_scalar(
            "INSERT INTO servers (peer_id, site_id, hostname, os_family)
             VALUES ('agent-1', $1, 'web-1', 'debian') RETURNING id",
        )
        .bind(paris)
        .fetch_one(pool)
        .await
        .unwrap();
        Self {
            pool: pool.clone(),
            customer,
            paris,
            lyon,
            server,
            other_customer,
            other_site,
        }
    }

    async fn customer(pool: &PgPool, name: &str) -> Uuid {
        sqlx::query_scalar("INSERT INTO customers (name) VALUES ($1) RETURNING id")
            .bind(name)
            .fetch_one(pool)
            .await
            .unwrap()
    }

    async fn site(pool: &PgPool, customer: Uuid, name: &str) -> Uuid {
        sqlx::query_scalar("INSERT INTO sites (name, customer_id) VALUES ($1, $2) RETURNING id")
            .bind(name)
            .bind(customer)
            .fetch_one(pool)
            .await
            .unwrap()
    }

    fn state(&self) -> ApiState {
        ApiState::new(
            self.pool.clone(),
            Registry::default(),
            ApiSettings::default(),
        )
    }

    async fn send(
        &self,
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
        let response = api::router(self.state()).oneshot(request).await.unwrap();
        let status = response.status();
        let bytes = axum::body::to_bytes(response.into_body(), 1 << 20)
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
        let body = json!({ "username": name, "password": PASSWORD }).to_string();
        let request = Request::post("/api/auth/login")
            .header(CSRF_HEADER, "1")
            .header("content-type", "application/json")
            .body(Body::from(body))
            .unwrap();
        let response = api::router(self.state()).oneshot(request).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        response.headers()["set-cookie"]
            .to_str()
            .unwrap()
            .split(';')
            .next()
            .unwrap()
            .to_owned()
    }

    async fn admin(&self) -> String {
        self.login(
            "admin",
            Role::Administrator,
            GrantScope::Customer(self.customer),
        )
        .await
    }

    async fn audit(&self, action: &str) -> Vec<(String, Value)> {
        sqlx::query_as("SELECT target_id, details FROM audit_log WHERE action = $1 ORDER BY id")
            .bind(action)
            .fetch_all(&self.pool)
            .await
            .unwrap()
    }
}

fn keys(value: &Value) -> Vec<&str> {
    let mut keys: Vec<&str> = value
        .as_object()
        .unwrap()
        .keys()
        .map(String::as_str)
        .collect();
    keys.sort_unstable();
    keys
}

db_tests! {
    #[sqlx::test]
    async fn rules_are_created_listed_deleted_and_audited(pool: PgPool) {
        let h = Harness::new(&pool).await;
        let admin = h.admin().await;
        let (status, rule) = h
            .send("POST", "/api/remediation/rules", &admin, Some(json!({
                "customer_id": h.customer, "kind": "deny", "pattern": " rm   -rf  / ",
                "description": "never"
            })))
            .await;
        assert_eq!(status, StatusCode::CREATED, "{rule}");
        assert_eq!(
            keys(&rule),
            ["created_at", "created_by", "customer_id", "description", "id", "kind", "pattern",
             "site_id", "tier"]
        );
        assert_eq!(rule["pattern"], "rm -rf /");
        assert_eq!(rule["kind"], "deny");
        assert_eq!(rule["tier"], Value::Null);
        assert_eq!(rule["site_id"], Value::Null);
        assert_eq!(rule["description"], "never");
        let (status, allow) = h
            .send("POST", "/api/remediation/rules", &admin, Some(json!({
                "customer_id": h.customer, "site_id": h.paris, "kind": "allow",
                "pattern": "nginx -t", "tier": "read_only"
            })))
            .await;
        assert_eq!(status, StatusCode::CREATED, "{allow}");
        assert_eq!(allow["tier"], "read_only");
        let (status, cache) = h
            .send("POST", "/api/remediation/rules", &admin, Some(json!({
                "customer_id": h.customer, "kind": "cache_dir", "pattern": "/var/cache/nginx/"
            })))
            .await;
        assert_eq!(status, StatusCode::CREATED, "{cache}");
        assert_eq!(cache["pattern"], "/var/cache/nginx");

        let (status, list) = h.send("GET", "/api/remediation/rules", &admin, None).await;
        assert_eq!(status, StatusCode::OK);
        let kinds: Vec<&str> = list["rules"].as_array().unwrap().iter()
            .map(|r| r["kind"].as_str().unwrap()).collect();
        // Customer-wide rules first, then by kind.
        assert_eq!(kinds, ["cache_dir", "deny", "allow"]);
        let uri = format!("/api/remediation/rules?site_id={}", h.paris);
        let (_, list) = h.send("GET", &uri, &admin, None).await;
        assert_eq!(list["rules"].as_array().unwrap().len(), 1);

        let uri = format!("/api/remediation/rules/{}", rule["id"].as_str().unwrap());
        let (status, _) = h.send("DELETE", &uri, &admin, None).await;
        assert_eq!(status, StatusCode::NO_CONTENT);
        let (status, _) = h.send("DELETE", &uri, &admin, None).await;
        assert_eq!(status, StatusCode::NOT_FOUND);
        let (status, _) = h.send("DELETE", "/api/remediation/rules/not-a-uuid", &admin, None).await;
        assert_eq!(status, StatusCode::NOT_FOUND);

        let created = h.audit("remediation_rule.create").await;
        assert_eq!(created.len(), 3);
        let deleted = h.audit("remediation_rule.delete").await;
        assert_eq!(deleted.len(), 1);
        assert_eq!(deleted[0].0, rule["id"].as_str().unwrap());
        assert_eq!(deleted[0].1["pattern"], "rm -rf /");
        assert_eq!(deleted[0].1["kind"], "deny");
    }

    #[sqlx::test]
    async fn rules_are_scoped_to_customer_administrators(pool: PgPool) {
        let h = Harness::new(&pool).await;
        let admin = h.admin().await;
        let body = json!({"customer_id": h.customer, "kind": "deny", "pattern": "reboot"});
        let (_, rule) = h.send("POST", "/api/remediation/rules", &admin, Some(body.clone())).await;
        let uri = format!("/api/remediation/rules/{}", rule["id"].as_str().unwrap());

        // A viewer of the customer reads but may not change.
        let viewer = h.login("viewer", Role::Viewer, GrantScope::Customer(h.customer)).await;
        let (status, list) = h.send("GET", "/api/remediation/rules", &viewer, None).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(list["rules"].as_array().unwrap().len(), 1);
        let (status, _) = h.send("POST", "/api/remediation/rules", &viewer, Some(body.clone())).await;
        assert_eq!(status, StatusCode::FORBIDDEN);
        let (status, _) = h.send("DELETE", &uri, &viewer, None).await;
        assert_eq!(status, StatusCode::FORBIDDEN);

        // Another customer's administrator does not see it.
        let other = h.login("other", Role::Administrator, GrantScope::Customer(h.other_customer)).await;
        let (_, list) = h.send("GET", "/api/remediation/rules", &other, None).await;
        assert_eq!(list["rules"], json!([]));
        let (status, _) = h.send("POST", "/api/remediation/rules", &other, Some(body.clone())).await;
        assert_eq!(status, StatusCode::NOT_FOUND);
        let (status, _) = h.send("DELETE", &uri, &other, None).await;
        assert_eq!(status, StatusCode::NOT_FOUND);

        // A site administrator does not administer the customer.
        let site_admin = h.login("siteadmin", Role::Administrator, GrantScope::Site(h.paris)).await;
        let (status, _) = h.send("POST", "/api/remediation/rules", &site_admin, Some(body)).await;
        assert_eq!(status, StatusCode::NOT_FOUND);
        let (_, list) = h.send("GET", "/api/remediation/rules", &site_admin, None).await;
        assert_eq!(list["rules"], json!([]));

        // No session at all.
        let (status, _) = h.send("GET", "/api/remediation/rules", "", None).await;
        assert_eq!(status, StatusCode::UNAUTHORIZED);
    }

    #[sqlx::test]
    async fn invalid_and_duplicate_rules_are_refused(pool: PgPool) {
        let h = Harness::new(&pool).await;
        let admin = h.admin().await;
        let c = h.customer;
        for (body, message) in [
            (json!({"customer_id": c, "kind": "allow", "pattern": "nginx -t"}),
             "allow rules need a tier of read_only or low_impact"),
            (json!({"customer_id": c, "kind": "allow", "pattern": "nginx -t", "tier": "high_impact"}),
             "allow rules need a tier of read_only or low_impact"),
            (json!({"customer_id": c, "kind": "deny", "pattern": "x", "tier": "read_only"}),
             "only allow rules have a tier"),
            (json!({"customer_id": c, "kind": "allow", "pattern": "sudo **", "tier": "read_only"}),
             "built-in rules decide `sudo`; allow rules only extend to unknown commands"),
            (json!({"customer_id": c, "kind": "deny", "pattern": "  "}), "pattern must not be empty"),
            (json!({"customer_id": c, "kind": "deny", "pattern": "rм"}), "pattern must be printable ASCII"),
            (json!({"customer_id": c, "kind": "cache_dir", "pattern": "/"}),
             "cache_dir must be an absolute directory of two components or more, without wildcards, `.` or `..`, outside system directories"),
            (json!({"customer_id": c, "site_id": h.other_site, "kind": "deny", "pattern": "x"}),
             "unknown site"),
        ] {
            let (status, answer) = h.send("POST", "/api/remediation/rules", &admin, Some(body.clone())).await;
            assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
            assert_eq!(answer["error"], message, "{body}");
        }
        let (status, _) = h
            .send("POST", "/api/remediation/rules", &admin, Some(json!({"customer_id": c, "kind": "nope", "pattern": "x"})))
            .await;
        assert_eq!(status, StatusCode::BAD_REQUEST);

        let body = json!({"customer_id": c, "kind": "deny", "pattern": "reboot"});
        let (status, _) = h.send("POST", "/api/remediation/rules", &admin, Some(body)).await;
        assert_eq!(status, StatusCode::CREATED);
        let again = json!({"customer_id": c, "kind": "deny", "pattern": " reboot "});
        let (status, answer) = h.send("POST", "/api/remediation/rules", &admin, Some(again)).await;
        assert_eq!(status, StatusCode::CONFLICT);
        assert_eq!(answer["error"], "this rule already exists");
        // The same pattern for one site is another rule.
        let site = json!({"customer_id": c, "site_id": h.paris, "kind": "deny", "pattern": "reboot"});
        let (status, _) = h.send("POST", "/api/remediation/rules", &admin, Some(site)).await;
        assert_eq!(status, StatusCode::CREATED);
        assert_eq!(h.audit("remediation_rule.create").await.len(), 2);
    }

    #[sqlx::test]
    async fn policies_override_per_site_and_are_audited(pool: PgPool) {
        let h = Harness::new(&pool).await;
        let admin = h.admin().await;
        assert!(!store::low_impact_auto(&pool, h.customer, Some(h.paris)).await.unwrap());

        let body = json!({"customer_id": h.customer, "low_impact_auto": true});
        let (status, policy) = h.send("PUT", "/api/remediation/policies", &admin, Some(body)).await;
        assert_eq!(status, StatusCode::OK, "{policy}");
        assert_eq!(
            keys(&policy),
            ["customer_id", "low_impact_auto", "site_id", "updated_at", "updated_by"]
        );
        let body = json!({"customer_id": h.customer, "site_id": h.paris, "low_impact_auto": false});
        let (status, _) = h.send("PUT", "/api/remediation/policies", &admin, Some(body)).await;
        assert_eq!(status, StatusCode::OK);
        assert!(!store::low_impact_auto(&pool, h.customer, Some(h.paris)).await.unwrap());
        assert!(store::low_impact_auto(&pool, h.customer, Some(h.lyon)).await.unwrap());
        assert!(store::low_impact_auto(&pool, h.customer, None).await.unwrap());

        // Replacing a row keeps one per scope.
        let body = json!({"customer_id": h.customer, "site_id": h.paris, "low_impact_auto": true});
        h.send("PUT", "/api/remediation/policies", &admin, Some(body)).await;
        assert!(store::low_impact_auto(&pool, h.customer, Some(h.paris)).await.unwrap());
        let (status, list) = h.send("GET", "/api/remediation/policies", &admin, None).await;
        assert_eq!(status, StatusCode::OK);
        let sites: Vec<Value> = list["policies"].as_array().unwrap().iter()
            .map(|p| p["site_id"].clone()).collect();
        assert_eq!(sites, [Value::Null, json!(h.paris)]);
        let audit = h.audit("remediation_policy.update").await;
        assert_eq!(audit.len(), 3);
        assert_eq!(audit[1].0, h.paris.to_string());
        assert_eq!(audit[1].1["low_impact_auto"], false);

        let viewer = h.login("viewer", Role::Viewer, GrantScope::Customer(h.customer)).await;
        let body = json!({"customer_id": h.customer, "low_impact_auto": false});
        let (status, _) = h.send("PUT", "/api/remediation/policies", &viewer, Some(body)).await;
        assert_eq!(status, StatusCode::FORBIDDEN);
        let body = json!({"customer_id": h.customer, "site_id": h.other_site, "low_impact_auto": true});
        let (status, _) = h.send("PUT", "/api/remediation/policies", &admin, Some(body)).await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        let other = h.login("other", Role::Administrator, GrantScope::Customer(h.other_customer)).await;
        let (_, list) = h.send("GET", "/api/remediation/policies", &other, None).await;
        assert_eq!(list["policies"], json!([]));
    }

    #[sqlx::test]
    async fn the_classifier_uses_the_rules_of_the_server_site(pool: PgPool) {
        let h = Harness::new(&pool).await;
        let admin = h.admin().await;
        let mut ids = Vec::new();
        for body in [
            json!({"customer_id": h.customer, "kind": "deny", "pattern": "systemctl stop postgresql*"}),
            json!({"customer_id": h.customer, "site_id": h.paris, "kind": "allow",
                   "pattern": "nginx -t", "tier": "read_only"}),
            json!({"customer_id": h.customer, "site_id": h.lyon, "kind": "cache_dir",
                   "pattern": "/var/cache/nginx"}),
            json!({"customer_id": h.other_customer, "kind": "deny", "pattern": "nginx **"}),
        ] {
            let (status, rule) = h.send("POST", "/api/remediation/rules", &admin, Some(body)).await;
            ids.push((status, rule["id"].as_str().map(|s| s.parse::<Uuid>().unwrap())));
        }
        // The last one was another customer's: refused.
        assert_eq!(ids[3].0, StatusCode::NOT_FOUND);
        let (deny, allow) = (ids[0].1.unwrap(), ids[1].1.unwrap());

        let rules = store::rule_set_for_server(&pool, h.server).await.unwrap().unwrap();
        assert_eq!((rules.deny.len(), rules.allow.len(), rules.cache_dirs.len()), (1, 1, 0));
        let decision = risk::classify("nginx -t", &rules);
        assert_eq!((decision.tier, decision.rule), (Tier::ReadOnly, Rule::Configured { id: allow }));
        let decision = risk::classify("df -h; systemctl stop postgresql", &rules);
        assert_eq!((decision.tier, decision.rule), (Tier::Forbidden, Rule::Configured { id: deny }));
        assert!(store::rule_set_for_server(&pool, Uuid::new_v4()).await.unwrap().is_none());

        // The dry run: site rules apply only with the site.
        let classify = |site: Option<Uuid>, command: &str| {
            json!({"customer_id": h.customer, "site_id": site, "command": command})
        };
        let (status, answer) = h
            .send("POST", "/api/remediation/classify", &admin, Some(classify(Some(h.paris), "nginx -t")))
            .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(answer["tier"], "read_only");
        assert_eq!(answer["rule"], json!({"source": "configured", "id": allow}));
        let (_, answer) = h
            .send("POST", "/api/remediation/classify", &admin, Some(classify(None, "nginx -t")))
            .await;
        assert_eq!(answer["tier"], "high_impact");
        assert_eq!(answer["rule"], json!({"source": "builtin", "id": "default.unknown"}));
        let (_, answer) = h
            .send("POST", "/api/remediation/classify", &admin,
                  Some(classify(Some(h.lyon), "find /var/cache/nginx -mindepth 1 -delete")))
            .await;
        assert_eq!(answer["tier"], "low_impact");
        let viewer = h.login("viewer", Role::Viewer, GrantScope::Customer(h.customer)).await;
        let (status, answer) = h
            .send("POST", "/api/remediation/classify", &viewer, Some(classify(None, "df -h")))
            .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(answer["tier"], "read_only");
        let other = h.login("other", Role::Administrator, GrantScope::Customer(h.other_customer)).await;
        let (status, _) = h
            .send("POST", "/api/remediation/classify", &other, Some(classify(None, "df -h")))
            .await;
        assert_eq!(status, StatusCode::NOT_FOUND);
    }
}
