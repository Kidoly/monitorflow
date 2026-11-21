//! The PRTG API reader against a real PRTG, read only and gently: an
//! ignored test, run by hand with the address and token in the
//! environment, never in CI.
//!
//! A full reading asks for every sensor's settings and channel limits
//! one by one, tens of thousands of requests on a large installation.
//! This check reads every table, then the details of a sample only: every
//! probe and group, `PRTG_LIVE_PER_TYPE` sensors of each type (2 by
//! default) and their devices. The other objects get the answer of an
//! object without settings, without a request. Requests are spaced to
//! `PRTG_LIVE_RATE` per second (2 by default), the default limit of
//! common web application firewalls, and the check stops at the first
//! 429. Answers are kept in `PRTG_LIVE_CACHE`, so a second run sends
//! nothing already asked.
//!
//! ```text
//! PRTG_LIVE_URL=https://prtg.example.com PRTG_LIVE_APITOKEN=... \
//! PRTG_LIVE_PROXY_USER=... PRTG_LIVE_PROXY_PASSWORD=... \
//! PRTG_LIVE_CACHE=/somewhere/outside/the/repository \
//! cargo test -p monitorflow-import --test prtg_live -- --ignored --nocapture
//! ```
//!
//! It writes `tree.json` (objects, whether their details were read) and
//! `inventory.json` beside the cache, and prints counts only. Both hold
//! the installation's names and addresses: keep them out of the
//! repository, which the check refuses as a cache directory.

use std::collections::{BTreeMap, BTreeSet};
use std::hash::{Hash, Hasher};
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use chrono::Utc;
use monitorflow_import::prtg::api::{self, Fetch};
use monitorflow_import::prtg::mapping::inventory;
use monitorflow_import::prtg::tree::Kind;
use monitorflow_import::source::ReadError;
use serde_json::{json, Value};

/// The sensor table as the reader asks for it, to pick the sample.
const SENSORS: &[(&str, &str)] = &[
    ("content", "sensors"),
    ("columns", "objid,name,parentid,type,interval,status"),
    ("count", "*"),
];

fn env(name: &str) -> Option<String> {
    std::env::var(name).ok().filter(|v| !v.is_empty())
}

#[derive(Default)]
struct Counts {
    live: AtomicUsize,
    cached: AtomicUsize,
    skipped: AtomicUsize,
}

struct Live {
    client: reqwest::Client,
    base: String,
    token: String,
    proxy: Option<(String, String)>,
    cache: PathBuf,
    /// Objects whose details are read; `None` before the sample is
    /// chosen, while the tables are read.
    sample: Mutex<Option<BTreeSet<i64>>>,
    spacing: Duration,
    next: tokio::sync::Mutex<Instant>,
    counts: Counts,
}

impl Live {
    fn key(path: &str, query: &[(&str, String)]) -> String {
        let query: Vec<String> = query.iter().map(|(k, v)| format!("{k}={v}")).collect();
        format!("{path}?{}", query.join("&"))
    }

    fn file(&self, key: &str) -> PathBuf {
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        key.hash(&mut hasher);
        self.cache.join(format!("{:016x}", hasher.finish()))
    }

    /// Whether a request is for the details of an object out of the
    /// sample.
    fn skipped(&self, path: &str, query: &[(&str, String)]) -> bool {
        let detail = path == "/api/getobjectproperty.htm"
            || query
                .iter()
                .any(|(k, v)| *k == "content" && v == "channels");
        let id = query
            .iter()
            .find(|(k, _)| *k == "id")
            .and_then(|(_, v)| v.parse::<i64>().ok());
        match (detail, id, self.sample.lock().unwrap().as_ref()) {
            (true, Some(id), Some(sample)) => !sample.contains(&id),
            _ => false,
        }
    }
}

#[async_trait::async_trait]
impl Fetch for Live {
    async fn get(&self, path: &str, query: &[(&str, String)]) -> Result<String, ReadError> {
        let key = Self::key(path, query);
        let file = self.file(&key);
        if let Ok(body) = std::fs::read_to_string(&file) {
            self.counts.cached.fetch_add(1, Ordering::Relaxed);
            return Ok(body);
        }
        if self.skipped(path, query) {
            self.counts.skipped.fetch_add(1, Ordering::Relaxed);
            return Ok(if path == "/api/getobjectproperty.htm" {
                "<prtg><result>(Property not found)</result></prtg>".into()
            } else {
                r#"{"channels":[]}"#.into()
            });
        }
        {
            let mut next = self.next.lock().await;
            tokio::time::sleep_until((*next).into()).await;
            *next = Instant::now() + self.spacing;
        }
        self.counts.live.fetch_add(1, Ordering::Relaxed);
        let mut request = self
            .client
            .get(format!("{}{path}", self.base))
            .query(query)
            .query(&[("apitoken", &self.token)]);
        if let Some((user, password)) = &self.proxy {
            request = request.basic_auth(user, Some(password));
        }
        let response = request
            .send()
            .await
            .map_err(|e| ReadError::Connect(format!("{path}: {e}")))?;
        let status = response.status();
        assert_ne!(status.as_u16(), 429, "rate limited at {path}: stopping");
        let body = response
            .text()
            .await
            .map_err(|e| ReadError::Connect(format!("{path}: {e}")))?;
        if !status.is_success() {
            return Err(ReadError::Connect(format!("{path} answered {status}")));
        }
        std::fs::write(&file, &body).unwrap();
        Ok(body)
    }
}

#[tokio::test]
#[ignore = "reads a real PRTG; run by hand, see the module's comment"]
async fn a_real_prtg_read_gently() {
    let (Some(url), Some(token), Some(cache)) = (
        env("PRTG_LIVE_URL"),
        env("PRTG_LIVE_APITOKEN"),
        env("PRTG_LIVE_CACHE"),
    ) else {
        panic!("set PRTG_LIVE_URL, PRTG_LIVE_APITOKEN and PRTG_LIVE_CACHE");
    };
    let cache = PathBuf::from(cache);
    let repository = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..");
    let repository = repository.canonicalize().unwrap();
    std::fs::create_dir_all(&cache).unwrap();
    assert!(
        !cache.canonicalize().unwrap().starts_with(&repository),
        "PRTG_LIVE_CACHE must be outside the repository"
    );
    let per_type: usize = env("PRTG_LIVE_PER_TYPE").map_or(2, |v| v.parse().unwrap());
    let rate: f64 = env("PRTG_LIVE_RATE").map_or(2.0, |v| v.parse().unwrap());
    let live = Arc::new(Live {
        client: reqwest::Client::builder()
            .timeout(Duration::from_secs(60))
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .unwrap(),
        base: url.trim_end_matches('/').to_owned(),
        token,
        proxy: env("PRTG_LIVE_PROXY_USER").zip(env("PRTG_LIVE_PROXY_PASSWORD")),
        cache: cache.clone(),
        sample: Mutex::new(None),
        spacing: Duration::from_secs_f64(1.0 / rate),
        next: tokio::sync::Mutex::new(Instant::now()),
        counts: Counts::default(),
    });

    let started = Instant::now();
    // The sample: the first sensors of each type and their devices.
    let query: Vec<(&str, String)> = SENSORS.iter().map(|(k, v)| (*k, (*v).to_owned())).collect();
    let sensors = api::objects(
        &live.get("/api/table.json", &query).await.unwrap(),
        Kind::Sensor,
    )
    .unwrap();
    let mut by_type: BTreeMap<String, usize> = BTreeMap::new();
    let mut sample = BTreeSet::new();
    let mut devices = BTreeSet::new();
    for sensor in &sensors {
        let seen = by_type
            .entry(sensor.sensor_type.clone().unwrap_or_default())
            .or_default();
        if *seen < per_type {
            *seen += 1;
            sample.insert(sensor.id);
            devices.extend(sensor.parent);
        }
    }
    let sensor_ids: BTreeSet<i64> = sensors.iter().map(|s| s.id).collect();
    let all_devices: BTreeSet<i64> = sensors.iter().filter_map(|s| s.parent).collect();
    *live.sample.lock().unwrap() = Some(sample.clone());

    /// Adds probes and groups to the sample as the reader asks for
    /// them: any object neither a sensor nor a device out of the sample.
    struct Open(Arc<Live>, BTreeSet<i64>, BTreeSet<i64>, BTreeSet<i64>);
    #[async_trait::async_trait]
    impl Fetch for Open {
        async fn get(&self, path: &str, query: &[(&str, String)]) -> Result<String, ReadError> {
            let id = query
                .iter()
                .find(|(k, _)| *k == "id")
                .and_then(|(_, v)| v.parse::<i64>().ok());
            if let Some(id) = id {
                // Not a sensor, and a device only when sampled.
                let container =
                    !self.1.contains(&id) && (!self.2.contains(&id) || self.3.contains(&id));
                if container {
                    self.0.sample.lock().unwrap().as_mut().unwrap().insert(id);
                }
            }
            self.0.get(path, query).await
        }
    }
    let fetch = Arc::new(Open(live.clone(), sensor_ids, all_devices, devices));
    let tree = api::read(fetch, Utc::now()).await.unwrap();
    let elapsed = started.elapsed();
    let inventory = inventory(&tree, Utc::now());
    let sample = live.sample.lock().unwrap().clone().unwrap();

    let objects: Vec<Value> = tree
        .objects
        .iter()
        .map(|o| {
            json!({
                "id": o.id,
                "kind": format!("{:?}", o.kind),
                "name": o.name,
                "parent": o.parent,
                "type": o.sensor_type,
                "detailed": sample.contains(&o.id),
                "settings": o.settings.keys().collect::<Vec<_>>(),
                "channels": o.channels.len(),
                "limited": o.channels.iter().filter(|c| c.limits.is_some()).count(),
                "maintenance": o.maintenance.is_some(),
                "depends": o.depends.is_some(),
                "paused": o.paused.is_some(),
            })
        })
        .collect();
    let out = |name: &str, value: &Value| {
        std::fs::write(
            cache.join("..").join(name),
            serde_json::to_string_pretty(value).unwrap(),
        )
        .unwrap();
    };
    out(
        "tree.json",
        &json!({"objects": objects, "notifications": tree.notifications.len()}),
    );
    out("inventory.json", &serde_json::to_value(&inventory).unwrap());
    let kinds = |kind: Kind| tree.objects.iter().filter(|o| o.kind == kind).count();
    println!(
        "probes {} groups {} devices {} sensors {}; detailed {}; requests live {} cached {} skipped {}; {:.1} s",
        kinds(Kind::Probe),
        kinds(Kind::Group),
        kinds(Kind::Device),
        kinds(Kind::Sensor),
        sample.len(),
        live.counts.live.load(Ordering::Relaxed),
        live.counts.cached.load(Ordering::Relaxed),
        live.counts.skipped.load(Ordering::Relaxed),
        elapsed.as_secs_f64(),
    );
    println!(
        "hosts {} checks {} thresholds {} silences {} windows {} channels {} dependencies {} unmovable {}",
        inventory.hosts.len(),
        inventory.checks.len(),
        inventory.thresholds.len(),
        inventory.silences.len(),
        inventory.windows.len(),
        inventory.channels.len(),
        inventory.dependencies.len(),
        inventory.unmovable.len(),
    );
}
