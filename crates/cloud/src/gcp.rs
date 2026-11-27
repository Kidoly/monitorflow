//! Google Cloud: the Compute Engine instances of one project and Cloud
//! Monitoring's metrics, read with a service account's JSON key. A JWT
//! signed with RS256 on `ring` is exchanged for an OAuth access token
//! (RFC 7523), kept until a minute before it expires.
//!
//! Calls, all read-only (the roles Compute Viewer and Monitoring
//! Viewer): `compute.instances.list` through `instances.aggregatedList`,
//! and `monitoring.timeSeries.list`, once per metric for the whole
//! project.

use std::collections::HashMap;
use std::time::Duration;

use base64::engine::general_purpose::{STANDARD, URL_SAFE_NO_PAD};
use base64::Engine;
use chrono::{DateTime, Utc};
use monitorflow_sensors::virt::Vm;
use ring::rand::SystemRandom;
use ring::signature::{RsaKeyPair, RSA_PKCS1_SHA256};
use serde_json::{json, Value};
use tokio::sync::Mutex;
use tokio::time::Instant;

use crate::http::{bounded, form, is_local, Failure, Http};
use crate::CloudVm;

/// Where tokens come from, and the audience of every assertion.
const TOKEN: &str = "https://oauth2.googleapis.com/token";
const COMPUTE: &str = "https://compute.googleapis.com";
const MONITORING: &str = "https://monitoring.googleapis.com";
/// Read-only scopes: nothing the key can do goes beyond reading.
const SCOPES: &str = "https://www.googleapis.com/auth/compute.readonly https://www.googleapis.com/auth/monitoring.read";
const GRANT: &str = "urn:ietf:params:oauth:grant-type:jwt-bearer";
/// How long an assertion is valid, Google's maximum.
const ASSERTION_SECONDS: i64 = 3600;
/// Longest JSON key; Google's are about 2.4 KiB.
pub const MAX_KEY: usize = 16 << 10;
/// Pages read at most per list.
const MAX_PAGES: usize = 50;
/// Instances per page of `aggregatedList`, its own limit.
const PAGE: usize = 500;
/// How far back metrics are asked for, and their alignment: Compute
/// Engine samples every minute and shows a point up to 4 minutes late.
const WINDOW: i64 = 20 * 60;
const ALIGNMENT: &str = "300s";

/// What Cloud Monitoring is asked for the whole project: its metric
/// type, how each series is aligned and summed per instance, and the
/// measure it becomes. Utilization is a ratio already; byte counts
/// aligned as a rate are bytes per second.
const METRICS: [(&str, &str, &str, &str); 5] = [
    (
        "compute.googleapis.com/instance/cpu/utilization",
        "ALIGN_MEAN",
        "REDUCE_MEAN",
        "cpu_usage_ratio",
    ),
    (
        "compute.googleapis.com/instance/network/received_bytes_count",
        "ALIGN_RATE",
        "REDUCE_SUM",
        "network_in_bytes_per_second",
    ),
    (
        "compute.googleapis.com/instance/network/sent_bytes_count",
        "ALIGN_RATE",
        "REDUCE_SUM",
        "network_out_bytes_per_second",
    ),
    (
        "compute.googleapis.com/instance/disk/read_bytes_count",
        "ALIGN_RATE",
        "REDUCE_SUM",
        "disk_read_bytes_per_second",
    ),
    (
        "compute.googleapis.com/instance/disk/write_bytes_count",
        "ALIGN_RATE",
        "REDUCE_SUM",
        "disk_write_bytes_per_second",
    ),
];

/// What a service account's JSON key holds that a check needs.
#[derive(Clone, PartialEq, Eq)]
pub struct Key {
    /// `client_email`: the service account.
    pub email: String,
    /// `private_key_id`, sent as the JWT's `kid`.
    pub key_id: Option<String>,
    /// `project_id`: the project the account belongs to.
    pub project: Option<String>,
    /// The private key, PKCS#8 DER.
    pub der: Vec<u8>,
}

impl std::fmt::Debug for Key {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Key")
            .field("email", &self.email)
            .field("project", &self.project)
            .finish_non_exhaustive()
    }
}

/// Reads a service account's JSON key as Google gives it. Errors never
/// quote the key.
pub fn read_key(text: &str) -> Result<Key, String> {
    if text.len() > MAX_KEY {
        return Err(format!("the key must be at most {MAX_KEY} bytes"));
    }
    let bad = || "the key must be a service account's JSON key file".to_string();
    let value: Value = serde_json::from_str(text).map_err(|_| bad())?;
    if value["type"].as_str() != Some("service_account") {
        return Err(bad());
    }
    let email = value["client_email"]
        .as_str()
        .map(str::trim)
        .filter(|e| valid_email(e))
        .ok_or("the key has no valid `client_email`")?
        .to_ascii_lowercase();
    let key_id = value["private_key_id"]
        .as_str()
        .filter(|k| !k.is_empty() && k.len() <= 64 && k.bytes().all(|b| b.is_ascii_hexdigit()))
        .map(str::to_owned);
    let project = value["project_id"]
        .as_str()
        .filter(|p| valid_project(p))
        .map(str::to_owned);
    let pem = value["private_key"]
        .as_str()
        .ok_or("the key has no `private_key`")?;
    let der = pkcs8(pem).ok_or("the key's `private_key` is not an RSA private key")?;
    RsaKeyPair::from_pkcs8(&der)
        .map_err(|_| "the key's `private_key` is not an RSA private key")?;
    Ok(Key {
        email,
        key_id,
        project,
        der,
    })
}

/// The DER inside a `PRIVATE KEY` PEM block.
fn pkcs8(pem: &str) -> Option<Vec<u8>> {
    let body = pem
        .trim()
        .strip_prefix("-----BEGIN PRIVATE KEY-----")?
        .strip_suffix("-----END PRIVATE KEY-----")?;
    let text: String = body.chars().filter(|c| !c.is_whitespace()).collect();
    STANDARD.decode(text).ok()
}

/// A service account's address, like
/// `reader@my-project.iam.gserviceaccount.com`.
pub fn valid_email(text: &str) -> bool {
    let Some((user, domain)) = text.split_once('@') else {
        return false;
    };
    text.len() <= 254
        && !user.is_empty()
        && domain.contains('.')
        && text
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'@' | b'.' | b'-' | b'_' | b'+'))
}

/// A project ID: 6 to 30 lower-case letters, digits and hyphens, from a
/// letter, not ending with a hyphen.
pub fn valid_project(text: &str) -> bool {
    (6..=30).contains(&text.len())
        && text.starts_with(|c: char| c.is_ascii_lowercase())
        && !text.ends_with('-')
        && text
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
}

/// A project read with one service account.
pub struct Client {
    http: Http,
    email: String,
    key_id: Option<String>,
    key: RsaKeyPair,
    project: String,
    /// `http://127.0.0.1:port` for tests: every service answers there.
    base: Option<String>,
    /// The access token and when to stop using it.
    token: Mutex<Option<(String, Instant)>>,
}

impl Client {
    /// A client for `project` with `key`. A `base` other than a test
    /// server on this machine is ignored: Google's endpoints are used.
    pub fn new(
        http: Http,
        key: &Key,
        project: String,
        base: Option<String>,
    ) -> Result<Self, String> {
        let pair = RsaKeyPair::from_pkcs8(&key.der)
            .map_err(|_| "the key's `private_key` is not an RSA private key".to_string())?;
        Ok(Self {
            http,
            email: key.email.clone(),
            key_id: key.key_id.clone(),
            key: pair,
            project,
            base: base.filter(|b| is_local(b)),
            token: Mutex::new(None),
        })
    }

    /// The token, Compute Engine and Cloud Monitoring endpoints.
    fn endpoints(&self) -> (String, &str, &str) {
        match &self.base {
            // A test server on this machine is the only endpoint other
            // than Google's, and the only one reached without TLS.
            Some(base) => (format!("{base}/token"), base, base),
            None => (TOKEN.into(), COMPUTE, MONITORING),
        }
    }

    /// The signed assertion asking for a token at `now` (Unix seconds).
    pub fn assertion(&self, now: i64) -> Result<String, Failure> {
        let mut header = json!({"alg": "RS256", "typ": "JWT"});
        if let Some(kid) = &self.key_id {
            header["kid"] = json!(kid);
        }
        let claims = json!({
            "iss": self.email, "scope": SCOPES, "aud": TOKEN,
            "iat": now, "exp": now + ASSERTION_SECONDS,
        });
        let input = format!(
            "{}.{}",
            URL_SAFE_NO_PAD.encode(header.to_string()),
            URL_SAFE_NO_PAD.encode(claims.to_string())
        );
        let mut signature = vec![0; self.key.public().modulus_len()];
        self.key
            .sign(
                &RSA_PKCS1_SHA256,
                &SystemRandom::new(),
                input.as_bytes(),
                &mut signature,
            )
            .map_err(|_| Failure::Login("the key cannot sign".into()))?;
        Ok(format!("{input}.{}", URL_SAFE_NO_PAD.encode(signature)))
    }

    /// An access token, kept until a minute before it expires.
    async fn token(&self) -> Result<String, Failure> {
        let mut kept = self.token.lock().await;
        if let Some((token, until)) = kept.as_ref() {
            if Instant::now() < *until {
                return Ok(token.clone());
            }
        }
        let body = form(&[
            ("grant_type".into(), GRANT.into()),
            ("assertion".into(), self.assertion(Utc::now().timestamp())?),
        ]);
        let request = self
            .http
            .client()
            .post(self.endpoints().0)
            .header("Content-Type", "application/x-www-form-urlencoded")
            .body(body);
        let (status, bytes) = self.http.send(request).await?;
        let answer: Value = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
        let token = answer["access_token"]
            .as_str()
            .filter(|t| status.is_success() && !t.is_empty());
        let Some(token) = token else {
            let error = answer["error"].as_str().unwrap_or("refused");
            let description = answer["error_description"]
                .as_str()
                .and_then(|d| d.lines().next())
                .unwrap_or_default();
            let message = bounded(&format!("{error}: {description}"));
            return Err(if status.is_server_error() {
                Failure::Api(message)
            } else {
                Failure::Login(message)
            });
        };
        let lasts = answer["expires_in"]
            .as_u64()
            .unwrap_or(3600)
            .clamp(120, 86_400);
        let until = Instant::now() + Duration::from_secs(lasts - 60);
        *kept = Some((token.to_owned(), until));
        Ok(token.to_owned())
    }

    /// One `GET` with the token. A refused token is forgotten.
    async fn get(&self, url: &str) -> Result<Value, Failure> {
        let token = self.token().await?;
        let request = self.http.client().get(url).bearer_auth(token);
        let (status, bytes) = self.http.send(request).await?;
        let answer: Value = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
        if status.is_success() {
            return Ok(answer);
        }
        let failure = refusal(status.as_u16(), &answer);
        if matches!(failure, Failure::Login(_)) {
            *self.token.lock().await = None;
        }
        Err(failure)
    }

    /// Every page of a list, following `nextPageToken` on the same URL.
    async fn pages(
        &self,
        url: &str,
        params: &[(&str, String)],
        mut each: impl FnMut(Value) -> bool,
    ) -> Result<(), Failure> {
        let mut next: Option<String> = None;
        for _ in 0..MAX_PAGES {
            let mut query: Vec<(&str, &str)> =
                params.iter().map(|(k, v)| (*k, v.as_str())).collect();
            if let Some(token) = &next {
                query.push(("pageToken", token));
            }
            let url = reqwest::Url::parse_with_params(url, &query)
                .map_err(|_| Failure::Api("the address does not read".into()))?;
            let mut page = self.get(url.as_str()).await?;
            next = page["nextPageToken"]
                .as_str()
                .filter(|t| !t.is_empty())
                .map(str::to_owned);
            let more = each(page.take());
            if !more || next.is_none() {
                break;
            }
        }
        Ok(())
    }

    /// The project's instances in every zone, at most `max`.
    pub async fn instances(&self, max: usize) -> Result<Vec<Instance>, Failure> {
        let url = format!(
            "{}/compute/v1/projects/{}/aggregated/instances",
            self.endpoints().1,
            self.project
        );
        let params = [
            ("maxResults", PAGE.to_string()),
            ("returnPartialSuccess", "true".into()),
        ];
        let mut out = Vec::new();
        self.pages(&url, &params, |page| {
            let scopes = page["items"].as_object().into_iter().flatten();
            for instance in
                scopes.flat_map(|(_, scope)| scope["instances"].as_array().into_iter().flatten())
            {
                if out.len() >= max {
                    return false;
                }
                if let Some(instance) = Instance::read(instance, &self.project) {
                    out.push(instance);
                }
            }
            out.len() < max
        })
        .await?;
        Ok(out)
    }

    /// The newest value of each metric of every instance of the project,
    /// by instance ID. The first failure is returned with what came.
    pub async fn metrics(
        &self,
        now: DateTime<Utc>,
    ) -> (HashMap<String, Vec<(&'static str, f64)>>, Option<Failure>) {
        let mut out: HashMap<String, Vec<(&'static str, f64)>> = HashMap::new();
        let url = format!(
            "{}/v3/projects/{}/timeSeries",
            self.endpoints().2,
            self.project
        );
        let start = (now - chrono::Duration::seconds(WINDOW)).format("%Y-%m-%dT%H:%M:%SZ");
        let end = now.format("%Y-%m-%dT%H:%M:%SZ");
        for (metric, aligner, reducer, measure) in METRICS {
            let params = [
                (
                    "filter",
                    format!(r#"metric.type = "{metric}" AND resource.type = "gce_instance""#),
                ),
                ("interval.startTime", start.to_string()),
                ("interval.endTime", end.to_string()),
                ("aggregation.alignmentPeriod", ALIGNMENT.into()),
                ("aggregation.perSeriesAligner", aligner.into()),
                ("aggregation.crossSeriesReducer", reducer.into()),
                (
                    "aggregation.groupByFields",
                    "resource.label.instance_id".into(),
                ),
                ("view", "FULL".into()),
            ];
            let read = self
                .pages(&url, &params, |page| {
                    for (id, value) in newest(&page) {
                        out.entry(id).or_default().push((measure, value));
                    }
                    true
                })
                .await;
            if let Err(failure) = read {
                return (out, Some(failure));
            }
        }
        (out, None)
    }
}

/// What a refusal of Google's APIs means, from its status and its
/// `error.status` (`UNAUTHENTICATED`, `PERMISSION_DENIED`, …).
fn refusal(status: u16, answer: &Value) -> Failure {
    let code = answer["error"]["status"].as_str().unwrap_or_default();
    let message = answer["error"]["message"]
        .as_str()
        .and_then(|m| m.lines().next())
        .unwrap_or("the API refused the call");
    let text = bounded(&format!("{code}: {message}"));
    match (status, code) {
        (401, _) | (_, "UNAUTHENTICATED") => Failure::Login(text),
        (403, _) | (_, "PERMISSION_DENIED") => Failure::Permission(text),
        _ if code.is_empty() => Failure::Api(format!("the API answered {status}")),
        _ => Failure::Api(text),
    }
}

/// The newest point of each series of a `timeSeries.list` page, by the
/// instance it belongs to.
fn newest(page: &Value) -> Vec<(String, f64)> {
    let mut out = Vec::new();
    for series in page["timeSeries"].as_array().into_iter().flatten() {
        let Some(id) = series["resource"]["labels"]["instance_id"].as_str() else {
            continue;
        };
        let value = series["points"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|p| {
                let at = p["interval"]["endTime"].as_str()?;
                let v = &p["value"];
                let value = v["doubleValue"]
                    .as_f64()
                    .or_else(|| v["int64Value"].as_str()?.parse().ok())
                    .or_else(|| v["int64Value"].as_f64())?;
                Some((at, value))
            })
            .max_by(|a, b| a.0.cmp(b.0))
            .map(|(_, v)| v)
            .filter(|v| v.is_finite());
        if let Some(value) = value {
            out.push((id.to_owned(), value));
        }
    }
    out
}

/// The last part of a resource URL: `…/zones/europe-west1-b` gives
/// `europe-west1-b`.
fn last(url: &str) -> &str {
    url.rsplit('/').next().unwrap_or(url)
}

/// The MAC address Compute Engine gives an interface: `42:01` and its
/// internal IPv4 address.
pub fn mac(ip: &str) -> Option<String> {
    let ip: std::net::Ipv4Addr = ip.parse().ok()?;
    let [a, b, c, d] = ip.octets();
    Some(format!("42:01:{a:02x}:{b:02x}:{c:02x}:{d:02x}"))
}

/// One Compute Engine instance.
#[derive(Debug, Clone, PartialEq)]
pub struct Instance {
    /// Its numeric ID.
    pub id: String,
    pub name: String,
    pub zone: String,
    pub machine_type: Option<String>,
    /// `RUNNING`, `TERMINATED`, `SUSPENDED`, `STAGING`, …
    pub status: String,
    pub hostname: String,
    pub addresses: Vec<String>,
    pub macs: Vec<String>,
    /// The boot disk's first license, like `debian-12-bookworm`.
    pub os: Option<String>,
}

impl Instance {
    fn read(value: &Value, project: &str) -> Option<Self> {
        let text = |v: &Value| v.as_str().filter(|t| !t.is_empty()).map(str::to_owned);
        let id = text(&value["id"]).or_else(|| value["id"].as_u64().map(|n| n.to_string()))?;
        let name = text(&value["name"])?;
        let zone = text(&value["zone"])
            .map(|z| last(&z).to_owned())
            .unwrap_or_default();
        let mut addresses = Vec::new();
        let mut macs = Vec::new();
        let nics = value["networkInterfaces"].as_array().into_iter().flatten();
        for nic in nics.clone() {
            if let Some(ip) = nic["networkIP"].as_str().filter(|i| !i.is_empty()) {
                macs.extend(mac(ip));
                addresses.push(ip.to_owned());
            }
        }
        // Public addresses after the private ones.
        for config in nics.flat_map(|n| n["accessConfigs"].as_array().into_iter().flatten()) {
            if let Some(ip) = config["natIP"].as_str().filter(|i| !i.is_empty()) {
                if !addresses.iter().any(|a| a == ip) {
                    addresses.push(ip.to_owned());
                }
            }
        }
        let os = value["disks"]
            .as_array()
            .into_iter()
            .flatten()
            .find(|d| d["boot"].as_bool() == Some(true))
            .and_then(|d| d["licenses"].as_array()?.first()?.as_str())
            .map(|l| last(l).to_owned());
        // The default host name is the zonal internal DNS name.
        let hostname = text(&value["hostname"])
            .unwrap_or_else(|| format!("{name}.{zone}.c.{project}.internal"));
        Some(Self {
            id,
            name,
            zone,
            machine_type: text(&value["machineType"]).map(|m| last(&m).to_owned()),
            status: text(&value["status"]).unwrap_or_default(),
            hostname,
            addresses,
            macs,
            os,
        })
    }

    pub fn running(&self) -> bool {
        self.status == "RUNNING"
    }

    /// As a virtual machine of the inventory, with its metrics. Compute
    /// Engine has no status checks: `status` stays unknown.
    pub fn into_vm(self, metrics: Vec<(&'static str, f64)>) -> CloudVm {
        let running = self.running();
        CloudVm {
            vm: Vm {
                key: self.id,
                name: self.name,
                host: Some(self.zone).filter(|z| !z.is_empty()),
                power_state: self.status.to_ascii_lowercase(),
                powered_on: running,
                guest_hostname: Some(self.hostname),
                guest_os: self.os,
                addresses: self.addresses,
                macs: self.macs,
                ..Vm::default()
            },
            size: self.machine_type,
            measures: if running { metrics } else { Vec::new() },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ring::signature::{UnparsedPublicKey, RSA_PKCS1_2048_8192_SHA256};

    const KEY: &str = include_str!("../tests/fixtures/gcp_key.json");

    #[test]
    fn keys_are_read_and_checked() {
        let key = read_key(KEY).unwrap();
        assert_eq!(key.email, "reader@example-project.iam.gserviceaccount.com");
        assert_eq!(key.project.as_deref(), Some("example-project"));
        assert_eq!(key.key_id.as_deref().map(str::len), Some(40));
        assert!(!format!("{key:?}").contains("PRIVATE"));
        let mut value: Value = serde_json::from_str(KEY).unwrap();
        value["type"] = json!("authorized_user");
        assert!(read_key(&value.to_string()).is_err());
        let mut value: Value = serde_json::from_str(KEY).unwrap();
        value["private_key"] =
            json!("-----BEGIN PRIVATE KEY-----\nAAAA\n-----END PRIVATE KEY-----\n");
        let error = read_key(&value.to_string()).unwrap_err();
        assert!(!error.contains("AAAA"), "{error}");
        let mut value: Value = serde_json::from_str(KEY).unwrap();
        value["client_email"] = json!("not an address");
        assert!(read_key(&value.to_string()).is_err());
        assert!(read_key("not json").is_err());
        assert!(read_key(&" ".repeat(MAX_KEY + 1)).is_err());
    }

    #[test]
    fn projects_and_macs_are_checked() {
        assert!(valid_project("example-project"));
        assert!(valid_project("prod-123456"));
        for bad in [
            "short",
            "Example-project",
            "1-starts-with-digit",
            "ends-with-",
            "a/b-cdefg",
        ] {
            assert!(!valid_project(bad), "{bad}");
        }
        assert_eq!(mac("192.0.2.10").as_deref(), Some("42:01:c0:00:02:0a"));
        assert_eq!(mac("fd20::1"), None);
    }

    #[test]
    fn assertions_are_signed_with_the_key() {
        let key = read_key(KEY).unwrap();
        let http = Http::new(Duration::from_secs(5), true).unwrap();
        let client = Client::new(http, &key, "example-project".into(), None).unwrap();
        let jwt = client.assertion(1_800_000_000).unwrap();
        let parts: Vec<&str> = jwt.split('.').collect();
        assert_eq!(parts.len(), 3);
        let decode = |p: &str| -> Value {
            serde_json::from_slice(&URL_SAFE_NO_PAD.decode(p).unwrap()).unwrap()
        };
        let header = decode(parts[0]);
        assert_eq!(header["alg"], "RS256");
        assert_eq!(header["kid"].as_str(), key.key_id.as_deref());
        let claims = decode(parts[1]);
        assert_eq!(claims["iss"], key.email);
        assert_eq!(claims["aud"], TOKEN);
        assert_eq!(claims["exp"], 1_800_003_600);
        assert!(claims["scope"]
            .as_str()
            .unwrap()
            .contains("compute.readonly"));
        let public = RsaKeyPair::from_pkcs8(&key.der).unwrap();
        let public = UnparsedPublicKey::new(&RSA_PKCS1_2048_8192_SHA256, public.public().as_ref());
        let signed = format!("{}.{}", parts[0], parts[1]);
        public
            .verify(
                signed.as_bytes(),
                &URL_SAFE_NO_PAD.decode(parts[2]).unwrap(),
            )
            .unwrap();
    }

    #[test]
    fn only_this_machine_stands_in_for_google() {
        let key = read_key(KEY).unwrap();
        let http = Http::new(Duration::from_secs(5), true).unwrap();
        let client = Client::new(
            http.clone(),
            &key,
            "example-project".into(),
            Some("http://example.com".into()),
        )
        .unwrap();
        assert_eq!(client.endpoints(), (TOKEN.to_string(), COMPUTE, MONITORING));
        let local = Client::new(
            http,
            &key,
            "example-project".into(),
            Some("http://127.0.0.1:9".into()),
        )
        .unwrap();
        assert_eq!(local.endpoints().1, "http://127.0.0.1:9");
    }

    #[test]
    fn instances_read_from_an_aggregated_list() {
        let page = json!({"items": {
            "zones/europe-west1-b": {"instances": [
                {"id": "4567890123456789012", "name": "web-01",
                 "zone": "https://www.googleapis.com/compute/v1/projects/example-project/zones/europe-west1-b",
                 "machineType": "https://www.googleapis.com/compute/v1/projects/example-project/zones/europe-west1-b/machineTypes/e2-medium",
                 "status": "RUNNING",
                 "networkInterfaces": [{"networkIP": "192.0.2.2", "accessConfigs": [{"natIP": "203.0.113.7"}]}],
                 "disks": [{"boot": true, "licenses": ["https://www.googleapis.com/compute/v1/projects/debian-cloud/global/licenses/debian-12-bookworm"]}]},
                {"id": "1", "name": "batch", "zone": "zones/europe-west1-b", "status": "TERMINATED",
                 "hostname": "batch.example.internal"}
            ]},
            "zones/us-central1-a": {"warning": {"code": "NO_RESULTS_ON_PAGE"}}
        }});
        let found: Vec<Instance> = page["items"]
            .as_object()
            .unwrap()
            .values()
            .flat_map(|s| s["instances"].as_array().into_iter().flatten())
            .filter_map(|i| Instance::read(i, "example-project"))
            .collect();
        assert_eq!(found.len(), 2);
        let web = found[0].clone().into_vm(vec![("cpu_usage_ratio", 0.2)]);
        assert_eq!(web.vm.key, "4567890123456789012");
        assert_eq!(web.vm.host.as_deref(), Some("europe-west1-b"));
        assert_eq!(web.size.as_deref(), Some("e2-medium"));
        assert_eq!(web.vm.power_state, "running");
        assert_eq!(web.vm.macs, ["42:01:c0:00:02:02"]);
        assert_eq!(web.vm.addresses, ["192.0.2.2", "203.0.113.7"]);
        assert_eq!(web.vm.guest_os.as_deref(), Some("debian-12-bookworm"));
        assert_eq!(
            web.vm.guest_hostname.as_deref(),
            Some("web-01.europe-west1-b.c.example-project.internal")
        );
        assert_eq!(web.vm.status, None);
        let batch = found[1].clone().into_vm(vec![("cpu_usage_ratio", 0.2)]);
        assert!(!batch.vm.powered_on && batch.measures.is_empty());
        assert_eq!(
            batch.vm.guest_hostname.as_deref(),
            Some("batch.example.internal")
        );
    }

    #[test]
    fn series_give_their_newest_point() {
        let page = json!({"timeSeries": [
            {"resource": {"type": "gce_instance", "labels": {"instance_id": "1"}},
             "points": [
                {"interval": {"endTime": "2026-11-27T10:05:00Z"}, "value": {"doubleValue": 0.4}},
                {"interval": {"endTime": "2026-11-27T10:00:00Z"}, "value": {"doubleValue": 0.1}}]},
            {"resource": {"labels": {"instance_id": "2"}},
             "points": [{"interval": {"endTime": "2026-11-27T10:05:00Z"}, "value": {"int64Value": "300"}}]},
            {"resource": {"labels": {}}, "points": []}
        ]});
        assert_eq!(
            newest(&page),
            [("1".to_string(), 0.4), ("2".to_string(), 300.0)]
        );
    }

    #[test]
    fn refusals_say_what_was_refused() {
        let answer = |status: &str| {
            json!({"error": {"code": 403, "status": status,
                             "message": "Required 'compute.instances.list' permission for 'projects/example-project'\nmore"}})
        };
        let failure = refusal(403, &answer("PERMISSION_DENIED"));
        assert_eq!(failure.reason(), "permission");
        assert_eq!(
            failure.message(),
            "PERMISSION_DENIED: Required 'compute.instances.list' permission for 'projects/example-project'"
        );
        assert_eq!(refusal(401, &answer("UNAUTHENTICATED")).reason(), "login");
        assert_eq!(refusal(429, &answer("RESOURCE_EXHAUSTED")).reason(), "api");
        assert_eq!(refusal(502, &Value::Null).message(), "the API answered 502");
    }
}
