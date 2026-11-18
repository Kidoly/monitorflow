//! The Zabbix reader against real exports of Zabbix 6.0, 6.4 and 7.0
//! (written by `deploy/import-test/zabbix/setup.sh`), and against a live
//! Zabbix in an ignored test:
//!
//! ```text
//! deploy/import-test/zabbix/setup.sh
//! MF_ZABBIX_URL=http://127.0.0.1:18080 MF_ZABBIX_TOKEN=... \
//!   cargo test -p monitorflow-import --test zabbix -- --ignored
//! ```

use std::collections::BTreeMap;
use std::path::Path;

use chrono::{DateTime, Utc};
use monitorflow_import::model::{
    Alert, Check, Direction, Fidelity, Inventory, Judged, Measure, Os, Severity, SilenceScope,
    Target, Threshold, Watch, WebMethod, Website, WindowFunction,
};
use monitorflow_import::source::{check_input, Connection, File};
use monitorflow_import::zabbix::Zabbix;
use monitorflow_import::{Context, Input, Source};

fn fixture(version: &str, name: &str) -> File {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/zabbix")
        .join(version)
        .join(name);
    File {
        name: name.to_owned(),
        content: std::fs::read(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display())),
    }
}

fn now() -> DateTime<Utc> {
    DateTime::parse_from_rfc3339("2026-11-17T10:00:00Z")
        .unwrap()
        .with_timezone(&Utc)
}

async fn read(files: Vec<File>) -> Inventory {
    let input = Input::Files(files);
    check_input(&Zabbix.info(), &input).unwrap();
    Zabbix.read(input, &Context::new(now())).await.unwrap()
}

fn check<'a>(inventory: &'a Inventory, host: &str, watch: &Watch) -> &'a Check {
    inventory
        .checks
        .iter()
        .find(|c| c.host == host && c.watch == *watch)
        .unwrap_or_else(|| panic!("no {watch:?} check on {host}: {:#?}", inventory.checks))
}

fn thresholds<'a>(inventory: &'a Inventory, trigger: &str) -> Vec<&'a Threshold> {
    inventory
        .thresholds
        .iter()
        .filter(|t| t.origin.name == trigger)
        .collect()
}

fn why(inventory: &Inventory, kind: &str, name: &str) -> String {
    inventory
        .unmovable
        .iter()
        .find(|u| u.origin.kind == kind && u.origin.name == name)
        .map(|u| u.why.code.clone())
        .unwrap_or_else(|| panic!("{kind} {name} is not unmovable: {:#?}", inventory.unmovable))
}

fn note(fidelity: &Fidelity) -> Option<&str> {
    match fidelity {
        Fidelity::Exact => None,
        Fidelity::Approximated(note) => Some(note.code.as_str()),
    }
}

/// What the hosts export of every version and format says, with the
/// templates read from the export or from stock knowledge.
fn hosts_and_custom_items(inventory: &Inventory, live: bool) {
    let web = inventory.host("web-01").unwrap();
    assert_eq!(web.visible_name.as_deref(), Some("Web server 01"));
    assert_eq!(web.addresses, ["mf-import-zbx-web-01"]);
    assert_eq!(web.os, Os::Debian);
    assert_eq!(web.groups, ["Linux servers", "Web"]);
    assert_eq!(web.origin.kind, "host");

    // Custom items of db-01.
    let port = check(
        inventory,
        "db-01",
        &Watch::Port {
            address: None,
            port: 5432,
        },
    );
    assert_eq!(port.interval_s, Some(30));
    let alert = port.alert.as_ref().unwrap();
    assert_eq!(alert.severity, Severity::Critical);
    assert_eq!(alert.for_s, 180);
    assert_eq!(port.fidelity, Fidelity::Exact);
    assert_eq!(port.origin.detail.as_deref(), Some("net.tcp.port[,5432]"));

    let log = check(
        inventory,
        "db-01",
        &Watch::Log {
            path: "/var/log/syslog".into(),
            pattern: "error".into(),
        },
    );
    assert_eq!(log.alert.as_ref().unwrap().severity, Severity::Warning);

    let service = check(
        inventory,
        "db-01",
        &Watch::Service {
            name: "postgresql.service".into(),
        },
    );
    // Average becomes a warning, with a note.
    assert_eq!(service.alert.as_ref().unwrap().severity, Severity::Warning);
    assert_eq!(note(&service.fidelity), Some("severity_mapped"));

    let process = check(
        inventory,
        "db-01",
        &Watch::Service {
            name: "postgres".into(),
        },
    );
    assert_eq!(note(&process.fidelity), Some("process_as_service"));
    assert_eq!(process.alert.as_ref().unwrap().severity, Severity::Critical);

    // Checked from a probe now (ADR-0148).
    let ntp = check(
        inventory,
        "db-01",
        &Watch::UdpPort {
            address: None,
            port: 123,
            query: Some("ntp".into()),
        },
    );
    assert_eq!(ntp.interval_s, Some(300));
    assert_eq!(why(inventory, "item", "Backup age"), "script");
    assert_eq!(why(inventory, "trigger", "Backup is old"), "script");

    // web-01: SSH, the web scenario's two pages on one address.
    let ssh = check(
        inventory,
        "web-01",
        &Watch::Port {
            address: None,
            port: 22,
        },
    );
    assert_eq!(ssh.alert.as_ref().unwrap().severity, Severity::Critical);
    let web_checks: Vec<_> = inventory
        .checks
        .iter()
        .filter(|c| matches!(c.watch, Watch::Web { .. }) && c.origin.kind == "web_scenario")
        .collect();
    assert_eq!(web_checks.len(), 1, "{web_checks:#?}");
    // Its first step, with the codes it requires; the second is noted.
    let Watch::Web(site) = &web_checks[0].watch else {
        unreachable!()
    };
    assert_eq!(site.url, "http://127.0.0.1/");
    assert_eq!(site.status.as_deref(), Some("200"));
    assert!(site.follow_redirects && !site.credentials);
    assert_eq!(note(&web_checks[0].fidelity), Some("web_steps"));
    assert_eq!(web_checks[0].origin.name, "Shop home");
    assert_eq!(web_checks[0].interval_s, Some(120));
    assert_eq!(
        web_checks[0].alert.as_ref().unwrap().severity,
        Severity::Critical
    );

    // Thresholds of custom triggers, with host and global macros.
    let root = thresholds(inventory, "Root file system is filling up");
    assert_eq!(root.len(), 1);
    assert_eq!(
        root[0].measure,
        Measure::DiskUsed {
            mount: Some("/".into())
        }
    );
    assert_eq!(root[0].warning, Some(0.85));
    assert_eq!(root[0].hosts, ["web-01"]);
    // {$DB.CPU.HIGH} is a global macro: exports do not have it.
    if !live {
        assert_eq!(
            why(inventory, "trigger", "Database CPU is high"),
            "unresolved_macro"
        );
        return;
    }
    let cpu = thresholds(inventory, "Database CPU is high");
    assert_eq!(cpu.len(), 1);
    assert_eq!(cpu[0].measure, Measure::CpuUsage);
    assert_eq!(cpu[0].direction, Direction::Above);
    assert_eq!(cpu[0].critical, Some(0.85));
    assert_eq!(cpu[0].for_s, 600);
    assert_eq!(cpu[0].recovery, Some(0.7));
}

/// What the Linux template gives both hosts, whether read from the
/// export or from stock knowledge.
fn linux_template(inventory: &Inventory, stock: bool) {
    for host in ["db-01", "web-01"] {
        for watch in [Watch::Cpu, Watch::Memory, Watch::Disk, Watch::DiskIo] {
            let builtin = inventory
                .checks
                .iter()
                .find(|c| c.host == host && c.watch == watch && c.origin.kind == "template")
                .unwrap_or_else(|| panic!("no {watch:?} of a template on {host}"));
            assert_eq!(builtin.origin.name, "Linux by Zabbix agent");
            assert_eq!(
                note(&builtin.fidelity),
                stock.then_some("zabbix_stock_template")
            );
        }
    }
    // The agent's availability trigger is the offline alert's; Average
    // becomes a warning.
    let agent = check(inventory, "web-01", &Watch::Agent);
    assert_eq!(agent.origin.name, "Linux by Zabbix agent");
    assert_eq!(
        note(&agent.fidelity),
        Some(if stock {
            "zabbix_stock_template"
        } else {
            "severity_mapped"
        })
    );
    assert_eq!(agent.alert.as_ref().unwrap().severity, Severity::Warning);
    assert_eq!(agent.alert.as_ref().unwrap().for_s, 180);

    // {$CPU.UTIL.CRIT} is 80 on web-01, 90 from the template on db-01.
    let cpu = thresholds(inventory, "Linux: High CPU utilization");
    let by_host: BTreeMap<&str, f64> = cpu
        .iter()
        .flat_map(|t| t.hosts.iter().map(|h| (h.as_str(), t.warning.unwrap())))
        .collect();
    assert_eq!(by_host, BTreeMap::from([("db-01", 0.9), ("web-01", 0.8)]));
    // `min(..., 5m)`: the lowest value over 5 minutes (ADR-0153).
    assert!(cpu.iter().all(|t| t.for_s == 0
        && t.judged
            == Judged::Window {
                function: WindowFunction::Min,
                window_s: 300
            }
        && t.measure == Measure::CpuUsage));

    // Memory: 95 on db-01 by a host macro, 90 elsewhere; Average is
    // mapped to a warning.
    let memory = thresholds(inventory, "Linux: High memory utilization");
    let by_host: BTreeMap<&str, f64> = memory
        .iter()
        .flat_map(|t| t.hosts.iter().map(|h| (h.as_str(), t.warning.unwrap())))
        .collect();
    assert_eq!(by_host, BTreeMap::from([("db-01", 0.05), ("web-01", 0.1)]));
    assert!(memory
        .iter()
        .all(|t| t.measure == Measure::MemoryAvailable && t.direction == Direction::Below));

    // Every file system at 90%, and /var/lib/postgresql at 95% on db-01.
    // "Linux: FS [{#FSNAME}]: Space is critically low" since 6.2,
    // "{#FSNAME}: Disk space is critically low" in 6.0.
    let critical: Vec<&Threshold> = inventory
        .thresholds
        .iter()
        .filter(|t| t.origin.name.contains("{#FSNAME}") && t.origin.name.contains("critically low"))
        .collect();
    let every = critical
        .iter()
        .find(|t| t.measure == Measure::DiskUsed { mount: None })
        .unwrap();
    assert_eq!(every.warning, Some(0.9));
    assert_eq!(every.hosts, ["db-01", "web-01"]);
    let postgres = critical
        .iter()
        .find(|t| {
            t.measure
                == Measure::DiskUsed {
                    mount: Some("/var/lib/postgresql".into()),
                }
        })
        .unwrap();
    assert_eq!(postgres.warning, Some(0.95));
    assert_eq!(postgres.hosts, ["db-01"]);

    assert_eq!(
        why(inventory, "trigger", "Linux: Load average is too high"),
        "per_cpu_load"
    );
    assert_eq!(
        why(inventory, "trigger", "Linux: Lack of available memory"),
        "zabbix_bytes"
    );
    assert_eq!(
        why(inventory, "trigger", "Linux: High swap space usage"),
        "zabbix_no_measure"
    );
}

fn media_types(inventory: &Inventory) {
    // No users' media in an export: the mail server has no recipients.
    assert_eq!(
        why(inventory, "media_type", "Ops mail"),
        "zabbix_no_recipients"
    );
    // Stock SMS: on in 6.0, off since; MonitorFlow sends none either way.
    let sms = why(inventory, "media_type", "SMS");
    assert!(sms == "no_target" || sms == "disabled", "{sms}");
    let webhook = inventory
        .channels
        .iter()
        .find(|c| c.name == "Ops webhook")
        .unwrap();
    let Target::Webhook { url } = &webhook.target else {
        panic!("webhook expected")
    };
    assert_eq!(url.expose(), "https://hooks.example.com/zabbix/0123456789");
    assert!(
        inventory.silences.is_empty(),
        "exports have no maintenances"
    );
}

async fn every_format(version: &str, templates: bool) {
    for format in ["yaml", "xml", "json"] {
        let mut files = vec![
            fixture(version, &format!("hosts.{format}")),
            fixture(version, &format!("media_types.{format}")),
        ];
        if templates && format == "yaml" {
            files.push(fixture(version, "templates.yaml"));
        }
        let inventory = read(files).await;
        let names: Vec<_> = inventory.hosts.iter().map(|h| h.name.as_str()).collect();
        assert_eq!(names, ["db-01", "web-01"]);
        hosts_and_custom_items(&inventory, false);
        linux_template(&inventory, !(templates && format == "yaml"));
        media_types(&inventory);
    }
}

#[tokio::test]
async fn zabbix_7_0_exports() {
    every_format("7.0", false).await;
}

#[tokio::test]
async fn zabbix_6_4_exports() {
    every_format("6.4", false).await;
}

#[tokio::test]
async fn zabbix_6_0_exports() {
    every_format("6.0", true).await;
}

/// The website check of `name`, by its origin.
fn site<'a>(inventory: &'a Inventory, name: &str) -> (&'a Website, &'a Check) {
    inventory
        .checks
        .iter()
        .find_map(|c| match &c.watch {
            Watch::Web(site) if c.origin.name == name => Some((&**site, c)),
            _ => None,
        })
        .unwrap_or_else(|| panic!("no website check from {name}: {:#?}", inventory.checks))
}

#[tokio::test]
async fn web_scenarios_and_http_agent_items_are_website_checks() {
    let inventory = read(vec![fixture("7.0", "web.yaml")]).await;

    // One step: every setting maps, the password is left for later.
    let (shop, check) = site(&inventory, "Shop");
    let mut expected = Website::new("https://shop.example.com/");
    expected.status = Some("200,301".into());
    expected.contains = Some("Welcome to the shop".into());
    expected.follow_redirects = false;
    expected.headers = vec![("Accept-Language".into(), "en".into())];
    expected.username = Some("monitor".into());
    expected.credentials = true;
    expected.timeout_s = Some(10);
    assert_eq!(shop, &expected);
    assert_eq!(check.fidelity, Fidelity::Exact);
    assert_eq!(check.interval_s, Some(60));
    assert_eq!(
        check.alert,
        Some(Alert {
            severity: Severity::Critical,
            for_s: 0,
            no_data: false,
        })
    );
    let slow = thresholds(&inventory, "Shop is slow");
    assert_eq!(slow.len(), 1);
    assert_eq!(
        (
            &slow[0].measure,
            slow[0].direction,
            slow[0].warning,
            slow[0].for_s
        ),
        (
            &Measure::WebResponseTime {
                url: "https://shop.example.com/".into()
            },
            Direction::Above,
            Some(2.0),
            300
        )
    );

    // Two steps: the first is the check.
    let (checkout, check) = site(&inventory, "Checkout");
    assert_eq!(checkout.url, "https://shop.example.com/cart");
    assert_eq!(checkout.status.as_deref(), Some("200"));
    assert_eq!(note(&check.fidelity), Some("web_steps"));
    assert_eq!(check.interval_s, Some(300));
    assert_eq!(
        why(&inventory, "trigger", "Payment page fails"),
        "zabbix_web_step"
    );

    // An HTTP agent item.
    let (api, check) = site(&inventory, "API health");
    assert_eq!(api.url, "https://api.example.com/health?full=1");
    assert_eq!(api.method, WebMethod::Head);
    assert_eq!(api.status.as_deref(), Some("200,204"));
    assert!(!api.follow_redirects);
    assert_eq!(api.headers, vec![("X-Monitor".into(), "zabbix".into())]);
    assert_eq!(api.timeout_s, Some(5));
    assert_eq!(check.fidelity, Fidelity::Exact);
    // `nodata(..., 5m)=1`: a no-data rule on the check (ADR-0153).
    assert_eq!(
        check.alert,
        Some(Alert {
            severity: Severity::Critical,
            for_s: 300,
            no_data: true,
        })
    );
    // A POST, with a password.
    let (login, check) = site(&inventory, "Login API");
    assert_eq!(login.method, WebMethod::Get);
    assert_eq!(login.status.as_deref(), Some("200"));
    assert!(login.credentials);
    assert_eq!(note(&check.fidelity), Some("web_method"));

    // The certificate item, and the template's trigger on its expiry.
    let (certificate, check) = site(&inventory, "Website certificate");
    assert_eq!(certificate.url, "https://www.example.com:8443/");
    assert_eq!(note(&check.fidelity), Some("web_certificate_only"));
    let soon = thresholds(&inventory, "Certificate expires soon");
    assert_eq!(soon.len(), 1);
    assert_eq!(
        (&soon[0].measure, soon[0].direction, soon[0].warning),
        (
            &Measure::CertificateDays {
                url: "https://www.example.com:8443/".into()
            },
            Direction::Below,
            Some(14.0)
        )
    );
}

#[tokio::test]
async fn the_nginx_template_is_missing_from_a_hosts_export() {
    let inventory = read(vec![fixture("7.0", "hosts.yaml")]).await;
    let missing = inventory
        .unmovable
        .iter()
        .find(|u| u.origin.kind == "template" && u.origin.name == "Nginx by Zabbix agent")
        .unwrap();
    assert_eq!(missing.why.code, "zabbix_template_missing");
    assert_eq!(missing.hosts, ["web-01"]);
}

#[tokio::test]
async fn files_that_are_not_exports_are_refused() {
    let error = Zabbix
        .read(
            Input::Files(vec![File {
                name: "x.yaml".into(),
                content: b"groups: []".to_vec(),
            }]),
            &Context::new(now()),
        )
        .await
        .unwrap_err();
    assert_eq!(
        error.to_string(),
        "x.yaml: not a Zabbix configuration export"
    );
}

/// A live Zabbix configured by `deploy/import-test/zabbix/setup.sh`.
#[tokio::test]
#[ignore = "needs a Zabbix: run deploy/import-test/zabbix/setup.sh and set MF_ZABBIX_URL and MF_ZABBIX_TOKEN"]
async fn a_live_zabbix() {
    let url = std::env::var("MF_ZABBIX_URL").expect("MF_ZABBIX_URL");
    let token = std::env::var("MF_ZABBIX_TOKEN").expect("MF_ZABBIX_TOKEN");
    let connection = Connection(BTreeMap::from([
        ("url".to_owned(), url.clone()),
        ("token".to_owned(), token.clone()),
    ]));
    let input = Input::Connect(connection);
    check_input(&Zabbix.info(), &input).unwrap();
    let inventory = Zabbix.read(input, &Context::new(Utc::now())).await.unwrap();
    if std::env::var_os("MF_ZABBIX_PRINT").is_some() {
        println!("{}", serde_json::to_string_pretty(&inventory).unwrap());
    }
    let names: Vec<_> = inventory.hosts.iter().map(|h| h.name.as_str()).collect();
    assert!(
        names.contains(&"web-01") && names.contains(&"db-01"),
        "{names:?}"
    );
    hosts_and_custom_items(&inventory, true);

    // Templates through the API: what the host has, inherited.
    let cpu = thresholds(&inventory, "Linux: High CPU utilization");
    let web = cpu
        .iter()
        .find(|t| t.hosts.contains(&"web-01".to_owned()))
        .unwrap();
    assert_eq!(web.warning, Some(0.8));
    let agent = check(&inventory, "db-01", &Watch::Agent);
    assert_eq!(agent.origin.name, "Linux by Zabbix agent");
    // Nginx's port, from its template's macros.
    let nginx = check(
        &inventory,
        "web-01",
        &Watch::Port {
            address: None,
            port: 80,
        },
    );
    assert_eq!(nginx.origin.kind, "item");

    // Maintenances: the one-time window as a silence, and the weekly one
    // as a maintenance window on Sundays at 02:00, on both hosts of
    // "Linux servers" (ADR-0151).
    let upgrade = inventory
        .silences
        .iter()
        .find(|s| s.origin.name == "Database upgrade")
        .unwrap();
    assert_eq!(upgrade.scope, SilenceScope::Hosts(vec!["db-01".into()]));
    assert_eq!(upgrade.reason, "Database upgrade: PostgreSQL 17");
    assert_eq!((upgrade.ends_at - upgrade.starts_at).num_hours(), 4);
    assert_eq!(upgrade.fidelity, Fidelity::Exact);
    let weekly = inventory
        .windows
        .iter()
        .find(|w| w.origin.name == "Weekly patching")
        .unwrap();
    let SilenceScope::Hosts(hosts) = &weekly.scope else {
        panic!("hosts expected")
    };
    assert!(hosts.contains(&"db-01".into()) && hosts.contains(&"web-01".into()));
    assert_eq!(weekly.rule.weekdays, 1 << 6, "{weekly:?}");
    assert_eq!(weekly.rule.start_time.to_string(), "02:00:00");

    // Channels: the mail server with Admin's two addresses, the webhook,
    // and the chat webhooks from Admin's "send to".
    let mail = inventory
        .channels
        .iter()
        .find(|c| c.name == "Ops mail")
        .unwrap();
    let Target::Email { to, smtp } = &mail.target else {
        panic!("email expected")
    };
    assert_eq!(to, &["admin@example.com", "oncall@example.com"]);
    assert_eq!(smtp.as_ref().unwrap().host, "mail.example.com");
    assert_eq!(smtp.as_ref().unwrap().port, 587);
    assert!(inventory.channels.iter().any(|c| c.name == "Ops webhook"));
    assert!(inventory
        .channels
        .iter()
        .any(|c| matches!(c.target, Target::Discord { .. })));
    // Slack posts with a bot token; 7.0 ships it turned off.
    let slack = why(&inventory, "media_type", "Slack");
    assert!(slack == "chat_token" || slack == "disabled", "{slack}");

    // The token is in nothing the reader returns.
    let shown = serde_json::to_string(&inventory).unwrap();
    assert!(!shown.contains(&token));
    assert!(!format!("{inventory:?}").contains(&token));

    // A wrong token is refused, and not shown back.
    let wrong = "0".repeat(64);
    let error = Zabbix
        .read(
            Input::Connect(Connection(BTreeMap::from([
                (
                    "url".to_owned(),
                    format!("{url}/zabbix.php?action=dashboard.view"),
                ),
                ("token".to_owned(), wrong.clone()),
            ]))),
            &Context::new(Utc::now()),
        )
        .await
        .unwrap_err();
    assert!(
        matches!(error, monitorflow_import::ReadError::Refused(_)),
        "{error}"
    );
    assert!(!error.to_string().contains(&wrong));
}
