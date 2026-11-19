//! `vmware` checks against vcsim, govmomi's vCenter simulator (ADR-0156).
//! Skipped unless `MF_VCSIM` names it as `host:port`, with the user
//! `MF_VCSIM_USER` and password `MF_VCSIM_PASSWORD` (`reader` and
//! `reader-pass` by default). `deploy/vmware-test/run-e2e.sh` starts one
//! with a disconnected host, a full datastore and two snapshots.

use std::collections::BTreeMap;
use std::time::Duration;

use monitorflow_sensors::Sample;
use monitorflow_vmware::soap::Client;
use monitorflow_vmware::tls::{self, Trust};
use tokio::time::Instant;

fn target() -> Option<(String, String)> {
    let addr = std::env::var("MF_VCSIM").ok()?;
    let (host, port) = addr.rsplit_once(':')?;
    Some((host.to_owned(), port.to_owned()))
}

fn credentials() -> (String, String) {
    (
        std::env::var("MF_VCSIM_USER").unwrap_or_else(|_| "reader".into()),
        std::env::var("MF_VCSIM_PASSWORD").unwrap_or_else(|_| "reader-pass".into()),
    )
}

async fn collect(pairs: &[(&str, &str)]) -> Vec<Sample> {
    let (host, port) = target().unwrap();
    let mut params: BTreeMap<String, String> = pairs
        .iter()
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect();
    params.insert("host".into(), host);
    params.insert("port".into(), port);
    let sensor = monitorflow_vmware::build(&params).unwrap();
    sensor
        .collect(Instant::now() + Duration::from_secs(30))
        .await
        .unwrap()
}

fn one<'a>(samples: &'a [Sample], sensor: &str) -> &'a Sample {
    samples
        .iter()
        .find(|s| s.sensor == sensor)
        .unwrap_or_else(|| panic!("no {sensor} in {samples:#?}"))
}

/// The fingerprint vcsim's certificate has, from a test.
async fn fingerprint() -> String {
    let test = collect(&[("what", "test")]).await;
    let certificate = one(&test, "certificate_trusted");
    assert_eq!(
        certificate.value, 0.0,
        "vcsim's own certificate is self-signed"
    );
    certificate.labels["fingerprint"].clone()
}

#[tokio::test]
async fn a_test_looks_before_it_logs_in() {
    if target().is_none() {
        eprintln!("MF_VCSIM is not set; skipped");
        return;
    }
    let (user, password) = credentials();
    let answers = collect(&[("what", "test"), ("user", &user), ("password", &password)]).await;
    assert_eq!(one(&answers, "answers").labels["api_type"], "VirtualCenter");
    assert!(
        answers.iter().all(|s| s.sensor != "login"),
        "never logs in to an untrusted certificate: {answers:#?}"
    );
    let print = fingerprint().await;
    let pinned = collect(&[
        ("what", "test"),
        ("user", &user),
        ("password", &password),
        ("fingerprint", &print),
    ])
    .await;
    assert_eq!(one(&pinned, "certificate_trusted").value, 1.0);
    assert_eq!(one(&pinned, "login").value, 1.0);
    let vms = pinned
        .iter()
        .find(|s| s.sensor == "objects" && s.instance == "vm")
        .unwrap();
    assert!(vms.value >= 4.0, "{vms:?}");

    let refused = collect(&[
        ("what", "test"),
        ("user", &user),
        ("password", "wrong password"),
        ("fingerprint", &print),
    ])
    .await;
    assert_eq!(one(&refused, "login").labels["reason"], "login");
}

#[tokio::test]
async fn an_inventory_reads_with_a_pinned_certificate() {
    if target().is_none() {
        eprintln!("MF_VCSIM is not set; skipped");
        return;
    }
    let (user, password) = credentials();
    let print = fingerprint().await;
    let base = [("user", user.as_str()), ("password", password.as_str())];

    let untrusted = collect(&base).await;
    let up = one(&untrusted, "up");
    assert_eq!(up.value, 0.0);
    assert_eq!(up.labels["reason"], "certificate");
    assert!(up.labels["error"].contains(&print), "{up:?}");

    let other = "AB:".repeat(31) + "AB";
    let changed = collect(&[base[0], base[1], ("fingerprint", &other)]).await;
    assert!(one(&changed, "up").labels["error"].contains("changed"));

    let wrong = collect(&[
        base[0],
        ("password", "wrong password"),
        ("fingerprint", &print),
    ])
    .await;
    assert_eq!(one(&wrong, "up").labels["reason"], "login");

    let samples = collect(&[base[0], base[1], ("fingerprint", &print)]).await;
    assert_eq!(one(&samples, "up").value, 1.0);
    let count = |sensor: &str| samples.iter().filter(|s| s.sensor == sensor).count();
    assert!(count("vm_info") >= 4);
    assert!(count("host_info") >= 2);
    assert!(count("datastore_info") >= 1);
    assert!(count("cluster_info") >= 1);
    assert!(samples
        .iter()
        .all(|s| s.sensor == "up" || s.labels.contains_key("virt")));

    if std::env::var("MF_VCSIM_SCENARIO").is_ok() {
        // As run-e2e.sh sets it up.
        let value = |sensor: &str, instance: &str| {
            samples
                .iter()
                .find(|s| s.sensor == sensor && s.instance == instance)
                .map(|s| s.value)
        };
        assert_eq!(value("host_connected", "DC0_C0_H1"), Some(0.0));
        assert!(value("datastore_free_ratio", "LocalDS_1").unwrap() < 0.05);
        assert_eq!(value("vm_snapshots", "DC0_H0_VM0"), Some(2.0));
        assert!(value("vm_snapshot_age_seconds", "DC0_H0_VM0").is_some());
    }
}

#[tokio::test]
async fn pages_are_followed_and_sessions_kept() {
    let Some((host, port)) = target() else {
        eprintln!("MF_VCSIM is not set; skipped");
        return;
    };
    let (user, password) = credentials();
    let print = tls::parse_fingerprint(&fingerprint().await).unwrap();
    let port: u16 = port.parse().unwrap();
    let mut whole = Client::new(
        &host,
        port,
        Trust::Verify(Some(print)),
        Duration::from_secs(10),
    )
    .unwrap();
    let content = whole.service_content().await.unwrap();
    whole.login(&content, &user, &password).await.unwrap();
    let view = whole.create_view(&content).await.unwrap();
    let all = whole.retrieve(&content, &view).await.unwrap();
    whole.destroy_view(&view).await;

    let mut paged = Client::new(
        &host,
        port,
        Trust::Verify(Some(print)),
        Duration::from_secs(10),
    )
    .unwrap()
    .with_page(3);
    paged.login(&content, &user, &password).await.unwrap();
    let view = paged.create_view(&content).await.unwrap();
    let pages = paged.retrieve(&content, &view).await.unwrap();
    assert_eq!(pages.len(), all.len());
    assert!(all.len() > 3);
    // The kept session works for the next call.
    let again = paged.create_view(&content).await.unwrap();
    paged.destroy_view(&again).await;
}
