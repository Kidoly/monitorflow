//! The Prometheus reader against real files and real API answers, all
//! taken from the stack of `deploy/import-test/prometheus` (Prometheus
//! 3.5, Alertmanager 0.28, node and blackbox exporters).
//!
//! - `tests/fixtures/prometheus/files`: its configuration files, as
//!   uploaded.
//! - `tests/fixtures/prometheus/api`: what its APIs answered, served here
//!   by a small HTTP server on 127.0.0.1.
//! - `the_running_stack`, ignored by default, reads the stack itself:
//!   `MF_PROMETHEUS_URL=http://127.0.0.1:19090
//!   MF_ALERTMANAGER_URL=http://127.0.0.1:19093 cargo test -p
//!   monitorflow-import --test prometheus -- --ignored`.

use std::collections::BTreeMap;
use std::path::Path;
use std::sync::{Arc, Mutex};

use chrono::Utc;
use monitorflow_import::model::{
    Alert, Check, Direction, Fidelity, Inventory, Measure, Os, Severity, SilenceScope, Target,
    Threshold, Watch, WebMethod,
};
use monitorflow_import::prometheus::Prometheus;
use monitorflow_import::source::{self, Connection, File, Input};
use monitorflow_import::{args, Context, Note, ReadError, Source};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

const FIXTURES: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/prometheus");

fn files() -> Vec<File> {
    let dir = Path::new(FIXTURES).join("files");
    let mut paths: Vec<_> = std::fs::read_dir(dir)
        .unwrap()
        .map(|e| e.unwrap().path())
        .collect();
    paths.sort();
    paths
        .iter()
        .map(|p| File {
            name: p.file_name().unwrap().to_string_lossy().into_owned(),
            content: std::fs::read(p).unwrap(),
        })
        .collect()
}

fn context() -> Context {
    Context::new(Utc::now())
}

fn names(inventory: &Inventory) -> Vec<&str> {
    let mut names: Vec<_> = inventory.hosts.iter().map(|h| h.name.as_str()).collect();
    names.sort_unstable();
    names
}

fn check<'a>(inventory: &'a Inventory, host: &str, watch: &Watch) -> &'a Check {
    inventory
        .checks
        .iter()
        .find(|c| c.host == host && &c.watch == watch)
        .unwrap_or_else(|| panic!("no {watch:?} on {host}: {:#?}", inventory.checks))
}

fn threshold<'a>(inventory: &'a Inventory, rule: &str) -> &'a Threshold {
    inventory
        .thresholds
        .iter()
        .find(|t| t.origin.name == rule)
        .unwrap_or_else(|| panic!("no threshold from {rule}"))
}

fn unmovable(inventory: &Inventory, name: &str) -> String {
    inventory
        .unmovable
        .iter()
        .find(|u| u.origin.name == name)
        .map(|u| u.why.code.clone())
        .unwrap_or_else(|| panic!("{name} should not be movable"))
}

fn critical(for_s: u32) -> Option<Alert> {
    Some(Alert {
        severity: Severity::Critical,
        for_s,
    })
}

fn sorted(mut hosts: Vec<String>) -> Vec<String> {
    hosts.sort();
    hosts
}

/// What the files and the API both give: hosts, checks of probes and
/// exporters, alerts and thresholds of the rules.
fn assert_common(inventory: &Inventory) {
    assert_eq!(names(inventory), vec!["db-01", "monitor-01", "web-01"]);
    let web = inventory.host("web-01").unwrap();
    assert_eq!(web.os, Os::Linux);
    assert_eq!(web.addresses, vec!["web-01"]);
    assert!(web.groups.contains(&"node".to_owned()));
    // The blackbox exporter's machine.
    let monitor = inventory.host("monitor-01").unwrap();
    assert!(monitor.groups.contains(&"blackbox-http".to_owned()));
    assert!(monitor.groups.contains(&"blackbox-exporter".to_owned()));

    // Probes of the blackbox exporter, each with BlackboxProbeFailed.
    let web_check = inventory
        .checks
        .iter()
        .find(|c| matches!(&c.watch, Watch::Web(site) if site.url == "http://shop/"))
        .expect("the website check of http://shop/");
    assert_eq!(web_check.host, "monitor-01");
    assert_eq!(web_check.interval_s, Some(15));
    // BlackboxProbeFailed and BlackboxProbeHttpFailure, both critical.
    assert_eq!(web_check.alert, critical(0));
    assert_eq!(web_check.fidelity, Fidelity::Exact);
    let Watch::Web(site) = &web_check.watch else {
        unreachable!()
    };
    assert_eq!(site.status, None, "the exporter's 2xx, the check's default");
    assert_eq!(site.method, WebMethod::Get);
    // BlackboxSlowProbe and BlackboxSslCertificateWillExpireSoon.
    let url = "http://shop/".to_owned();
    let t = threshold(inventory, "BlackboxSlowProbe");
    assert_eq!(
        (&t.measure, t.direction, t.warning, t.for_s, &t.fidelity),
        (
            &Measure::WebResponseTime { url: url.clone() },
            Direction::Above,
            Some(1.0),
            60,
            &Fidelity::Exact
        )
    );
    assert_eq!(t.hosts, ["monitor-01"]);
    let t = threshold(inventory, "BlackboxSslCertificateWillExpireSoon");
    assert_eq!(
        (&t.measure, t.direction, t.warning, &t.fidelity),
        (
            &Measure::CertificateDays { url },
            Direction::Below,
            Some(20.0),
            &Fidelity::Approximated(Note::plain("extra_condition"))
        )
    );
    let port = check(
        inventory,
        "monitor-01",
        &Watch::Port {
            address: Some("shop".into()),
            port: 80,
        },
    );
    assert_eq!(
        (port.alert.clone(), &port.fidelity),
        (critical(0), &Fidelity::Exact)
    );
    check(
        inventory,
        "monitor-01",
        &Watch::Port {
            address: Some("db-01".into()),
            port: 9100,
        },
    );
    // The exporter's own port, with BlackboxExporterDown.
    let exporter = check(
        inventory,
        "monitor-01",
        &Watch::Port {
            address: Some("monitor-01".into()),
            port: 9115,
        },
    );
    assert_eq!(exporter.interval_s, Some(30));
    assert_eq!(
        exporter.alert,
        Some(Alert {
            severity: Severity::Warning,
            for_s: 300
        })
    );
    assert_eq!(
        exporter.fidelity,
        Fidelity::Approximated(Note::new("scrape_as_port", args! {"port" => 9115}))
    );
    // HostDown: the offline alert of both machines. db-01 is also pinged,
    // which the rule on the host itself replaces.
    for host in ["web-01", "db-01"] {
        let agent = check(inventory, host, &Watch::Agent);
        assert_eq!(agent.fidelity, Fidelity::Exact, "{host}");
        assert_eq!(agent.origin.name, "HostDown", "{host}");
        assert!(agent.alert.as_ref().unwrap().severity == Severity::Critical);
    }
    assert_eq!(
        check(inventory, "web-01", &Watch::Agent).alert,
        critical(60)
    );
    // NginxDown, on the web machines only.
    let nginx = Watch::Service {
        name: "nginx.service".into(),
    };
    assert_eq!(check(inventory, "web-01", &nginx).alert, critical(60));
    assert!(!inventory
        .checks
        .iter()
        .any(|c| c.host == "db-01" && c.watch == nginx));

    let both = vec!["db-01".to_owned(), "web-01".to_owned()];
    let t = threshold(inventory, "HostHighCpuLoad");
    assert_eq!(sorted(t.hosts.clone()), both);
    assert_eq!(
        (&t.measure, t.direction, t.warning, t.critical, t.for_s),
        (&Measure::CpuUsage, Direction::Above, Some(0.8), None, 300)
    );
    let t = threshold(inventory, "HostCpuSaturated");
    assert_eq!((t.critical, t.for_s), (Some(0.95), 120));
    let t = threshold(inventory, "HostHighLoad");
    assert_eq!(
        (&t.measure, t.warning, t.for_s),
        (&Measure::CpuLoad { minutes: 15 }, Some(4.0), 900)
    );
    assert_eq!(
        t.fidelity,
        Fidelity::Approximated(Note::new(
            "severity_mapped",
            args! {"severity" => "minor", "mapped" => "warning"}
        ))
    );
    let t = threshold(inventory, "HostOutOfMemory");
    assert_eq!(
        (&t.measure, t.direction, t.warning),
        (&Measure::MemoryAvailable, Direction::Below, Some(0.1))
    );
    let t = threshold(inventory, "HostMemoryUnderPressure");
    assert_eq!((t.direction, t.critical), (Direction::Below, Some(0.05)));
    let t = threshold(inventory, "HostOutOfDiskSpace");
    assert_eq!(
        (&t.measure, t.direction, t.critical, t.for_s),
        (
            &Measure::DiskUsed {
                mount: Some("/".into())
            },
            Direction::Above,
            Some(0.9),
            120
        )
    );
    assert_eq!(
        t.fidelity,
        Fidelity::Approximated(Note::plain("disk_free_as_used"))
    );
    let t = threshold(inventory, "HostUnusualDiskReadLatency");
    assert_eq!(
        (&t.measure, t.warning),
        (
            &Measure::DiskWait {
                device: Some("sda".into())
            },
            Some(0.1)
        )
    );
    assert_eq!(inventory.thresholds.len(), 9);

    // What cannot be moved, and why.
    assert_eq!(unmovable(inventory, "HostSystemSaturated"), "per_cpu_load");
    for rule in [
        "HostDiskMayFillIn24Hours",
        "HostOutOfInodes",
        "HostUnusualNetworkThroughputIn",
        "HostOomKillDetected",
        "HostSystemdServiceCrashed",
    ] {
        assert_eq!(unmovable(inventory, rule), "expression", "{rule}");
    }
    let expression = inventory
        .unmovable
        .iter()
        .find(|u| u.origin.name == "HostOomKillDetected")
        .unwrap();
    assert_eq!(
        expression.origin.detail.as_deref(),
        Some("increase(node_vmstat_oom_kill[1m]) > 0")
    );
    // Docker's resolver, a loopback address no probe reaches.
    assert_eq!(unmovable(inventory, "127.0.0.11"), "no_address");
    // db-01 runs a node exporter and is pinged too: a ping from a probe.
    assert_eq!(
        check(inventory, "db-01", &Watch::Ping { address: None }).fidelity,
        Fidelity::Exact
    );
    assert_eq!(unmovable(inventory, "localhost:9090"), "unknown_host");
    assert_eq!(unmovable(inventory, "route"), "routes");
    // The recording rule is skipped silently.
    assert!(!inventory
        .unmovable
        .iter()
        .any(|u| u.origin.name.starts_with("instance:")));
}

#[tokio::test]
async fn the_reader_is_offered() {
    let reader = source::source("prometheus").expect("listed in sources()");
    let info = reader.info();
    assert_eq!(
        (info.id, info.name, info.many_files),
        ("prometheus", "Prometheus", true)
    );
    assert_eq!(info.extensions, &["yml", "yaml", "json", "rules"]);
    let fields: Vec<_> = info
        .connect
        .iter()
        .map(|f| (f.key, f.secret, f.optional))
        .collect();
    assert_eq!(
        fields,
        vec![
            ("url", false, false),
            ("alertmanager_url", false, true),
            ("token", true, true)
        ]
    );
}

#[tokio::test]
async fn files_of_a_real_setup() {
    let inventory = Prometheus
        .read(Input::Files(files()), &context())
        .await
        .unwrap();
    assert_common(&inventory);
    let web = inventory.host("web-01").unwrap();
    assert_eq!(web.groups, vec!["node", "env=production", "role=web"]);
    let db = inventory.host("db-01").unwrap();
    assert_eq!(db.groups, vec!["node", "env=production", "role=database"]);

    // Channels, with the secrets of the file.
    let channels: Vec<_> = inventory
        .channels
        .iter()
        .map(|c| (c.name.as_str(), c.target.kind()))
        .collect();
    assert_eq!(
        channels,
        vec![
            ("ops-email", "email"),
            ("on-call", "slack"),
            ("web-team 1", "discord"),
            ("web-team 2", "teams"),
            ("automation", "webhook"),
        ]
    );
    let Target::Email {
        to,
        smtp: Some(smtp),
    } = &inventory.channels[0].target
    else {
        panic!("an email channel with its mail server");
    };
    assert_eq!(
        to,
        &vec!["ops@example.com".to_owned(), "admin@example.com".into()]
    );
    assert_eq!(
        (
            smtp.host.as_str(),
            smtp.port,
            smtp.username.as_str(),
            smtp.from.as_str()
        ),
        (
            "mail.example.com",
            587,
            "alertmanager@example.com",
            "alertmanager@example.com"
        )
    );
    assert_eq!(
        smtp.password.as_ref().unwrap().expose(),
        "example-smtp-password"
    );
    let Target::Slack { url } = &inventory.channels[1].target else {
        panic!("slack");
    };
    assert!(url
        .expose()
        .starts_with("https://hooks.slack.com/services/"));
    let Target::Teams { url } = &inventory.channels[3].target else {
        panic!("teams");
    };
    assert!(url.expose().contains(".logic.azure.com/workflows/"));
    assert_eq!(unmovable(&inventory, "on-call"), "no_target");
    // No silences in files, but the muted time intervals of routes are
    // maintenance windows (ADR-0151).
    assert!(inventory.silences.is_empty());
    let backups = inventory
        .windows
        .iter()
        .find(|w| w.reason == "sunday-backups")
        .unwrap();
    assert!(
        matches!(&backups.scope, SilenceScope::Hosts(h) if h.iter().all(|h| h.starts_with("db-")))
    );
    assert_eq!(
        (backups.rule.weekdays, backups.rule.duration_minutes),
        (1 << 6, 240)
    );
    assert_eq!(backups.rule.time_zone.as_deref(), Some("UTC"));
    let batch = inventory
        .windows
        .iter()
        .find(|w| w.reason == "nightly-batch")
        .unwrap();
    assert_eq!(batch.scope, SilenceScope::Hosts(vec!["web-01".into()]));
    assert_eq!(batch.checks, vec![Watch::Cpu]);
    assert_eq!(batch.rule.time_zone.as_deref(), Some("Europe/Paris"));
    assert_eq!(batch.fidelity, Fidelity::Exact);
}

#[tokio::test]
async fn missing_files_are_reported() {
    let only_config: Vec<File> = files()
        .into_iter()
        .filter(|f| f.name == "prometheus.yml")
        .collect();
    let inventory = Prometheus
        .read(Input::Files(only_config), &context())
        .await
        .unwrap();
    // web-01 is a static target; db-01's node exporter came from a file
    // not uploaded, so the address pinged alone becomes a device.
    assert_eq!(names(&inventory), vec!["db-01", "monitor-01", "web-01"]);
    assert!(inventory.host("db-01").is_some_and(|h| h.agentless));
    let missing: Vec<_> = inventory
        .unmovable
        .iter()
        .filter(|u| u.why.code == "file_missing")
        .map(|u| u.why.args["file"].as_str())
        .collect();
    assert_eq!(
        missing,
        vec![
            "/etc/prometheus/targets/nodes-*.json",
            "/etc/prometheus/targets/probes-http.yml",
            "/etc/prometheus/rules/*.yml",
        ]
    );
    let err = Prometheus
        .read(
            Input::Files(vec![File {
                name: "notes.yml".into(),
                content: b"title: not a monitoring file".to_vec(),
            }]),
            &context(),
        )
        .await
        .unwrap_err();
    assert!(
        matches!(err, ReadError::File { ref file, .. } if file == "notes.yml"),
        "{err}"
    );
}

/// A request the test server received.
#[derive(Debug, Clone)]
struct Seen {
    method: String,
    path: String,
    authorization: Option<String>,
}

/// An HTTP server on 127.0.0.1 answering Prometheus's and Alertmanager's
/// paths with the captured answers, or every request with `status`.
async fn serve(status: u16) -> (String, Arc<Mutex<Vec<Seen>>>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let seen = Arc::new(Mutex::new(Vec::new()));
    let log = seen.clone();
    let answers: BTreeMap<&str, &str> = BTreeMap::from([
        ("/api/v1/status/config", "status_config.json"),
        ("/api/v1/targets", "targets.json"),
        ("/api/v1/rules", "rules.json"),
        ("/api/v1/query", "uname.json"),
        ("/api/v2/status", "am_status.json"),
        ("/api/v2/silences", "am_silences.json"),
    ]);
    tokio::spawn(async move {
        loop {
            let Ok((mut stream, _)) = listener.accept().await else {
                return;
            };
            let mut request = Vec::new();
            let mut buffer = [0u8; 4096];
            while !request.windows(4).any(|w| w == b"\r\n\r\n") {
                match stream.read(&mut buffer).await {
                    Ok(0) | Err(_) => break,
                    Ok(n) => request.extend_from_slice(&buffer[..n]),
                }
            }
            let text = String::from_utf8_lossy(&request).into_owned();
            let mut first = text.lines().next().unwrap_or_default().split(' ');
            let method = first.next().unwrap_or_default().to_owned();
            let path = first.next().unwrap_or_default().to_owned();
            let authorization = text
                .lines()
                .find(|l| l.to_ascii_lowercase().starts_with("authorization:"))
                .map(|l| l["authorization:".len()..].trim().to_owned());
            log.lock().unwrap().push(Seen {
                method,
                path: path.clone(),
                authorization,
            });
            let route = path.split('?').next().unwrap_or_default();
            let (code, body) = match (status, answers.get(route)) {
                (200, Some(file)) => (
                    "200 OK",
                    std::fs::read(Path::new(FIXTURES).join("api").join(file)).unwrap(),
                ),
                (200, None) => ("404 Not Found", b"{}".to_vec()),
                (401, _) => ("401 Unauthorized", b"{}".to_vec()),
                _ => ("500 Internal Server Error", b"{}".to_vec()),
            };
            let head = format!(
                "HTTP/1.1 {code}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                body.len()
            );
            let _ = stream.write_all(head.as_bytes()).await;
            let _ = stream.write_all(&body).await;
        }
    });
    (format!("http://{address}"), seen)
}

fn connect(url: &str, alertmanager: Option<&str>, token: Option<&str>) -> Input {
    let mut values = BTreeMap::from([("url".to_owned(), url.to_owned())]);
    if let Some(am) = alertmanager {
        values.insert("alertmanager_url".into(), am.into());
    }
    if let Some(token) = token {
        values.insert("token".into(), token.into());
    }
    Input::Connect(Connection(values))
}

/// What only the APIs give: names from node_uname_info, hidden secrets,
/// silences.
fn assert_live(inventory: &Inventory) {
    assert_common(inventory);
    // Alertmanager's API hides the mail password and every URL secret.
    let channels: Vec<_> = inventory
        .channels
        .iter()
        .map(|c| (c.name.as_str(), c.target.kind()))
        .collect();
    assert_eq!(channels, vec![("ops-email", "email")]);
    let Target::Email {
        smtp: Some(smtp), ..
    } = &inventory.channels[0].target
    else {
        panic!("email");
    };
    assert_eq!((smtp.host.as_str(), smtp.port), ("mail.example.com", 587));
    assert!(smtp.password.is_none());
    let hidden: Vec<_> = inventory
        .unmovable
        .iter()
        .filter(|u| u.why.code == "secret_hidden")
        .map(|u| {
            (
                u.origin.name.as_str(),
                u.origin.detail.as_deref().unwrap_or(""),
            )
        })
        .collect();
    assert_eq!(
        hidden,
        vec![
            ("on-call", "slack_configs"),
            ("web-team", "discord_configs"),
            ("web-team", "msteams_configs"),
            ("automation", "webhook_configs"),
        ]
    );

    let silence = |reason: &str| {
        inventory
            .silences
            .iter()
            .find(|s| s.reason == reason)
            .unwrap_or_else(|| panic!("no silence {reason}"))
    };
    let upgrade = silence("Database upgrade");
    assert_eq!(upgrade.scope, SilenceScope::Hosts(vec!["db-01".into()]));
    assert_eq!(upgrade.fidelity, Fidelity::Exact);
    assert!(upgrade.ends_at > upgrade.starts_at);
    let batch = silence("Nightly batch on the web server");
    assert_eq!(batch.scope, SilenceScope::Hosts(vec!["web-01".into()]));
    // The alert rule's check only, not the whole server (ADR-0151).
    assert_eq!(batch.checks, vec![Watch::Cpu]);
    assert_eq!(batch.fidelity, Fidelity::Exact);
    assert_eq!(unmovable(inventory, "Old staging machines"), "no_hosts");
}

#[tokio::test]
async fn the_api_of_a_real_setup() {
    let (url, seen) = serve(200).await;
    let inventory = Prometheus
        .read(connect(&url, Some(&url), Some("test-token")), &context())
        .await
        .unwrap();
    assert_live(&inventory);
    let seen = seen.lock().unwrap().clone();
    let paths: Vec<_> = seen.iter().map(|s| s.path.as_str()).collect();
    assert_eq!(
        paths,
        vec![
            "/api/v1/status/config",
            "/api/v1/query?query=node_uname_info",
            "/api/v1/targets?state=active",
            "/api/v1/rules?type=alert",
            "/api/v2/status",
            "/api/v2/silences",
        ]
    );
    // Read only, with the token on every request.
    for request in &seen {
        assert_eq!(request.method, "GET");
        assert_eq!(request.authorization.as_deref(), Some("Bearer test-token"));
    }
}

#[tokio::test]
async fn without_alertmanager_or_token() {
    let (url, seen) = serve(200).await;
    let inventory = Prometheus
        .read(connect(&url, None, None), &context())
        .await
        .unwrap();
    assert_eq!(names(&inventory), vec!["db-01", "monitor-01", "web-01"]);
    assert!(inventory.channels.is_empty() && inventory.silences.is_empty());
    let seen = seen.lock().unwrap();
    assert_eq!(seen.len(), 4);
    assert!(seen.iter().all(|s| s.authorization.is_none()));
}

#[tokio::test]
async fn refusals_and_errors_never_show_the_token() {
    let (url, _) = serve(401).await;
    let err = Prometheus
        .read(connect(&url, None, Some("s3cret-token")), &context())
        .await
        .unwrap_err();
    assert!(matches!(err, ReadError::Refused(_)), "{err}");
    assert!(!err.to_string().contains("s3cret-token"));
    let (url, _) = serve(500).await;
    let err = Prometheus
        .read(connect(&url, None, Some("s3cret-token")), &context())
        .await
        .unwrap_err();
    assert!(matches!(err, ReadError::Connect(_)), "{err}");
    assert!(!err.to_string().contains("s3cret-token"));
    // Nothing listens there.
    let err = Prometheus
        .read(connect("http://127.0.0.1:1", None, None), &context())
        .await
        .unwrap_err();
    assert!(matches!(err, ReadError::Connect(_)), "{err}");
}

/// The stack of `deploy/import-test/prometheus`, running.
#[tokio::test]
#[ignore = "needs deploy/import-test/prometheus/setup.sh running"]
async fn the_running_stack() {
    let url =
        std::env::var("MF_PROMETHEUS_URL").unwrap_or_else(|_| "http://127.0.0.1:19090".into());
    let am =
        std::env::var("MF_ALERTMANAGER_URL").unwrap_or_else(|_| "http://127.0.0.1:19093".into());
    let inventory = Prometheus
        .read(connect(&url, Some(&am), None), &context())
        .await
        .unwrap();
    assert_live(&inventory);
}
