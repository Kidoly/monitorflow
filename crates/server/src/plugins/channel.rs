//! Notification channels run by a plugin (ADR-0103). The dispatcher hands
//! a plugin channel's messages here: the plugin must still be turned on
//! and allowed for what the channel covers, its settings are checked again
//! against its current manifest, and it runs with its own settings and
//! secret settings only, reaching public addresses only.

use std::collections::{BTreeMap, HashMap};
use std::net::IpAddr;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use monitorflow_plugin::http::{Checked, Http, ReqwestHttp, Response};
use monitorflow_plugin::package::Package;
use monitorflow_plugin::{limits, Plugin, Runner};
use serde_json::json;
use sqlx::PgPool;

use crate::notify::message::Message;
use crate::notify::send::{is_public, public_http_client, SendError};
use crate::notify::{Channel, Destination};

/// Modules compiled from installed packages, by SHA-256.
type Compiled = Mutex<HashMap<Vec<u8>, Arc<Plugin>>>;

/// Most modules kept compiled; past it the cache starts over.
const MAX_COMPILED: usize = 32;

/// HTTP for plugins on the central server: HTTPS only, since what they
/// send carries their secret settings across the Internet, and
/// [`public_http_client`], whose resolver connects to public addresses
/// only, with a check of addresses written in the URL, which never reach
/// the resolver.
struct PublicOnly(ReqwestHttp);

impl Http for PublicOnly {
    fn send(&self, request: Checked, timeout: Duration) -> Result<Response, String> {
        if request.url.scheme() != "https" {
            return Err("plugins on the central server connect over HTTPS only".into());
        }
        let literal = request
            .url
            .host_str()
            .and_then(|h| h.trim_matches(['[', ']']).parse::<IpAddr>().ok());
        if let Some(ip) = literal.filter(|ip| !is_public(*ip)) {
            return Err(format!(
                "{ip} is a private address; plugins on the central server reach public addresses only"
            ));
        }
        self.0.send(request, timeout)
    }
}

fn runner() -> &'static Runner {
    static RUNNER: OnceLock<Runner> = OnceLock::new();
    RUNNER.get_or_init(|| {
        let http = ReqwestHttp::new(public_http_client(), tokio::runtime::Handle::current());
        Runner::new(Arc::new(PublicOnly(http)))
    })
}

fn compiled() -> &'static Compiled {
    static COMPILED: OnceLock<Compiled> = OnceLock::new();
    COMPILED.get_or_init(Compiled::default)
}

fn database(error: sqlx::Error) -> SendError {
    tracing::warn!(%error, "could not read a plugin");
    SendError::new("Could not read the plugin; it will be tried again.")
}

/// The module of the installed package `sha256`, compiled once.
async fn module(pool: &PgPool, sha256: &[u8]) -> Result<Arc<Plugin>, SendError> {
    if let Some(plugin) = compiled().lock().expect("not poisoned").get(sha256) {
        return Ok(plugin.clone());
    }
    let package: Vec<u8> = sqlx::query_scalar("SELECT package FROM plugins WHERE sha256 = $1")
        .bind(sha256)
        .fetch_one(pool)
        .await
        .map_err(database)?;
    // Verified when it was installed; read again, not trusted again.
    let plugin = tokio::task::spawn_blocking(move || {
        let package = Package::parse(&package).map_err(|e| e.to_string())?;
        let module =
            base64::Engine::decode(&base64::engine::general_purpose::STANDARD, &package.module)
                .map_err(|e| e.to_string())?;
        Plugin::load(&module).map_err(|e| e.to_string())
    })
    .await
    .map_err(|e| SendError::new(e.to_string()))?
    .map_err(|e| SendError::new(format!("The plugin cannot run: {e}")))?;
    let plugin = Arc::new(plugin);
    let mut cache = compiled().lock().expect("not poisoned");
    if cache.len() >= MAX_COMPILED {
        cache.clear();
    }
    cache.insert(sha256.to_vec(), plugin.clone());
    Ok(plugin)
}

/// The secret settings stored for a channel: a JSON object of strings.
pub fn secrets(secret: Option<&str>) -> Result<BTreeMap<String, String>, String> {
    match secret {
        None => Ok(BTreeMap::new()),
        Some(text) => serde_json::from_str(text)
            .map_err(|_| "the secret settings are not a JSON object of strings".into()),
    }
}

/// Sends `message` through the plugin of `channel`.
pub async fn deliver(pool: &PgPool, channel: &Channel, message: &Message) -> Result<(), SendError> {
    let Destination::Plugin(destination) = &channel.destination else {
        return Err(SendError::new("not a plugin channel"));
    };
    let name = &destination.plugin;
    let (customer, site) = channel.coverage.columns();
    let installed = super::usable(pool, name, customer, site)
        .await
        .map_err(database)?
        .ok_or_else(|| {
            SendError::new(format!(
                "The {name} plugin is turned off, removed, or not allowed for what this channel covers."
            ))
        })?;
    let mut values = destination.values.clone();
    let stored = secrets(channel.secret.as_ref().map(|s| s.expose())).map_err(SendError::new)?;
    values.extend(stored);
    let values = installed
        .manifest
        .values(&values)
        .map_err(|e| SendError::new(format!("Edit the channel: {e}")))?;
    let plugin = module(pool, &installed.sha256).await?;
    let input = json!({
        "settings": values.settings,
        "secrets": values.secrets,
        "message": {
            "event": message.payload["event"],
            "text": message.text,
            "url": message.url,
            "lines": message.lines,
            "payload": message.payload,
        },
    });
    let rules = installed.manifest.host_rules(&values.settings);
    let hidden = values.secrets.into_values().collect();
    runner()
        .run(name, plugin, input, rules, hidden, limits::CHANNEL_TIMEOUT)
        .await
        .map(drop)
        .map_err(|e| SendError::new(e.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plain_http_and_literal_private_addresses_are_refused_before_any_connection() {
        let runtime = tokio::runtime::Runtime::new().unwrap();
        let http = PublicOnly(ReqwestHttp::new(
            public_http_client(),
            runtime.handle().clone(),
        ));
        let send = |url: &str| {
            let request = Checked {
                method: reqwest::Method::GET,
                url: reqwest::Url::parse(url).unwrap(),
                headers: Vec::new(),
                body: String::new(),
            };
            http.send(request, Duration::from_secs(1)).unwrap_err()
        };
        assert!(send("http://ntfy.example/").contains("HTTPS only"));
        for url in [
            "https://127.0.0.1/",
            "https://10.1.2.3:8080/",
            "https://[::1]/",
            "https://169.254.169.254/",
        ] {
            let error = send(url);
            assert!(error.contains("private address"), "{url}: {error}");
        }
    }

    #[test]
    fn secrets_are_an_object_of_strings() {
        assert_eq!(secrets(None).unwrap(), BTreeMap::new());
        assert_eq!(secrets(Some(r#"{"token": "t"}"#)).unwrap()["token"], "t");
        assert!(secrets(Some(r#"{"token": 1}"#)).is_err());
        assert!(secrets(Some("plain")).is_err());
    }
}
