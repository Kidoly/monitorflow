//! Checks against real servers, run when their addresses are given
//! (ADR-0183); skipped otherwise, as CI has none. `scripts/apps-test.sh`
//! starts PostgreSQL, MySQL, MariaDB and Redis in containers with the
//! read-only accounts the guides describe, and runs these.
//!
//! `MF_APPS_TEST_POSTGRESQL`, `MF_APPS_TEST_MYSQL`, `MF_APPS_TEST_MARIADB`
//! and `MF_APPS_TEST_REDIS` are `host:port`; the account is `monitorflow`
//! with the password in `MF_APPS_TEST_PASSWORD`. `MF_APPS_TEST_DOCKER=1`
//! reads the local engine.

use std::collections::BTreeMap;
use std::time::Duration;

use monitorflow_apps::{build, Place};
use monitorflow_sensors::Sample;

fn target(var: &str) -> Option<(String, String)> {
    let value = std::env::var(var).ok().filter(|v| !v.is_empty())?;
    let (host, port) = value.rsplit_once(':')?;
    Some((host.to_owned(), port.to_owned()))
}

fn params(host: &str, port: &str, password: &str) -> BTreeMap<String, String> {
    BTreeMap::from([
        ("host".to_owned(), host.to_owned()),
        ("port".to_owned(), port.to_owned()),
        ("user".to_owned(), "monitorflow".to_owned()),
        ("password".to_owned(), password.to_owned()),
    ])
}

async fn collect(kind: &str, params: &BTreeMap<String, String>) -> Vec<Sample> {
    let sensor = build(kind, params, Place::Relay).unwrap();
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    sensor.collect(deadline).await.unwrap()
}

fn find<'a>(samples: &'a [Sample], metric: &str) -> Option<&'a Sample> {
    samples.iter().find(|s| s.sensor == metric)
}

/// Collects twice, so changes have a value, and checks `wanted` are there
/// with `up` at 1; then a wrong password gives `up` 0 for `login`.
async fn check(kind: &str, var: &str, wanted: &[&str]) {
    let Some((host, port)) = target(var) else {
        eprintln!("{var} not set: skipped");
        return;
    };
    let password = std::env::var("MF_APPS_TEST_PASSWORD").unwrap_or_default();
    let good = params(&host, &port, &password);
    let sensor = build(kind, &good, Place::Relay).unwrap();
    let deadline = || tokio::time::Instant::now() + Duration::from_secs(10);
    let _ = sensor.collect(deadline()).await.unwrap();
    tokio::time::sleep(Duration::from_secs(1)).await;
    let samples = sensor.collect(deadline()).await.unwrap();
    let up = find(&samples, "up").expect("up");
    assert_eq!(up.value, 1.0, "{kind}: {up:?}");
    assert!(!up.labels["version"].is_empty());
    for metric in wanted {
        assert!(
            find(&samples, metric).is_some(),
            "{kind}: no {metric} in {samples:#?}"
        );
    }
    for sample in &samples {
        assert!(sample.value.is_finite(), "{sample:?}");
        assert!(!format!("{sample:?}").contains(&password) || password.len() < 3);
    }

    let wrong = params(&host, &port, "not-the-password");
    let samples = collect(kind, &wrong).await;
    let up = find(&samples, "up").unwrap();
    assert_eq!(up.value, 0.0);
    assert_eq!(up.labels["reason"], "login", "{kind}: {up:?}");
}

#[tokio::test]
async fn postgresql() {
    check(
        "postgresql",
        "MF_APPS_TEST_POSTGRESQL",
        &[
            "response_seconds",
            "connections",
            "connections_ratio",
            "slow_queries",
            "locks_waiting",
            "deadlocks",
            "transactions_per_second",
            "database_size_bytes",
            "replicas",
        ],
    )
    .await;
}

#[tokio::test]
async fn mysql() {
    let wanted = [
        "connections",
        "connections_ratio",
        "threads_running",
        "queries_per_second",
        "slow_queries",
        "buffer_pool_hit_ratio",
        "lock_waits",
    ];
    check("mysql", "MF_APPS_TEST_MYSQL", &wanted).await;
}

#[tokio::test]
async fn mariadb() {
    let wanted = [
        "connections",
        "connections_ratio",
        "queries_per_second",
        "lock_waits",
    ];
    check("mysql", "MF_APPS_TEST_MARIADB", &wanted).await;
    if let Some((host, port)) = target("MF_APPS_TEST_MARIADB") {
        let password = std::env::var("MF_APPS_TEST_PASSWORD").unwrap_or_default();
        let samples = collect("mysql", &params(&host, &port, &password)).await;
        assert_eq!(find(&samples, "up").unwrap().labels["flavor"], "mariadb");
    }
}

#[tokio::test]
async fn redis() {
    check(
        "redis",
        "MF_APPS_TEST_REDIS",
        &[
            "clients",
            "memory_used_bytes",
            "keys",
            "evicted_keys",
            "last_save_ok",
            "replicas",
        ],
    )
    .await;
}

#[tokio::test]
async fn docker() {
    if std::env::var("MF_APPS_TEST_DOCKER").as_deref() != Ok("1") {
        eprintln!("MF_APPS_TEST_DOCKER not set: skipped");
        return;
    }
    let sensor = build("docker", &BTreeMap::new(), Place::Agent(None)).unwrap();
    let deadline = || tokio::time::Instant::now() + Duration::from_secs(30);
    let _ = sensor.collect(deadline()).await.unwrap();
    let samples = sensor.collect(deadline()).await.unwrap();
    let up = find(&samples, "up").unwrap();
    assert_eq!(up.value, 1.0, "{up:?}");
    assert!(find(&samples, "containers_running").unwrap().value >= 1.0);
    let running = find(&samples, "container_running").unwrap();
    assert!(!running.instance.is_empty());
    assert!(find(&samples, "container_memory_bytes").is_some());
    assert!(find(&samples, "container_cpu_ratio").is_some());
}
