//! The example plugins of `plugins/examples`, signed with a throwaway key
//! and run through the real runtime. They are built for WebAssembly apart
//! from the workspace, so this test needs `MONITORFLOW_PLUGIN_EXAMPLES`,
//! the directory of the built modules; CI sets it, and without it the
//! test only says it skipped.

use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use monitorflow_plugin::http::{Checked, Http, Response};
use monitorflow_plugin::package::{self, Package};
use monitorflow_plugin::{Manifest, Plugin, Runner};
use ring::rand::SystemRandom;
use ring::signature::{Ed25519KeyPair, KeyPair as _};
use serde_json::json;

fn examples() -> Option<PathBuf> {
    let dir = std::env::var_os("MONITORFLOW_PLUGIN_EXAMPLES").map(PathBuf::from);
    if dir.is_none() {
        eprintln!("MONITORFLOW_PLUGIN_EXAMPLES is not set: skipped");
    }
    dir
}

/// Signs and verifies the example `name`, as an install would.
fn install(dir: &std::path::Path, name: &str) -> (Manifest, Arc<Plugin>) {
    let module = std::fs::read(dir.join(format!("{}.wasm", name.replace('-', "_")))).unwrap();
    let manifest_path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join(format!("../../plugins/examples/{name}/manifest.json"));
    let manifest = std::fs::read_to_string(manifest_path).unwrap();
    let pkcs8 = Ed25519KeyPair::generate_pkcs8(&SystemRandom::new()).unwrap();
    let pair = Ed25519KeyPair::from_pkcs8(pkcs8.as_ref()).unwrap();
    let key = monitorflow_plugin::keys::PublicKey::from_bytes(pair.public_key().as_ref()).unwrap();
    let file = Package::sign(&manifest, &module, &pair).unwrap().to_bytes();
    let verified = package::verify_file(&file, &[key]).unwrap();
    let plugin = Plugin::load(&verified.module).unwrap();
    (verified.manifest, Arc::new(plugin))
}

#[derive(Default)]
struct Recorder {
    requests: Mutex<Vec<Checked>>,
    status: u16,
    body: String,
}

impl Http for Recorder {
    fn send(&self, request: Checked, _: Duration) -> Result<Response, String> {
        self.requests.lock().unwrap().push(request);
        Ok(Response {
            status: self.status,
            headers: Vec::new(),
            body: self.body.clone(),
            elapsed_ms: 250,
        })
    }
}

#[tokio::test]
async fn the_http_json_check_reports_its_samples() {
    let Some(dir) = examples() else { return };
    let (manifest, plugin) = install(&dir, "http-json");
    let values = manifest
        .values(
            &[
                ("url".into(), "http://app.lan:8080/health".into()),
                ("field".into(), "queue.length".into()),
            ]
            .into(),
        )
        .unwrap();
    let http = Arc::new(Recorder {
        status: 200,
        body: r#"{"queue": {"length": 7}}"#.into(),
        ..Default::default()
    });
    let runner = Runner::new(http.clone());
    let out = runner
        .run(
            "http-json",
            plugin,
            json!({"settings": values.settings, "instance": ""}),
            manifest.host_rules(&values.settings),
            Vec::new(),
            Duration::from_secs(10),
        )
        .await
        .unwrap();
    assert_eq!(
        out["samples"],
        json!([
            {"metric": "up", "instance": "", "value": 1.0, "labels": {}},
            {"metric": "status_code", "instance": "", "value": 200.0, "labels": {}},
            {"metric": "seconds", "instance": "", "value": 0.25, "labels": {}},
            {"metric": "value", "instance": "", "value": 7.0, "labels": {}},
        ])
    );
    assert_eq!(
        http.requests.lock().unwrap()[0].url.as_str(),
        "http://app.lan:8080/health"
    );
}

#[tokio::test]
async fn the_ntfy_channel_posts_the_message_with_its_token() {
    let Some(dir) = examples() else { return };
    let (manifest, plugin) = install(&dir, "ntfy");
    let values = manifest
        .values(
            &[
                ("topic".into(), "ops-alerts".into()),
                ("token".into(), "tk_secret".into()),
            ]
            .into(),
        )
        .unwrap();
    let http = Arc::new(Recorder {
        status: 200,
        ..Default::default()
    });
    let input = json!({
        "settings": values.settings,
        "secrets": values.secrets,
        "message": {"event": "incident.opened", "text": "web1: disk / at 95%", "url": "https://mf.example/incidents/1",
                    "lines": [], "payload": {"incident": {"severity": "critical"}}},
    });
    let secrets: Vec<String> = values.secrets.values().cloned().collect();
    let rules = manifest.host_rules(&values.settings);
    let runner = Runner::new(http.clone());
    runner
        .run(
            "ntfy",
            plugin.clone(),
            input.clone(),
            rules.clone(),
            secrets.clone(),
            Duration::from_secs(10),
        )
        .await
        .unwrap();
    let request = http.requests.lock().unwrap().remove(0);
    assert_eq!(request.url.as_str(), "https://ntfy.sh/ops-alerts");
    assert_eq!(request.body, "web1: disk / at 95%");
    let header = |name: &str| {
        request
            .headers
            .iter()
            .find(|(n, _)| n == name)
            .map(|(_, v)| v.clone())
    };
    assert_eq!(header("Authorization").as_deref(), Some("Bearer tk_secret"));
    assert_eq!(header("Priority").as_deref(), Some("5"));
    assert_eq!(
        header("Click").as_deref(),
        Some("https://mf.example/incidents/1")
    );

    // A refusal is the channel's error, without the token.
    let refusing = Arc::new(Recorder {
        status: 403,
        body: "forbidden for tk_secret\n".into(),
        ..Default::default()
    });
    let error = Runner::new(refusing)
        .run(
            "ntfy",
            plugin,
            input,
            rules,
            secrets,
            Duration::from_secs(10),
        )
        .await
        .unwrap_err();
    assert_eq!(
        error.to_string(),
        "ntfy answered 403: forbidden for [REDACTED]"
    );
}
