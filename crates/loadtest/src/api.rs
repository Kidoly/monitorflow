//! The REST API, as the web interface uses it: a session cookie from a
//! login, then JSON.

use std::time::{Duration, Instant};

use anyhow::{bail, Context};
use serde_json::{json, Value};

#[derive(Clone)]
pub struct Api {
    base: String,
    client: reqwest::Client,
}

impl Api {
    pub async fn login(base: &str, username: &str, password: &str) -> anyhow::Result<Self> {
        let client = reqwest::Client::builder()
            .cookie_store(true)
            .timeout(Duration::from_secs(60))
            .build()?;
        let api = Self {
            base: base.to_owned(),
            client,
        };
        api.post(
            "/api/auth/login",
            &json!({"username": username, "password": password}),
        )
        .await
        .context("login")?;
        Ok(api)
    }

    pub async fn get(&self, path: &str) -> anyhow::Result<Value> {
        let response = self
            .client
            .get(format!("{}{path}", self.base))
            .send()
            .await?;
        Self::body(path, response).await
    }

    pub async fn post(&self, path: &str, body: &Value) -> anyhow::Result<Value> {
        let response = self
            .client
            .post(format!("{}{path}", self.base))
            .json(body)
            .send()
            .await?;
        Self::body(path, response).await
    }

    async fn body(path: &str, response: reqwest::Response) -> anyhow::Result<Value> {
        let status = response.status();
        let text = response.text().await?;
        if !status.is_success() {
            bail!("{path}: {status}: {text}");
        }
        if text.is_empty() {
            return Ok(Value::Null);
        }
        serde_json::from_str(&text).with_context(|| format!("{path}: not JSON"))
    }

    /// The ID of the customer everything is created under.
    pub async fn default_customer(&self) -> anyhow::Result<String> {
        let customers = self.get("/api/customers").await?;
        customers["customers"][0]["id"]
            .as_str()
            .map(str::to_owned)
            .context("no customer")
    }

    pub async fn create_site(&self, customer: &str, name: &str) -> anyhow::Result<String> {
        let site = self
            .post(
                &format!("/api/customers/{customer}/sites"),
                &json!({"name": name}),
            )
            .await?;
        site["id"]
            .as_str()
            .map(str::to_owned)
            .context("site without an ID")
    }

    /// A single-use token, valid a day.
    pub async fn token(
        &self,
        role: &str,
        peer_id: &str,
        site_id: &str,
        server_names: &[&str],
    ) -> anyhow::Result<String> {
        let created = self
            .post(
                "/api/tokens",
                &json!({
                    "role": role,
                    "peer_id": peer_id,
                    "site_id": site_id,
                    "ttl_s": 86_400,
                    "server_names": server_names,
                }),
            )
            .await?;
        created["token"]
            .as_str()
            .map(str::to_owned)
            .context("token missing from the response")
    }

    /// Servers the API lists online, following pages like the interface.
    pub async fn servers(&self) -> anyhow::Result<Vec<Value>> {
        let mut servers = Vec::new();
        let mut path = "/api/servers".to_owned();
        loop {
            let page = self.get(&path).await?;
            servers.extend(page["servers"].as_array().cloned().unwrap_or_default());
            match page["next_cursor"].as_str() {
                Some(cursor) => path = format!("/api/servers?cursor={cursor}"),
                None => return Ok(servers),
            }
        }
    }

    /// What the fleet overview loads: every server, and the open incidents.
    pub async fn fleet_page(&self) -> anyhow::Result<Duration> {
        let start = Instant::now();
        let (servers, incidents) = tokio::join!(
            self.servers(),
            self.get("/api/incidents?state=open&limit=500")
        );
        servers?;
        incidents?;
        Ok(start.elapsed())
    }

    /// What the incident list loads: sites, servers, the first page and
    /// the open incidents.
    pub async fn incident_page(&self) -> anyhow::Result<Duration> {
        let start = Instant::now();
        let (sites, servers, page, open) = tokio::join!(
            self.get("/api/sites"),
            self.servers(),
            self.get("/api/incidents?limit=50"),
            self.get("/api/incidents?state=open&limit=500"),
        );
        sites?;
        servers?;
        page?;
        open?;
        Ok(start.elapsed())
    }
}
