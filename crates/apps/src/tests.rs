use std::collections::BTreeMap;

use monitorflow_sensors::port::PortTargets;
use serde_json::json;

use super::*;
use crate::rates::Rates;

fn params(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
    pairs
        .iter()
        .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
        .collect()
}

fn error(kind: &str, pairs: &[(&str, &str)], place: Place<'_>) -> String {
    build(kind, &params(pairs), place)
        .err()
        .unwrap()
        .to_string()
}

#[test]
fn kinds_build_with_their_defaults() {
    for kind in KINDS {
        assert!(
            build(kind, &BTreeMap::new(), Place::Check).is_ok(),
            "{kind}"
        );
        assert!(is_kind(kind));
    }
    assert!(!is_kind("cpu"));
    assert!(takes_secret(POSTGRESQL) && !takes_secret(DOCKER));
    assert!(matches!(
        build("cpu", &BTreeMap::new(), Place::Check),
        Err(BuildError::UnknownKind(_))
    ));
}

#[test]
fn parameters_are_checked() {
    let all = [
        ("host", "db.example.com"),
        ("port", "6432"),
        ("user", "watcher"),
        ("password", "secret"),
        ("database", "app"),
        ("tls", "verify"),
        ("slow_after", "10"),
    ];
    assert!(build(POSTGRESQL, &params(&all), Place::Relay).is_ok());
    assert!(error(POSTGRESQL, &[("port", "0")], Place::Check).contains("between 1 and 65535"));
    assert!(error(POSTGRESQL, &[("tls", "maybe")], Place::Check)
        .contains("`off`, `prefer` or `verify`"));
    assert!(error(POSTGRESQL, &[("slow_after", "0")], Place::Check).contains("from 1 to 86400"));
    assert!(
        error(MYSQL, &[("database", "app")], Place::Check).contains("unknown parameter `database`")
    );
    assert!(error(MYSQL, &[("host", "a b")], Place::Check).contains("neither an IP address"));
    assert!(error(REDIS, &[("tls", "verify")], Place::Check).contains("not checked yet"));
    assert!(build(MSSQL, &params(&all[..6]), Place::Relay).is_err());
    assert!(build(
        MSSQL,
        &params(&[all[0], all[1], all[2], all[3], all[5]]),
        Place::Relay
    )
    .is_ok());
    assert!(
        error(MSSQL, &[("host", "db\\SQLEXPRESS")], Place::Check).contains("neither an IP address")
    );
    assert!(error(REDIS, &[("password", "a\nb")], Place::Check).contains("line breaks"));
    assert!(
        error(DOCKER, &[("engine", "/tmp/sock")], Place::Check).contains("`docker` or `podman`")
    );
    assert!(error(DOCKER, &[("containers", "(")], Place::Check).contains("regular expression"));
    assert!(build(
        DOCKER,
        &params(&[("containers", "^web-"), ("stats", "false")]),
        Place::Check
    )
    .is_ok());
}

#[test]
fn places_bound_where_checks_connect() {
    // Docker is the agent's alone.
    assert!(error(DOCKER, &[], Place::Relay).contains("own agent"));
    assert!(error(DOCKER, &[], Place::Central).contains("own agent"));
    // The central server never reaches link-local addresses.
    assert!(error(REDIS, &[("host", "169.254.169.254")], Place::Central).contains("link-local"));
    assert!(error(REDIS, &[("host", "fe80::1")], Place::Central).contains("link-local"));
    assert!(build(REDIS, &params(&[("host", "169.254.169.254")]), Place::Relay).is_ok());
    // An agent stays within its port targets.
    let targets = PortTargets::parse(&["127.0.0.1", "db.internal:5432"]).unwrap();
    let agent = Place::Agent(Some(&targets));
    assert!(build(POSTGRESQL, &BTreeMap::new(), agent).is_ok());
    assert!(build(POSTGRESQL, &params(&[("host", "db.internal")]), agent).is_ok());
    assert!(error(MYSQL, &[("host", "db.internal")], agent).contains("db.internal:3306 is not in"));
    assert!(error(MYSQL, &[("host", "10.0.0.5")], agent).contains("not in this agent's"));
    let none = PortTargets::parse::<&str>(&[]).unwrap();
    assert!(error(REDIS, &[], Place::Agent(Some(&none))).contains("is empty"));
}

#[test]
fn the_password_never_shows() {
    let target = Target {
        host: "127.0.0.1".into(),
        port: 5432,
        user: Some("monitorflow".into()),
        password: Some("hunter22".into()),
        tls: Tls::Prefer,
    };
    assert!(!format!("{target:?}").contains("hunter22"));
    let sample = down(
        Some(&target),
        &Failure::new(Reason::Login, "password hunter22 was refused"),
    );
    assert_eq!(sample.sensor, "up");
    assert_eq!(sample.value, 0.0);
    assert_eq!(sample.labels["reason"], "login");
    assert_eq!(sample.labels["error"], "password *** was refused");
}

#[test]
fn labels_are_bounded() {
    assert_eq!(label("a\nb\tc", 10), "abc");
    assert_eq!(label(&"é".repeat(200), 201).len(), 200);
}

#[tokio::test]
async fn a_timeout_is_a_failure() {
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_millis(400);
    let failure = within(deadline, async {
        tokio::time::sleep(std::time::Duration::from_secs(5)).await;
        Ok::<(), Failure>(())
    })
    .await
    .unwrap_err();
    assert_eq!(failure.reason, Reason::Timeout);
}

#[tokio::test]
async fn a_closed_port_reports_up_zero() {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port().to_string();
    drop(listener);
    for kind in NETWORK_KINDS {
        let sensor = build(kind, &params(&[("port", &port)]), Place::Relay).unwrap();
        let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(5);
        let samples = sensor.collect(deadline).await.unwrap();
        assert_eq!(samples.len(), 1, "{kind}: {samples:?}");
        assert_eq!(samples[0].sensor, "up");
        assert_eq!(samples[0].value, 0.0);
        assert_eq!(samples[0].labels["reason"], "connect", "{kind}");
    }
}

const INFO: &str = "# Server\r\nredis_version:7.2.4\r\n\r\n# Clients\r\nconnected_clients:12\r\nblocked_clients:1\r\n\
# Memory\r\nused_memory:800\r\nmaxmemory:1000\r\nmem_fragmentation_ratio:1.20\r\n\
# Persistence\r\nrdb_last_bgsave_status:err\r\naof_enabled:0\r\n\
# Stats\r\nrejected_connections:0\r\nevicted_keys:5\r\nkeyspace_hits:90\r\nkeyspace_misses:10\r\n\
# Replication\r\nrole:master\r\nconnected_slaves:1\r\nmaster_repl_offset:1000\r\n\
slave0:ip=10.0.0.2,port=6379,state=online,offset=400,lag=0\r\n\
# Keyspace\r\ndb0:keys=3,expires=0,avg_ttl=0\r\ndb2:keys=4,expires=1,avg_ttl=0\r\n";

fn value(samples: &[monitorflow_sensors::Sample], metric: &str) -> Option<f64> {
    samples.iter().find(|s| s.sensor == metric).map(|s| s.value)
}

#[test]
fn redis_info_becomes_samples() {
    let rates = Rates::default();
    let mut round = rates.round();
    let first = redis::samples(&redis::parse_info(INFO), 0.01, &mut round);
    rates.finish(round);
    assert_eq!(value(&first, "up"), Some(1.0));
    let up = first.iter().find(|s| s.sensor == "up").unwrap();
    assert_eq!(up.labels["version"], "7.2.4");
    assert_eq!(up.labels["role"], "primary");
    assert_eq!(value(&first, "clients"), Some(12.0));
    assert_eq!(value(&first, "memory_usage_ratio"), Some(0.8));
    assert_eq!(value(&first, "keys"), Some(7.0));
    assert_eq!(value(&first, "last_save_ok"), Some(0.0));
    assert_eq!(value(&first, "aof_write_ok"), None);
    assert_eq!(value(&first, "replicas"), Some(1.0));
    let lag = first
        .iter()
        .find(|s| s.sensor == "replica_lag_bytes")
        .unwrap();
    assert_eq!((lag.instance.as_str(), lag.value), ("10.0.0.2:6379", 600.0));
    // Changes need a second collection.
    assert_eq!(value(&first, "evicted_keys"), None);
    let later = INFO
        .replace("evicted_keys:5", "evicted_keys:8")
        .replace("keyspace_hits:90", "keyspace_hits:170")
        .replace("keyspace_misses:10", "keyspace_misses:30");
    let mut round = rates.round();
    let second = redis::samples(&redis::parse_info(&later), 0.01, &mut round);
    assert_eq!(value(&second, "evicted_keys"), Some(3.0));
    assert_eq!(value(&second, "keyspace_hit_ratio"), Some(0.8));

    let replica = "role:slave\r\nmaster_link_status:down\r\nmaster_last_io_seconds_ago:-1\r\n";
    let samples = redis::samples(&redis::parse_info(replica), 0.0, &mut rates.round());
    assert_eq!(value(&samples, "primary_link_up"), Some(0.0));
    assert_eq!(value(&samples, "primary_last_io_seconds"), None);
    assert_eq!(value(&samples, "memory_usage_ratio"), None);
}

#[test]
fn docker_containers_become_samples() {
    let inspected = json!({
        "RestartCount": 2,
        "State": {"Status": "running", "Running": true, "Health": {"Status": "unhealthy"}},
        "HostConfig": {"RestartPolicy": {"Name": "unless-stopped"}}
    });
    let state = docker::Container::read(&inspected).unwrap();
    assert!(state.watched());
    let stats = |cpu: f64, system: f64| {
        json!({
            "memory_stats": {"usage": 600.0, "limit": 1000.0, "stats": {"inactive_file": 100.0}},
            "cpu_stats": {"cpu_usage": {"total_usage": cpu}, "system_cpu_usage": system}
        })
    };
    let rates = Rates::default();
    let mut round = rates.round();
    let first = docker::container_samples(
        "web",
        "nginx:1",
        "abc",
        &state,
        Some(&stats(10.0, 100.0)),
        &mut round,
    );
    rates.finish(round);
    assert_eq!(value(&first, "container_running"), Some(1.0));
    assert_eq!(value(&first, "container_health"), Some(2.0));
    assert_eq!(value(&first, "container_memory_bytes"), Some(500.0));
    assert_eq!(value(&first, "container_memory_ratio"), Some(0.5));
    assert_eq!(value(&first, "container_restarts"), None);
    assert_eq!(value(&first, "container_cpu_ratio"), None);
    let running = first
        .iter()
        .find(|s| s.sensor == "container_running")
        .unwrap();
    assert_eq!(running.instance, "web");
    assert_eq!(running.labels["image"], "nginx:1");

    let again = docker::Container {
        restarts: 3.0,
        ..state.clone()
    };
    let mut round = rates.round();
    let second = docker::container_samples(
        "web",
        "nginx:1",
        "abc",
        &again,
        Some(&stats(30.0, 200.0)),
        &mut round,
    );
    assert_eq!(value(&second, "container_restarts"), Some(1.0));
    assert_eq!(value(&second, "container_cpu_ratio"), Some(0.2));

    // A one-off container that exited is not watched; one meant to run is.
    let exited = |policy: &str| {
        docker::Container::read(&json!({
            "State": {"Status": "exited", "Running": false},
            "HostConfig": {"RestartPolicy": {"Name": policy}}
        }))
        .unwrap()
    };
    assert!(!exited("no").watched());
    assert!(!exited("on-failure").watched());
    assert!(exited("always").watched());
}

#[test]
fn sql_server_readings_become_samples() {
    use mssql::{Readings, Replica, ReplicaDatabase};
    let counter = |object: &str, name: &str, instance: &str, value: f64| {
        (
            (object.to_owned(), name.to_owned(), instance.to_owned()),
            value,
        )
    };
    let mut readings = Readings {
        version: "16.0.4325.4".into(),
        edition: "Standard Edition (64-bit)".into(),
        max_connections: Some(100.0),
        counters: [
            counter("General Statistics", "User Connections", "", 25.0),
            counter("General Statistics", "Processes blocked", "", 2.0),
            counter("SQL Statistics", "Batch Requests/sec", "", 1000.0),
            counter("Buffer Manager", "Page life expectancy", "", 1200.0),
            counter("Buffer Manager", "Buffer cache hit ratio", "", 99.0),
            counter("Buffer Manager", "Buffer cache hit ratio base", "", 100.0),
            counter("Locks", "Number of Deadlocks/sec", "_Total", 4.0),
            counter("Databases", "Data File(s) Size (KB)", "shop", 2048.0),
            counter("Databases", "Log File(s) Size (KB)", "shop", 1000.0),
            counter("Databases", "Log File(s) Used Size (KB)", "shop", 250.0),
            counter("Databases", "Data File(s) Size (KB)", "_Total", 9999.0),
            counter(
                "Databases",
                "Data File(s) Size (KB)",
                "mssqlsystemresource",
                40.0,
            ),
        ]
        .into_iter()
        .collect(),
        longest_blocked_ms: Some(4500.0),
        longest_query_ms: Some(0.0),
        replicas: vec![Replica {
            group: "sales".into(),
            server: "SQL2".into(),
            role: 2,
            connected: false,
            health: 1,
        }],
        replica_databases: vec![ReplicaDatabase {
            group: "sales".into(),
            server: "SQL2".into(),
            database: "shop".into(),
            log_send_queue_kb: Some(10.0),
            redo_queue_kb: None,
            lag_seconds: Some(42.0),
        }],
    };
    let rates = Rates::default();
    let mut round = rates.round();
    let first = mssql::samples(&readings, 0.02, &mut round);
    rates.finish(round);
    let up = first.iter().find(|s| s.sensor == "up").unwrap();
    assert_eq!(up.labels["version"], "16.0.4325.4");
    assert_eq!(value(&first, "connections_ratio"), Some(0.25));
    assert_eq!(value(&first, "page_life_expectancy_seconds"), Some(1200.0));
    assert_eq!(value(&first, "buffer_cache_hit_ratio"), Some(0.99));
    assert_eq!(value(&first, "processes_blocked"), Some(2.0));
    assert_eq!(value(&first, "longest_blocked_seconds"), Some(4.5));
    // Counters since start need a second collection.
    assert_eq!(value(&first, "batch_requests_per_second"), None);
    assert_eq!(value(&first, "deadlocks"), None);
    // Databases of SQL Server's own are left out.
    let sizes: Vec<_> = first
        .iter()
        .filter(|s| s.sensor == "database_size_bytes")
        .map(|s| (s.instance.as_str(), s.value))
        .collect();
    assert_eq!(sizes, [("shop", 2048.0 * 1024.0)]);
    assert_eq!(value(&first, "log_used_ratio"), Some(0.25));
    let replica = first
        .iter()
        .find(|s| s.sensor == "ag_replica_connected")
        .unwrap();
    assert_eq!(
        (replica.instance.as_str(), replica.value),
        ("sales/SQL2", 0.0)
    );
    assert_eq!(replica.labels["role"], "secondary");
    assert_eq!(value(&first, "ag_replica_healthy"), Some(0.0));
    let lag = first
        .iter()
        .find(|s| s.sensor == "ag_secondary_lag_seconds")
        .unwrap();
    assert_eq!(
        (lag.instance.as_str(), lag.value),
        ("sales/SQL2/shop", 42.0)
    );
    assert_eq!(value(&first, "ag_log_send_queue_bytes"), Some(10240.0));
    assert_eq!(value(&first, "ag_redo_queue_bytes"), None);

    readings.counters.insert(
        (
            "Locks".into(),
            "Number of Deadlocks/sec".into(),
            "_Total".into(),
        ),
        5.0,
    );
    let mut round = rates.round();
    let second = mssql::samples(&readings, 0.02, &mut round);
    assert_eq!(value(&second, "deadlocks"), Some(1.0));
    assert!(value(&second, "batch_requests_per_second").is_some());
}

#[test]
fn sql_server_errors_say_why() {
    use mssql::{failure, TdsError};
    let server = |number| TdsError::Server {
        number,
        message: "no".into(),
    };
    assert_eq!(failure(&server(18456)).reason, Reason::Login);
    assert_eq!(failure(&server(300)).reason, Reason::Permission);
    assert_eq!(failure(&server(208)).reason, Reason::Protocol);
    assert_eq!(
        failure(&TdsError::Tls("handshake".into())).reason,
        Reason::Tls
    );
    let refused = std::io::Error::from(std::io::ErrorKind::ConnectionRefused);
    assert_eq!(failure(&TdsError::Io(refused)).reason, Reason::Connect);
    assert!(failure(&server(18456)).error.contains("error 18456"));
}
