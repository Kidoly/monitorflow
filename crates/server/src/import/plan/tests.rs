use chrono::TimeZone;
use monitorflow_common::redact::Secret;
use monitorflow_import::model::{
    Alert, Channel, Host, Origin, Silence, Smtp, Threshold, Unmovable, Website,
};

use super::*;

fn now() -> DateTime<Utc> {
    Utc.with_ymd_and_hms(2026, 11, 17, 12, 0, 0).unwrap()
}

fn host(key: &str, name: &str, os: Os) -> Host {
    Host {
        key: key.into(),
        name: name.into(),
        visible_name: None,
        addresses: vec!["10.0.0.5".into()],
        os,
        groups: vec!["Linux servers".into()],
        origin: Origin::new("host", name),
    }
}

fn check(host: &str, watch: Watch, alert: Option<Severity>) -> Check {
    Check {
        host: host.into(),
        watch,
        interval_s: Some(60),
        alert: alert.map(|severity| Alert {
            severity,
            for_s: 120,
        }),
        origin: Origin::new("item", "an item"),
        fidelity: Fidelity::Exact,
    }
}

fn threshold(
    hosts: &[&str],
    measure: Measure,
    warning: Option<f64>,
    critical: Option<f64>,
    name: &str,
) -> Threshold {
    Threshold {
        hosts: hosts.iter().map(|h| (*h).to_owned()).collect(),
        measure,
        direction: Direction::Above,
        warning,
        critical,
        for_s: 300,
        recovery: None,
        origin: Origin::new("trigger", name),
        fidelity: Fidelity::Exact,
    }
}

fn site() -> Uuid {
    Uuid::from_u128(1)
}

fn run(inventory: &Inventory, existing: &Existing) -> Planned {
    plan(inventory, site(), false, existing, now())
}

fn by_id<'a>(planned: &'a Planned, id: &str) -> &'a Item {
    planned
        .items
        .iter()
        .find(|i| i.id == id)
        .unwrap_or_else(|| {
            panic!(
                "no item {id} in {:#?}",
                planned.items.iter().map(|i| &i.id).collect::<Vec<_>>()
            )
        })
}

#[test]
fn hosts_become_servers_waiting_for_their_agent() {
    let inventory = Inventory {
        hosts: vec![
            host("1", "Web-01.example.com", Os::Debian),
            host("2", "db 01", Os::Windows),
            // The same name twice is one server.
            host("3", "web-01.example.com", Os::Linux),
        ],
        ..Inventory::default()
    };
    let planned = run(&inventory, &Existing::default());
    assert_eq!(planned.items.len(), 2);
    let web = by_id(&planned, "server:web-01.example.com");
    assert_eq!(web.list, List::Exact);
    assert!(!web.present);
    assert_eq!(
        web.create,
        Some(Create::Server {
            peer_id: "web-01.example.com".into(),
            hostname: "Web-01.example.com".into(),
            os_family: "debian".into(),
            addresses: vec!["10.0.0.5".into()],
        })
    );
    let Some(Create::Server {
        peer_id, os_family, ..
    }) = &by_id(&planned, "server:db-01").create
    else {
        panic!()
    };
    assert_eq!((peer_id.as_str(), os_family.as_str()), ("db-01", "windows"));
}

#[test]
fn servers_monitorflow_has_are_present_and_keep_their_id() {
    let inventory = Inventory {
        hosts: vec![host("1", "web-01", Os::Linux)],
        checks: vec![check(
            "1",
            Watch::Port {
                address: None,
                port: 22,
            },
            Some(Severity::Critical),
        )],
        ..Inventory::default()
    };
    let existing = Existing {
        servers: vec![ExistingServer {
            peer_id: "Web-01".into(),
            hostname: "web-01".into(),
            site_id: site(),
            may_change: true,
        }],
        checks: HashSet::from([("web-01".to_owned(), "port_22".to_owned())]),
        ..Existing::default()
    };
    let planned = run(&inventory, &existing);
    let server = by_id(&planned, "server:web-01");
    assert!(server.present);
    let check = by_id(&planned, "check:web-01:port_22");
    assert!(check.present, "the check exists already");
    // An existing server is not an item to tick: the check needs nothing.
    assert!(check.needs.is_empty());
    let Some(Create::Check { server, .. }) = &check.create else {
        panic!()
    };
    assert_eq!(server, "Web-01");
    let alert = by_id(&planned, "threshold:web-01:monitorflow_port_22_up");
    assert!(!alert.present);
    assert_eq!(alert.needs, vec!["check:web-01:port_22".to_owned()]);
}

#[test]
fn checks_become_sensor_rows_with_their_alert() {
    let inventory = Inventory {
        hosts: vec![host("l", "lin", Os::Debian), host("w", "win", Os::Windows)],
        checks: vec![
            check(
                "l",
                Watch::Service {
                    name: "nginx.service".into(),
                },
                Some(Severity::Critical),
            ),
            check(
                "w",
                Watch::Service {
                    name: "W3SVC".into(),
                },
                Some(Severity::Warning),
            ),
            check(
                "l",
                Watch::Log {
                    path: "/var/log/syslog".into(),
                    pattern: "error".into(),
                },
                Some(Severity::Warning),
            ),
            check(
                "l",
                Watch::Port {
                    address: Some("db.example".into()),
                    port: 5432,
                },
                None,
            ),
            check(
                "l",
                Watch::web(Website::new("https://shop.example/health")),
                Some(Severity::Critical),
            ),
        ],
        ..Inventory::default()
    };
    let planned = run(&inventory, &Existing::default());
    let nginx = by_id(&planned, "check:lin:service_nginx");
    assert_eq!(nginx.list, List::Exact);
    let Some(Create::Check {
        kind,
        params,
        interval_ms,
        ..
    }) = &nginx.create
    else {
        panic!()
    };
    assert_eq!((kind.as_str(), *interval_ms), ("systemd", 60_000));
    assert_eq!(params["units"], "nginx.service");
    assert_eq!(nginx.needs, vec!["server:lin".to_owned()]);
    let Some(Create::Threshold {
        metric,
        comparison,
        warning,
        critical,
        duration_s,
        ..
    }) = &by_id(&planned, "threshold:lin:monitorflow_service_nginx_active").create
    else {
        panic!()
    };
    assert_eq!(
        (
            metric.as_str(),
            comparison.as_str(),
            *warning,
            *critical,
            *duration_s
        ),
        (
            "monitorflow_service_nginx_active",
            "<",
            None,
            Some(1.0),
            120
        )
    );
    let Some(Create::Check { kind, params, .. }) =
        &by_id(&planned, "check:win:service_w3svc").create
    else {
        panic!()
    };
    assert_eq!(
        (kind.as_str(), params["services"].as_str()),
        ("windows_service", Some("W3SVC"))
    );
    let Some(Create::Threshold {
        warning, critical, ..
    }) = &by_id(&planned, "threshold:win:monitorflow_service_w3svc_active").create
    else {
        panic!()
    };
    assert_eq!((*warning, *critical), (Some(1.0), None));
    let log = by_id(&planned, "check:lin:log_syslog");
    assert_eq!(log.list, List::Approximated);
    assert_eq!(log.note.as_ref().unwrap().code, "log_dirs");
    let Some(Create::Threshold { comparison, .. }) =
        &by_id(&planned, "threshold:lin:monitorflow_log_syslog_matches").create
    else {
        panic!()
    };
    assert_eq!(comparison, ">");
    let remote = by_id(&planned, "check:lin:port_db_example_5432");
    let Some(Create::Check { params, .. }) = &remote.create else {
        panic!()
    };
    assert_eq!(
        (params["host"].as_str(), params["port"].as_str()),
        (Some("db.example"), Some("5432"))
    );
    assert!(!planned
        .items
        .iter()
        .any(|i| i.id.starts_with("threshold:lin:monitorflow_port_db")));
    let web = by_id(&planned, "check:lin:port_shop_example_443");
    assert_eq!(web.list, List::Approximated);
    assert_eq!(web.note.as_ref().unwrap().code, "web_as_port");
}

#[test]
fn checks_every_server_has_are_one_line() {
    let mut cpu = check("1", Watch::Cpu, None);
    cpu.origin = Origin::new("item", "CPU utilization");
    let mut cpu2 = cpu.clone();
    cpu2.host = "2".into();
    let mut agent = check("1", Watch::Agent, Some(Severity::Critical));
    agent.origin = Origin::new("trigger", "Agent unreachable");
    let inventory = Inventory {
        hosts: vec![host("1", "a", Os::Linux), host("2", "b", Os::Linux)],
        checks: vec![cpu, cpu2, agent],
        ..Inventory::default()
    };
    let planned = run(&inventory, &Existing::default());
    let builtins: Vec<_> = planned
        .items
        .iter()
        .filter(|i| i.kind == "builtin")
        .collect();
    assert_eq!(builtins.len(), 2);
    assert_eq!(builtins[0].hosts, vec!["a", "b"]);
    assert!(builtins
        .iter()
        .all(|i| i.present && i.create.is_none() && i.list == List::Exact));
    assert_eq!(builtins[1].note.as_ref().unwrap().code, "offline_alert");
}

#[test]
fn intervals_are_kept_within_what_monitorflow_allows() {
    let mut fast = check("1", Watch::Service { name: "ssh".into() }, None);
    fast.interval_s = Some(5);
    let inventory = Inventory {
        hosts: vec![host("1", "a", Os::Linux)],
        checks: vec![fast],
        ..Inventory::default()
    };
    let planned = run(&inventory, &Existing::default());
    let item = by_id(&planned, "check:a:service_ssh");
    assert_eq!(item.list, List::Approximated);
    assert_eq!(item.note.as_ref().unwrap().code, "interval_clamped");
    let Some(Create::Check { interval_ms, .. }) = item.create else {
        panic!()
    };
    assert_eq!(interval_ms, 10_000);
}

#[test]
fn thresholds_of_one_measure_are_merged() {
    let inventory = Inventory {
        hosts: vec![host("1", "a", Os::Linux), host("2", "b", Os::Linux)],
        thresholds: vec![
            threshold(
                &["1", "2"],
                Measure::DiskUsed {
                    mount: Some("/".into()),
                },
                Some(0.8),
                None,
                "Disk warning",
            ),
            threshold(
                &["1"],
                Measure::DiskUsed {
                    mount: Some("/".into()),
                },
                None,
                Some(0.9),
                "Disk critical",
            ),
            threshold(&["1"], Measure::CpuUsage, Some(0.9), None, "High CPU"),
            // Unknown host.
            threshold(&["9"], Measure::MemoryAvailable, Some(0.1), None, "Memory"),
        ],
        ..Inventory::default()
    };
    let planned = run(&inventory, &Existing::default());
    let disk = by_id(&planned, "threshold:a:monitorflow_disk_used_ratio:/");
    assert_eq!(disk.origin.name, "Disk warning + Disk critical");
    let Some(Create::Threshold {
        warning,
        critical,
        instance,
        comparison,
        duration_s,
        ..
    }) = &disk.create
    else {
        panic!()
    };
    assert_eq!(
        (
            *warning,
            *critical,
            instance.as_deref(),
            comparison.as_str(),
            *duration_s
        ),
        (Some(0.8), Some(0.9), Some("/"), ">", 300)
    );
    let Some(Create::Threshold { critical, .. }) =
        &by_id(&planned, "threshold:b:monitorflow_disk_used_ratio:/").create
    else {
        panic!()
    };
    assert_eq!(*critical, None);
    let Some(Create::Threshold { instance, .. }) =
        &by_id(&planned, "threshold:a:monitorflow_cpu_usage_ratio:total").create
    else {
        panic!()
    };
    assert_eq!(instance.as_deref(), Some("total"));
    let unknown = planned
        .items
        .iter()
        .find(|i| i.origin.name == "Memory")
        .unwrap();
    assert_eq!(unknown.list, List::NotMovable);
    assert_eq!(unknown.note.as_ref().unwrap().code, "unknown_host");
}

#[test]
fn a_recovery_value_becomes_hysteresis_and_crossed_levels_are_fixed() {
    let mut t = threshold(
        &["1"],
        Measure::MemoryAvailable,
        Some(0.1),
        Some(0.2),
        "Low memory",
    );
    t.direction = Direction::Below;
    t.recovery = Some(0.15);
    let inventory = Inventory {
        hosts: vec![host("1", "a", Os::Linux)],
        thresholds: vec![t],
        ..Inventory::default()
    };
    let planned = run(&inventory, &Existing::default());
    let Some(Create::Threshold {
        warning,
        critical,
        hysteresis,
        comparison,
        ..
    }) = &by_id(&planned, "threshold:a:monitorflow_memory_available_ratio:*").create
    else {
        panic!()
    };
    // Below: critical 0.2 is not past warning 0.1, so warning goes.
    assert_eq!(
        (*warning, *critical, comparison.as_str()),
        (None, Some(0.2), "<")
    );
    assert!((hysteresis - 0.05).abs() < 1e-9);
}

#[test]
fn silences_are_cut_skipped_or_per_server() {
    let silence = |scope, starts: i64, ends: i64, name: &str| Silence {
        scope,
        starts_at: now() + TimeDelta::hours(starts),
        ends_at: now() + TimeDelta::hours(ends),
        reason: format!("{name} window"),
        origin: Origin::new("maintenance", name),
        fidelity: Fidelity::Exact,
    };
    let inventory = Inventory {
        hosts: vec![host("1", "a", Os::Linux), host("2", "b", Os::Linux)],
        silences: vec![
            silence(
                SilenceScope::Hosts(vec!["1".into(), "2".into()]),
                1,
                3,
                "Patch",
            ),
            silence(SilenceScope::Everything, -2, 24 * 60, "Freeze"),
            silence(SilenceScope::Hosts(vec!["1".into()]), -5, -1, "Old"),
        ],
        ..Inventory::default()
    };
    let planned = run(&inventory, &Existing::default());
    let patch: Vec<_> = planned
        .items
        .iter()
        .filter(|i| i.origin.name == "Patch")
        .collect();
    assert_eq!(patch.len(), 2);
    assert_eq!(patch[0].needs, vec!["server:a".to_owned()]);
    let freeze = planned
        .items
        .iter()
        .find(|i| i.origin.name == "Freeze")
        .unwrap();
    assert_eq!(freeze.list, List::Approximated);
    let Some(Create::Silence {
        server,
        starts_at,
        ends_at,
        ..
    }) = &freeze.create
    else {
        panic!()
    };
    assert_eq!(*server, None);
    assert_eq!(*starts_at, now());
    assert_eq!(*ends_at - *starts_at, TimeDelta::days(30));
    let old = planned
        .items
        .iter()
        .find(|i| i.origin.name == "Old")
        .unwrap();
    assert_eq!(
        (old.list, old.note.as_ref().unwrap().code.as_str()),
        (List::NotMovable, "silence_over")
    );
}

#[test]
fn channels_keep_secrets_apart() {
    let smtp = Smtp {
        host: "smtp.example".into(),
        port: 587,
        username: "alerts".into(),
        from: "alerts@example".into(),
        password: None,
    };
    let channel = |name: &str, target| Channel {
        name: name.into(),
        target,
        origin: Origin::new("media_type", name),
        fidelity: Fidelity::Exact,
    };
    let inventory = Inventory {
        channels: vec![
            channel(
                "Ops mail",
                Target::Email {
                    to: vec!["ops@example".into()],
                    smtp: Some(smtp),
                },
            ),
            channel(
                "Mail without server",
                Target::Email {
                    to: vec!["x@example".into()],
                    smtp: None,
                },
            ),
            channel(
                "Hook",
                Target::Webhook {
                    url: Secret::new("https://hooks.example/t/s3cret?k=1"),
                },
            ),
            channel(
                "Chat",
                Target::Slack {
                    url: Secret::new("https://hooks.slack.com/services/T/B/x"),
                },
            ),
        ],
        ..Inventory::default()
    };
    let existing = Existing {
        channels: HashSet::from([channel_key(
            "slack",
            "https://hooks.slack.com/services/T/B/x",
        )]),
        ..Existing::default()
    };
    let planned = plan(&inventory, site(), true, &existing, now());
    let mail = by_id(&planned, "channel:email:Ops mail");
    assert_eq!(mail.note.as_ref().unwrap().code, "smtp_password");
    let Some(Create::Channel {
        site_id, settings, ..
    }) = &mail.create
    else {
        panic!()
    };
    assert_eq!(
        *site_id, None,
        "a global administrator's channels cover everything"
    );
    assert_eq!(settings["to"], json!(["ops@example"]));
    let none = planned
        .items
        .iter()
        .find(|i| i.origin.name == "Mail without server")
        .unwrap();
    assert_eq!(none.list, List::NotMovable);
    let hook = by_id(&planned, "channel:webhook:Hook");
    let Some(Create::Channel { settings, .. }) = &hook.create else {
        panic!()
    };
    assert_eq!(settings["url"], "https://hooks.example");
    assert!(!serde_json::to_string(&planned.items)
        .unwrap()
        .contains("s3cret"));
    assert_eq!(
        planned.secrets["channel:webhook:Hook"].url.as_deref(),
        Some("https://hooks.example/t/s3cret?k=1")
    );
    assert!(by_id(&planned, "channel:slack:Chat").present);
    let planned = plan(&inventory, site(), false, &existing, now());
    let Some(Create::Channel { site_id, .. }) = &by_id(&planned, "channel:email:Ops mail").create
    else {
        panic!()
    };
    assert_eq!(*site_id, Some(site()));
}

#[test]
fn what_cannot_be_moved_is_one_line_per_source_item() {
    let item = |host: &str| Unmovable {
        hosts: vec![host.into()],
        origin: Origin::new("item", "Bits received").with_detail("net.if.in[eth0]"),
        why: Note::plain("network_traffic"),
    };
    let inventory = Inventory {
        hosts: vec![host("1", "a", Os::Linux), host("2", "b", Os::Linux)],
        unmovable: vec![item("1"), item("2")],
        ..Inventory::default()
    };
    let planned = run(&inventory, &Existing::default());
    let lines: Vec<_> = planned
        .items
        .iter()
        .filter(|i| i.list == List::NotMovable)
        .collect();
    assert_eq!(lines.len(), 1);
    assert_eq!(lines[0].hosts, vec!["a", "b"]);
    assert_eq!(lines[0].origin.detail.as_deref(), Some("net.if.in[eth0]"));
}

#[test]
fn servers_of_sites_one_does_not_administer_get_nothing() {
    let inventory = Inventory {
        hosts: vec![host("1", "a", Os::Linux)],
        checks: vec![check("1", Watch::Service { name: "ssh".into() }, None)],
        ..Inventory::default()
    };
    let existing = Existing {
        servers: vec![ExistingServer {
            peer_id: "a".into(),
            hostname: "a".into(),
            site_id: Uuid::from_u128(2),
            may_change: false,
        }],
        ..Existing::default()
    };
    let planned = run(&inventory, &existing);
    let check = planned.items.iter().find(|i| i.kind == "check").unwrap();
    assert_eq!(check.list, List::NotMovable);
    assert_eq!(check.note.as_ref().unwrap().code, "server_elsewhere");
}

#[test]
fn check_names_are_unique_per_server() {
    let inventory = Inventory {
        hosts: vec![host("1", "a", Os::Linux)],
        checks: vec![
            check(
                "1",
                Watch::Log {
                    path: "/var/log/app.log".into(),
                    pattern: "error".into(),
                },
                None,
            ),
            check(
                "1",
                Watch::Log {
                    path: "/srv/app.log".into(),
                    pattern: "fatal".into(),
                },
                None,
            ),
        ],
        ..Inventory::default()
    };
    let planned = run(&inventory, &Existing::default());
    by_id(&planned, "check:a:log_app");
    by_id(&planned, "check:a:log_app_2");
}

#[test]
fn chosen_items_follow_what_they_need() {
    let inventory = Inventory {
        hosts: vec![host("1", "a", Os::Linux), host("2", "b", Os::Linux)],
        checks: vec![
            check(
                "1",
                Watch::Service { name: "ssh".into() },
                Some(Severity::Critical),
            ),
            check(
                "2",
                Watch::Service { name: "ssh".into() },
                Some(Severity::Critical),
            ),
        ],
        ..Inventory::default()
    };
    let planned = run(&inventory, &Existing::default());
    let skip = HashSet::from([
        "server:a".to_owned(),
        "threshold:b:monitorflow_service_ssh_active".to_owned(),
    ]);
    let (chosen, skipped, present) = crate::import::store::chosen(&planned.items, &skip);
    let ids: Vec<_> = chosen.iter().map(|i| i.id.as_str()).collect();
    assert_eq!(ids, vec!["server:b", "check:b:service_ssh"]);
    assert_eq!((skipped, present), (4, 0));
}

#[test]
fn channels_the_channels_page_would_refuse_cannot_be_moved() {
    let inventory = Inventory {
        channels: vec![Channel {
            name: "Old Teams".into(),
            target: Target::Teams {
                url: Secret::new("https://example.webhook.office.com/webhookb2/x"),
            },
            origin: Origin::new("media_type", "Old Teams"),
            fidelity: Fidelity::Exact,
        }],
        ..Inventory::default()
    };
    let planned = run(&inventory, &Existing::default());
    let item = &planned.items[0];
    assert_eq!(item.list, List::NotMovable);
    assert_eq!(item.note.as_ref().unwrap().code, "refused");
    assert!(item.note.as_ref().unwrap().text.contains("retired"));
    assert!(planned.secrets.is_empty());
}

#[test]
fn the_same_check_twice_is_one_check_with_the_more_severe_alert() {
    let inventory = Inventory {
        hosts: vec![host("1", "a", Os::Linux)],
        checks: vec![
            check(
                "1",
                Watch::Port {
                    address: None,
                    port: 80,
                },
                Some(Severity::Warning),
            ),
            check(
                "1",
                Watch::Port {
                    address: Some("127.0.0.1".into()),
                    port: 80,
                },
                Some(Severity::Critical),
            ),
            check(
                "1",
                Watch::Port {
                    address: Some("localhost".into()),
                    port: 80,
                },
                None,
            ),
        ],
        ..Inventory::default()
    };
    let planned = run(&inventory, &Existing::default());
    let checks: Vec<_> = planned.items.iter().filter(|i| i.kind == "check").collect();
    assert_eq!(checks.len(), 1);
    let alerts: Vec<_> = planned
        .items
        .iter()
        .filter(|i| i.kind == "threshold")
        .collect();
    assert_eq!(alerts.len(), 1);
    let Some(Create::Threshold {
        warning, critical, ..
    }) = &alerts[0].create
    else {
        panic!()
    };
    assert_eq!((*warning, *critical), (None, Some(1.0)));
}

#[test]
fn event_logs_become_eventlog_checks() {
    let inventory = Inventory {
        hosts: vec![host("w", "win", Os::Windows)],
        checks: vec![
            check(
                "w",
                Watch::EventLog {
                    channel: "System".into(),
                    levels: vec!["critical".into(), "error".into()],
                    providers: vec!["Disk".into()],
                    event_ids: vec![7, 51],
                },
                Some(Severity::Warning),
            ),
            check(
                "w",
                Watch::EventLog {
                    channel: "Application".into(),
                    levels: vec![],
                    providers: vec![],
                    event_ids: vec![],
                },
                None,
            ),
        ],
        ..Inventory::default()
    };
    let planned = run(&inventory, &Existing::default());
    let system = by_id(&planned, "check:win:eventlog_system");
    let Some(Create::Check { kind, params, .. }) = &system.create else {
        panic!("{system:?}")
    };
    assert_eq!(kind, "eventlog");
    assert_eq!(
        Value::Object(params.clone()),
        json!({"channel": "System", "levels": "critical,error", "providers": "Disk", "event_ids": "7,51"})
    );
    let alert = by_id(
        &planned,
        "threshold:win:monitorflow_eventlog_system_matches",
    );
    let Some(Create::Threshold {
        comparison,
        warning,
        critical,
        ..
    }) = &alert.create
    else {
        panic!("{alert:?}")
    };
    assert_eq!(
        (comparison.as_str(), *warning, *critical),
        (">", Some(0.0), None)
    );
    let application = by_id(&planned, "check:win:eventlog_application");
    let Some(Create::Check { params, .. }) = &application.create else {
        panic!("{application:?}")
    };
    assert_eq!(
        Value::Object(params.clone()),
        json!({"channel": "Application"})
    );
}
