use std::time::Duration;

use monitorflow_plugin::http::{Checked, Http, Response};
use ring::rand::SystemRandom;
use ring::signature::{EcdsaKeyPair, Ed25519KeyPair, ECDSA_P256_SHA256_ASN1_SIGNING};

use super::*;

/// A check plugin that returns `result` whatever its input.
fn module(result: &str) -> Vec<u8> {
    let escaped = result.replace('"', "\\\"");
    wat::parse_str(format!(
        r#"(module
            (import "mf" "output" (func $out (param i32 i32)))
            (memory (export "memory") 1)
            (data (i32.const 0) "{escaped}")
            (func (export "mf_run") (result i32)
                (call $out (i32.const 0) (i32.const {})) (i32.const 0)))"#,
        result.len()
    ))
    .unwrap()
}

/// A package of the check `name` signed with a throwaway key.
fn package(name: &str, result: &str) -> Vec<u8> {
    let pkcs8 = Ed25519KeyPair::generate_pkcs8(&SystemRandom::new()).unwrap();
    let pair = Ed25519KeyPair::from_pkcs8(pkcs8.as_ref()).unwrap();
    let manifest = json!({
        "name": name, "version": "1.0.0", "kind": "check", "title": "T", "abi": 1,
        "settings": [{"key": "url", "label": "Address", "type": "url", "required": true}]
    });
    Package::sign(&manifest.to_string(), &module(result), &pair)
        .unwrap()
        .to_bytes()
}

/// A throwaway certificate authority signing grants as the server does.
struct Server {
    cert: CertificateDer<'static>,
    key: EcdsaKeyPair,
}

impl Server {
    fn new() -> Self {
        let key = rcgen::KeyPair::generate_for(&rcgen::PKCS_ECDSA_P256_SHA256).unwrap();
        let cert = rcgen::CertificateParams::default()
            .self_signed(&key)
            .unwrap();
        let pair = EcdsaKeyPair::from_pkcs8(
            &ECDSA_P256_SHA256_ASN1_SIGNING,
            &key.serialize_der(),
            &SystemRandom::new(),
        )
        .unwrap();
        Self {
            cert: cert.der().clone(),
            key: pair,
        }
    }

    fn grant(&self, agent_id: &str, name: &str, package: Vec<u8>) -> PluginGrant {
        let message = plugin_grant::signed_bytes(agent_id, name, &plugin_grant::sha256(&package));
        let signature = self.key.sign(&SystemRandom::new(), &message).unwrap();
        PluginGrant {
            name: name.into(),
            package,
            signature: signature.as_ref().to_vec(),
        }
    }

    /// The signature of `grants` as the set of `web1` made at
    /// `issued_at`, valid for a week of the test's clock.
    fn set(&self, grants: &[PluginGrant], issued_at: i64) -> PluginGrantSet {
        let expires_at = issued_at + plugin_grant::SET_LIFETIME_MS;
        let message = plugin_grant::set_signed_bytes("web1", issued_at, expires_at, grants);
        let signature = self.key.sign(&SystemRandom::new(), &message).unwrap();
        PluginGrantSet {
            issued_at_unix_ms: issued_at,
            expires_at_unix_ms: expires_at,
            signature: signature.as_ref().to_vec(),
        }
    }

    fn checks(&self, config: AgentPluginsConfig) -> PluginChecks {
        struct NoHttp;
        impl Http for NoHttp {
            fn send(&self, _: Checked, _: Duration) -> Result<Response, String> {
                Err("no network in tests".into())
            }
        }
        let cert = self.cert.clone();
        let runner = Runner::new(Arc::new(NoHttp)).with_fuel(10_000_000);
        PluginChecks::with_runner("web1", Arc::new(move || vec![cert.clone()]), config, runner)
    }
}

fn config(plugin: &str, url: Option<&str>) -> sensors::SensorConfig {
    let mut params = BTreeMap::from([(PLUGIN_PARAM.to_owned(), plugin.to_owned())]);
    if let Some(url) = url {
        params.insert("url".into(), url.into());
    }
    sensors::SensorConfig {
        name: "shop".into(),
        instance: String::new(),
        interval: Duration::from_secs(60),
        timeout: None,
        params,
    }
}

fn refusal(result: Result<Arc<dyn Sensor>, FactoryError>) -> String {
    match result {
        Err(FactoryError::Refused(why)) => why,
        Err(other) => panic!("not a refusal: {other}"),
        Ok(_) => panic!("built"),
    }
}

const UP: &str = r#"{"samples": [{"metric": "up", "value": 1}, {"metric": "seconds", "instance": "eu", "value": 0.25, "labels": {"code": "200"}}]}"#;

#[tokio::test]
async fn a_granted_plugin_runs_as_a_check() {
    let server = Server::new();
    let checks = server.checks(AgentPluginsConfig::default());
    checks.prepare(&[server.grant("web1", "web", package("web", UP))], None);
    let sensor = checks
        .build(&config("web", Some("https://shop.example")))
        .unwrap();
    let samples = sensor
        .collect(tokio::time::Instant::now() + Duration::from_secs(5))
        .await
        .unwrap();
    assert_eq!(samples.len(), 2);
    assert_eq!((samples[0].sensor.as_str(), samples[0].value), ("up", 1.0));
    assert_eq!(samples[1].instance, "eu");
    assert_eq!(samples[1].labels["code"], "200");
    // Its settings are checked against its manifest.
    assert!(matches!(
        checks.build(&config("web", None)),
        Err(FactoryError::Invalid(_))
    ));
}

#[tokio::test]
async fn only_plugins_granted_to_this_agent_run() {
    let server = Server::new();
    let checks = server.checks(AgentPluginsConfig::default());
    let url = Some("https://shop.example");
    assert!(refusal(checks.build(&config("web", url))).contains("not granted"));
    checks.prepare(&[server.grant("web2", "web", package("web", UP))], None);
    assert!(refusal(checks.build(&config("web", url))).contains("the server's grant"));
    // A relay's own CA signs nothing the agent trusts.
    checks.prepare(
        &[Server::new().grant("web1", "web", package("web", UP))],
        None,
    );
    assert!(refusal(checks.build(&config("web", url))).contains("the server's grant"));
    // A grant under another name.
    checks.prepare(&[server.grant("web1", "shop", package("web", UP))], None);
    assert!(refusal(checks.build(&config("shop", url))).contains("not the check plugin"));
}

#[tokio::test]
async fn the_host_keeps_the_last_word() {
    let server = Server::new();
    let off = server.checks(AgentPluginsConfig {
        checks: false,
        official_only: false,
    });
    off.prepare(&[server.grant("web1", "web", package("web", UP))], None);
    assert!(
        refusal(off.build(&config("web", Some("https://x.example")))).contains("checks = false")
    );
    let official = server.checks(AgentPluginsConfig {
        checks: true,
        official_only: true,
    });
    official.prepare(&[server.grant("web1", "web", package("web", UP))], None);
    assert!(
        refusal(official.build(&config("web", Some("https://x.example"))))
            .contains("official_only")
    );
}

#[tokio::test]
async fn bad_samples_fail_the_run() {
    let server = Server::new();
    let checks = server.checks(AgentPluginsConfig::default());
    for result in [
        r#"{"samples": [{"metric": "Up", "value": 1}]}"#,
        r#"{"samples": [{"metric": "up", "value": 1, "labels": {"peer": "x"}}]}"#,
        r#"{"samples": [{"metric": "up", "value": 1, "extra": 1}]}"#,
    ] {
        checks.prepare(&[server.grant("web1", "web", package("web", result))], None);
        let sensor = checks
            .build(&config("web", Some("https://x.example")))
            .unwrap();
        let error = sensor
            .collect(tokio::time::Instant::now() + Duration::from_secs(5))
            .await
            .unwrap_err();
        assert!(!error.is_permanent(), "{result}: {error}");
    }
}

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_millis() as i64
}

#[tokio::test]
async fn an_older_list_of_plugins_never_comes_back() {
    let server = Server::new();
    let checks = server.checks(AgentPluginsConfig::default());
    let now = now_ms();
    let v1 = [server.grant("web1", "web", package("web", UP))];
    let old = server.set(&v1, now - 60_000);

    checks.prepare(&v1, Some(&old));
    checks.build(&config("web", Some("http://x"))).unwrap();
    // The same list again, as at every reconnection.
    checks.prepare(&v1, Some(&old));
    checks.build(&config("web", Some("http://x"))).unwrap();

    // The administrator turns the plugin off: no plugin, and signed so.
    checks.prepare(&[], Some(&server.set(&[], now)));
    let why = refusal(checks.build(&config("web", Some("http://x"))));
    assert!(why.contains("not granted"), "{why}");

    // A relay sends the old configuration again: refused.
    checks.prepare(&v1, Some(&old));
    let why = refusal(checks.build(&config("web", Some("http://x"))));
    assert!(why.contains("older than"), "{why}");
    // Or the old grant under the new, empty list, or with none at all.
    checks.prepare(&v1, Some(&server.set(&[], now)));
    let why = refusal(checks.build(&config("web", Some("http://x"))));
    assert!(why.contains("list of plugins"), "{why}");
    checks.prepare(&v1, None);
    let why = refusal(checks.build(&config("web", Some("http://x"))));
    assert!(why.contains("is missing"), "{why}");

    // Turned on again by the server: a newer list.
    checks.prepare(&v1, Some(&server.set(&v1, now + 1)));
    checks.build(&config("web", Some("http://x"))).unwrap();
}

#[tokio::test]
async fn a_list_of_plugins_is_for_this_agent_and_ends() {
    let server = Server::new();
    let checks = server.checks(AgentPluginsConfig::default());
    let now = now_ms();
    let grants = [server.grant("web1", "web", package("web", UP))];
    let why = |set: &PluginGrantSet| {
        checks.prepare(&grants, Some(set));
        refusal(checks.build(&config("web", Some("http://x"))))
    };

    // Signed by someone else, or for other times than it says.
    let foreign = Server::new().set(&grants, now);
    assert!(why(&foreign).contains("does not verify"));
    let mut stretched = server.set(&grants, now);
    stretched.expires_at_unix_ms += 1;
    assert!(why(&stretched).contains("does not verify"));

    // Older than a week: a new one comes with the next connection.
    let expired = server.set(&grants, now - plugin_grant::SET_LIFETIME_MS - 1);
    assert!(why(&expired).contains("expired"));
}

#[tokio::test]
async fn the_newest_list_is_remembered_across_restarts() {
    let server = Server::new();
    let dir = tempfile::tempdir().unwrap();
    let state = dir.path().join("plugin-grants");
    let now = now_ms();
    let v1 = [server.grant("web1", "web", package("web", UP))];
    let old = server.set(&v1, now - 60_000);

    let checks = server
        .checks(AgentPluginsConfig::default())
        .with_state(state.clone());
    checks.prepare(&v1, Some(&old));
    checks.prepare(&[], Some(&server.set(&[], now)));
    // Saved off the runtime.
    for _ in 0..200 {
        if std::fs::read_to_string(&state).is_ok_and(|t| t == now.to_string()) {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert_eq!(std::fs::read_to_string(&state).unwrap(), now.to_string());

    // After a restart the old list is still refused.
    let restarted = server
        .checks(AgentPluginsConfig::default())
        .with_state(state);
    restarted.prepare(&v1, Some(&old));
    let why = refusal(restarted.build(&config("web", Some("http://x"))));
    assert!(why.contains("older than"), "{why}");
}
