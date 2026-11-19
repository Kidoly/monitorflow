use chrono::TimeZone;
use monitorflow_common::redact::Secret;
use monitorflow_import::model::{
    Alert, Channel, Host, Origin, Repeat, Rule, Silence, Smtp, Threshold, Unmovable, Website,
    Window, EVERY_MONTH,
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
        agentless: false,
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
    // The page is a website check, with the source's alert.
    let web = by_id(&planned, "check:lin:http_shop_example");
    assert_eq!((web.list, web.note.as_ref()), (List::Exact, None));
    let Some(Create::Check { kind, params, .. }) = &web.create else {
        panic!()
    };
    assert_eq!(kind, "http");
    assert_eq!(
        Value::Object(params.clone()),
        json!({"url": "https://shop.example/health"})
    );
    let Some(Create::Threshold {
        comparison,
        critical,
        duration_s,
        ..
    }) = &by_id(&planned, "threshold:lin:monitorflow_http_shop_example_up").create
    else {
        panic!()
    };
    assert_eq!(
        (comparison.as_str(), *critical, *duration_s),
        ("<", Some(1.0), 120)
    );
}

fn website(url: &str) -> Website {
    Website::new(url)
}

fn create_of(item: &Item) -> &Create {
    item.create
        .as_ref()
        .expect("an item that creates something")
}

#[test]
fn websites_come_with_the_alerts_the_web_interface_adds() {
    let mut shop = website("https://shop.example.com/");
    shop.status = Some("200,301".into());
    shop.contains = Some("Welcome".into());
    shop.not_contains = Some("Error".into());
    shop.follow_redirects = false;
    shop.method = monitorflow_import::model::WebMethod::Get;
    shop.headers = vec![("Accept-Language".into(), "en".into())];
    shop.ip = monitorflow_import::model::IpFamily::V4;
    shop.timeout_s = Some(90);
    let mut shop_check = check("l", Watch::web(shop), None);
    shop_check.origin = Origin::new("web_scenario", "Shop");
    let inventory = Inventory {
        hosts: vec![host("l", "lin", Os::Linux)],
        checks: vec![
            shop_check,
            check(
                "l",
                Watch::web(website("http://intranet.example:8080/")),
                Some(Severity::Warning),
            ),
        ],
        ..Inventory::default()
    };
    let planned = run(&inventory, &Existing::default());
    let shop = by_id(&planned, "check:lin:http_shop_example_com");
    assert_eq!(
        (shop.list, &shop.needs),
        (List::Exact, &vec!["server:lin".to_owned()])
    );
    let Create::Check {
        kind,
        params,
        interval_ms,
        timeout_ms,
        ..
    } = create_of(shop)
    else {
        panic!()
    };
    assert_eq!(kind, "http");
    assert_eq!(
        Value::Object(params.clone()),
        json!({
            "url": "https://shop.example.com/",
            "status": "200,301",
            "contains": "Welcome",
            "not_contains": "Error",
            "follow_redirects": "false",
            "headers": "Accept-Language: en",
            "ip": "4",
        })
    );
    // At most the interval.
    assert_eq!((*interval_ms, *timeout_ms), (60_000, Some(60_000)));
    // Its alerts, as the web interface adds them for https.
    let alert = |metric: &str| {
        let item = by_id(&planned, &format!("threshold:lin:{metric}"));
        // Its check, then its server.
        let check = metric.split_once("monitorflow_").unwrap().1;
        let check = ["_up", "_cert_ok", "_cert_days_left", "_response_seconds"]
            .iter()
            .find_map(|s| check.split(':').next().unwrap().strip_suffix(s))
            .unwrap();
        assert_eq!(
            item.needs,
            [format!("check:lin:{check}"), "server:lin".to_owned()],
            "{metric}"
        );
        let Create::Threshold {
            comparison,
            warning,
            critical,
            duration_s,
            ..
        } = create_of(item)
        else {
            panic!()
        };
        (comparison.clone(), *warning, *critical, *duration_s)
    };
    assert_eq!(
        alert("monitorflow_http_shop_example_com_up"),
        ("<".into(), None, Some(1.0), 120)
    );
    assert_eq!(
        alert("monitorflow_http_shop_example_com_cert_ok:*"),
        ("<".into(), None, Some(1.0), 300)
    );
    assert_eq!(
        alert("monitorflow_http_shop_example_com_cert_days_left:*"),
        ("<".into(), Some(14.0), Some(7.0), 0)
    );
    assert_eq!(
        alert("monitorflow_http_shop_example_com_response_seconds:*"),
        (">".into(), Some(3.0), None, 300)
    );
    // Plain http: no certificate; the port is in the name; the source's
    // severity and wait.
    let intranet = by_id(&planned, "check:lin:http_intranet_example_8080");
    assert_eq!(intranet.list, List::Exact);
    assert_eq!(
        alert("monitorflow_http_intranet_example_8080_up"),
        ("<".into(), Some(1.0), None, 120)
    );
    assert!(!planned
        .items
        .iter()
        .any(|i| i.id.contains("http_intranet_example_8080_cert")));
    by_id(
        &planned,
        "threshold:lin:monitorflow_http_intranet_example_8080_response_seconds:*",
    );
    // Nothing is planned twice, and every id is unique.
    let mut ids: Vec<_> = planned.items.iter().map(|i| i.id.as_str()).collect();
    ids.sort_unstable();
    let count = ids.len();
    ids.dedup();
    assert_eq!(count, ids.len());
}

#[test]
fn thresholds_of_websites_replace_the_defaults() {
    let url = "https://shop.example.com/";
    let mut slow = threshold(
        &["l"],
        Measure::WebResponseTime { url: url.into() },
        Some(1.5),
        Some(5.0),
        "Shop is slow",
    );
    slow.for_s = 60;
    let mut expiry = threshold(
        &["l"],
        Measure::CertificateDays { url: url.into() },
        Some(30.0),
        None,
        "Certificate expires",
    );
    expiry.direction = Direction::Below;
    expiry.for_s = 0;
    let elsewhere = threshold(
        &["l"],
        Measure::WebResponseTime {
            url: "https://other.example.com/".into(),
        },
        Some(1.0),
        None,
        "Other is slow",
    );
    let inventory = Inventory {
        hosts: vec![host("l", "lin", Os::Linux)],
        checks: vec![check(
            "l",
            Watch::web(website(url)),
            Some(Severity::Critical),
        )],
        thresholds: vec![slow, expiry, elsewhere],
        ..Inventory::default()
    };
    let planned = run(&inventory, &Existing::default());
    let response = by_id(
        &planned,
        "threshold:lin:monitorflow_http_shop_example_com_response_seconds:*",
    );
    assert_eq!(response.origin.name, "Shop is slow");
    assert!(response
        .needs
        .contains(&"check:lin:http_shop_example_com".to_owned()));
    let Create::Threshold {
        comparison,
        warning,
        critical,
        duration_s,
        ..
    } = create_of(response)
    else {
        panic!()
    };
    assert_eq!(
        (comparison.as_str(), *warning, *critical, *duration_s),
        (">", Some(1.5), Some(5.0), 60)
    );
    let days = by_id(
        &planned,
        "threshold:lin:monitorflow_http_shop_example_com_cert_days_left:*",
    );
    let Create::Threshold {
        comparison,
        warning,
        critical,
        ..
    } = create_of(days)
    else {
        panic!()
    };
    assert_eq!(
        (comparison.as_str(), *warning, *critical),
        ("<", Some(30.0), None)
    );
    // A threshold on a website nothing checks.
    let other = planned
        .items
        .iter()
        .find(|i| i.origin.name == "Other is slow")
        .unwrap();
    assert_eq!(other.list, List::NotMovable);
    assert_eq!(other.note.as_ref().unwrap().code, "web_no_check");
}

#[test]
fn website_passwords_are_set_after_the_import() {
    let mut basic = website("https://shop.example.com/admin");
    basic.username = Some("monitor".into());
    basic.credentials = true;
    let mut token = website("https://api.example.com/");
    token.credentials = true;
    let mut clear = website("http://intranet.example/");
    clear.username = Some("monitor".into());
    clear.credentials = true;
    // A reader's own note gives way to the password's.
    let mut noted = check("l", Watch::web(basic), None);
    noted.fidelity = Fidelity::Approximated(Note::plain("web_certificate_only"));
    let inventory = Inventory {
        hosts: vec![host("l", "lin", Os::Linux)],
        checks: vec![
            noted,
            check("l", Watch::web(token), None),
            check("l", Watch::web(clear), None),
        ],
        ..Inventory::default()
    };
    let planned = run(&inventory, &Existing::default());
    let note_of = |id: &str| {
        let item = by_id(&planned, id);
        assert_eq!(item.list, List::Approximated, "{id}");
        let Create::Check { params, .. } = create_of(item) else {
            panic!()
        };
        // Neither the user name alone nor any secret is sent.
        assert!(!params.contains_key("password") && !params.contains_key("username"));
        item.note.clone().unwrap()
    };
    let note = note_of("check:lin:http_shop_example_com");
    assert_eq!(
        (note.code.as_str(), note.args["username"].as_str()),
        ("web_password", "monitor")
    );
    assert_eq!(note_of("check:lin:http_api_example_com").code, "web_token");
    assert_eq!(
        note_of("check:lin:http_intranet_example").code,
        "web_password_http"
    );
    assert!(planned.secrets.is_empty());
}

#[test]
fn a_website_the_check_refuses_cannot_be_moved() {
    let mut bad = website("https://shop.example.com/");
    bad.status = Some("not codes".into());
    let inventory = Inventory {
        hosts: vec![host("l", "lin", Os::Linux)],
        checks: vec![check("l", Watch::web(bad), None)],
        ..Inventory::default()
    };
    let planned = run(&inventory, &Existing::default());
    assert_eq!(planned.items.len(), 2, "the server and the refusal");
    let refused = &planned.items[1];
    assert_eq!(refused.list, List::NotMovable);
    assert_eq!(refused.note.as_ref().unwrap().code, "refused");
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
        checks: vec![],
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

#[test]
fn a_silence_of_checks_names_the_checks_planned() {
    let inventory = Inventory {
        hosts: vec![host("1", "a", Os::Linux)],
        checks: vec![check(
            "1",
            Watch::Service {
                name: "nginx.service".into(),
            },
            Some(Severity::Critical),
        )],
        silences: vec![Silence {
            scope: SilenceScope::Hosts(vec!["1".into()]),
            checks: vec![
                Watch::Service {
                    name: "nginx.service".into(),
                },
                Watch::Disk,
                // The offline alert is no check: left out.
                Watch::Agent,
            ],
            starts_at: now() + TimeDelta::hours(1),
            ends_at: now() + TimeDelta::hours(2),
            reason: "Rebuild".into(),
            origin: Origin::new("silence", "Rebuild"),
            fidelity: Fidelity::Exact,
        }],
        ..Inventory::default()
    };
    let planned = run(&inventory, &Existing::default());
    let silence = planned.items.iter().find(|i| i.kind == "silence").unwrap();
    let Some(Create::Silence { checks, .. }) = &silence.create else {
        panic!()
    };
    let names: Vec<&str> = checks.iter().map(|c| c.name.as_str()).collect();
    assert_eq!(names, ["service_nginx", "disk"]);
}

#[test]
fn a_silence_names_website_checks_planned_or_not() {
    let shop = Website::new("https://shop.example.com/");
    let inventory = Inventory {
        hosts: vec![host("1", "a", Os::Linux)],
        checks: vec![check(
            "1",
            Watch::web(shop.clone()),
            Some(Severity::Critical),
        )],
        silences: vec![Silence {
            scope: SilenceScope::Hosts(vec!["1".into()]),
            checks: vec![
                Watch::web(shop),
                // Not imported here: named as the plan would name it.
                Watch::web(Website::new("http://intranet:8080/")),
            ],
            starts_at: now() + TimeDelta::hours(1),
            ends_at: now() + TimeDelta::hours(2),
            reason: "Shop release".into(),
            origin: Origin::new("silence", "Shop release"),
            fidelity: Fidelity::Exact,
        }],
        ..Inventory::default()
    };
    let planned = run(&inventory, &Existing::default());
    let silence = planned.items.iter().find(|i| i.kind == "silence").unwrap();
    let Some(Create::Silence { checks, .. }) = &silence.create else {
        panic!()
    };
    let names: Vec<&str> = checks.iter().map(|c| c.name.as_str()).collect();
    assert_eq!(names, ["http_shop_example_com", "http_intranet_8080"]);
}

fn weekly(zone: Option<&str>) -> Rule {
    Rule {
        repeat: Repeat::Weekly,
        every: 1,
        weekdays: 1 << 6,
        month_day: None,
        month_week: None,
        months: EVERY_MONTH,
        start_time: chrono::NaiveTime::from_hms_opt(2, 0, 0).unwrap(),
        duration_minutes: 120,
        time_zone: zone.map(str::to_owned),
        starts_on: None,
        ends_on: None,
    }
}

#[test]
fn windows_go_to_the_site_in_the_zone_chosen() {
    let window = |scope, rule, name: &str| Window {
        scope,
        checks: vec![],
        rule,
        reason: format!("{name} window"),
        origin: Origin::new("maintenance", name),
        fidelity: Fidelity::Exact,
    };
    let mut over = weekly(Some("UTC"));
    over.ends_on = Some(chrono::NaiveDate::from_ymd_opt(2026, 1, 1).unwrap());
    let inventory = Inventory {
        hosts: vec![host("1", "a", Os::Linux), host("2", "b", Os::Linux)],
        windows: vec![
            window(
                SilenceScope::Hosts(vec!["1".into(), "2".into()]),
                weekly(None),
                "Patch",
            ),
            window(
                SilenceScope::Everything,
                weekly(Some("America/New_York")),
                "Freeze",
            ),
            window(
                SilenceScope::Everything,
                weekly(Some("Mars/Olympus")),
                "Mars",
            ),
            window(SilenceScope::Everything, over, "Old"),
        ],
        ..Inventory::default()
    };
    let paris: Tz = "Europe/Paris".parse().unwrap();
    let planned = plan_in(
        &inventory,
        site(),
        false,
        &Existing::default(),
        now(),
        paris,
    );
    let patch = planned
        .items
        .iter()
        .find(|i| i.origin.name == "Patch")
        .unwrap();
    let Some(Create::Window {
        servers,
        time_zone,
        weekdays,
        start_time,
        ..
    }) = &patch.create
    else {
        panic!()
    };
    assert_eq!(servers, &["a", "b"]);
    assert_eq!(
        (time_zone.as_str(), weekdays.as_slice(), start_time.as_str()),
        ("Europe/Paris", &["sunday".to_owned()][..], "02:00")
    );
    assert_eq!(
        patch.needs,
        vec!["server:a".to_owned(), "server:b".to_owned()]
    );
    let freeze = planned
        .items
        .iter()
        .find(|i| i.origin.name == "Freeze")
        .unwrap();
    assert!(
        matches!(&freeze.create, Some(Create::Window { servers, time_zone, .. })
        if servers.is_empty() && time_zone == "America/New_York")
    );
    for (name, code) in [("Mars", "refused"), ("Old", "silence_over")] {
        let item = planned
            .items
            .iter()
            .find(|i| i.origin.name == name)
            .unwrap();
        assert_eq!(
            (item.list, item.note.as_ref().unwrap().code.as_str()),
            (List::NotMovable, code)
        );
    }
    // The same window again is there already.
    let existing = Existing {
        windows: [window_key(patch.create.as_ref().unwrap()).unwrap()].into(),
        ..Existing::default()
    };
    let again = plan_in(&inventory, site(), false, &existing, now(), paris);
    assert!(
        again
            .items
            .iter()
            .find(|i| i.origin.name == "Patch")
            .unwrap()
            .present
    );
}

/// Hosts without an agent become devices, and pings, UDP ports and SNMP
/// become checks a probe runs (ADR-0148, ADR-0149).
#[test]
fn devices_and_probe_checks() {
    let switch = Host {
        agentless: true,
        addresses: vec!["192.0.2.2".into()],
        ..host("sw", "core-switch", Os::Unknown)
    };
    let nameless = Host {
        agentless: true,
        addresses: vec!["not an address".into()],
        ..host("x", "bad name!", Os::Unknown)
    };
    let inventory = Inventory {
        hosts: vec![switch, nameless, host("web", "web-01", Os::Debian)],
        checks: vec![
            check(
                "sw",
                Watch::Ping { address: None },
                Some(Severity::Critical),
            ),
            check("sw", Watch::Traffic, None),
            check(
                "sw",
                Watch::Snmp {
                    group: "system".into(),
                },
                Some(Severity::Critical),
            ),
            check("sw", Watch::Cpu, None),
            check("sw", Watch::Agent, None),
            check(
                "web",
                Watch::Ping { address: None },
                Some(Severity::Critical),
            ),
            check(
                "web",
                Watch::UdpPort {
                    address: Some("192.0.2.53".into()),
                    port: 53,
                    query: Some("dns".into()),
                },
                None,
            ),
            check(
                "web",
                Watch::Snmp {
                    group: "host".into(),
                },
                None,
            ),
            check("web", Watch::Traffic, None),
        ],
        ..Inventory::default()
    };
    let planned = run(&inventory, &Existing::default());
    assert_eq!(
        by_id(&planned, "server:core-switch").create,
        Some(Create::Device {
            peer_id: "core-switch".into(),
            hostname: "core-switch".into(),
            address: "192.0.2.2".into()
        })
    );
    // An agentless host the name of which is no address and with none.
    assert!(planned.items.iter().any(|i| i.kind == "server"
        && i.list == List::NotMovable
        && i.note.as_ref().is_some_and(|n| n.code == "no_address")));
    let create = |id: &str| by_id(&planned, id).create.clone().unwrap();
    let Create::Check {
        kind,
        params,
        probe,
        ..
    } = create("check:core-switch:ping")
    else {
        panic!()
    };
    assert_eq!((kind.as_str(), probe), ("ping", true));
    assert!(params.is_empty(), "a device's ping checks its address");
    let Create::Check { kind, params, .. } = create("check:core-switch:snmp_if") else {
        panic!()
    };
    assert_eq!(
        (kind.as_str(), params["what"].as_str()),
        ("snmp", Some("interfaces"))
    );
    let Create::Check { params, .. } = create("check:core-switch:snmp_host") else {
        panic!()
    };
    assert_eq!(
        params["what"], "host",
        "what an agent measures comes from SNMP on a device"
    );
    assert_eq!(
        by_id(&planned, "check:core-switch:snmp")
            .note
            .as_ref()
            .unwrap()
            .code,
        "snmp_credentials"
    );
    let alert = planned
        .items
        .iter()
        .find(|i| {
            i.id.starts_with("threshold:core-switch:monitorflow_snmp_up")
        })
        .unwrap();
    assert!(
        matches!(&alert.create, Some(Create::Threshold { comparison, critical: Some(c), .. }) if comparison == "<" && *c == 1.0)
    );
    assert!(planned
        .items
        .iter()
        .any(|i| i.note.as_ref().is_some_and(|n| n.code == "device_no_agent")));
    // A server is pinged at its address, and its own traffic is built in.
    let Create::Check { params, probe, .. } = create("check:web-01:ping_10_0_0_5") else {
        panic!()
    };
    assert_eq!((params["host"].as_str(), probe), (Some("10.0.0.5"), true));
    let Create::Check { params, .. } = create("check:web-01:udp_53") else {
        panic!()
    };
    assert_eq!(
        (
            params["protocol"].as_str(),
            params["query"].as_str(),
            params["host"].as_str()
        ),
        (Some("udp"), Some("dns"), Some("192.0.2.53"))
    );
    assert!(planned
        .items
        .iter()
        .any(|i| i.note.as_ref().is_some_and(|n| n.code == "snmp_of_agent")));
    assert!(planned
        .items
        .iter()
        .any(|i| i.kind == "builtin" && i.note.as_ref().is_some_and(|n| n.code == "builtin_net")));
}

#[test]
fn dependencies_follow_the_servers_and_checks_they_name() {
    use monitorflow_import::model::{Dependency, DependencyEnd};

    let mut switch = host("sw", "sw-core", Os::Unknown);
    switch.agentless = true;
    let https = Watch::Port {
        address: None,
        port: 443,
    };
    let ping = Watch::Ping { address: None };
    let dependency = |child: DependencyEnd, parent: DependencyEnd, fidelity: Fidelity| Dependency {
        child,
        parent,
        origin: Origin::new("trigger", "HTTPS is down"),
        fidelity,
    };
    let inventory = Inventory {
        hosts: vec![host("app", "app-01", Os::Debian), switch],
        checks: vec![
            check("app", https.clone(), Some(Severity::Critical)),
            check("sw", ping.clone(), Some(Severity::Critical)),
        ],
        dependencies: vec![
            dependency(
                DependencyEnd::host("app"),
                DependencyEnd::host("sw"),
                Fidelity::Exact,
            ),
            dependency(
                DependencyEnd::check("app", https),
                DependencyEnd::check("sw", ping),
                Fidelity::Approximated(Note::new(
                    "dependency_whole_check",
                    args! {"trigger" => "HTTPS is down"},
                )),
            ),
            // A host that is not in the inventory is listed with it.
            dependency(
                DependencyEnd::host("app"),
                DependencyEnd::host("gone"),
                Fidelity::Exact,
            ),
        ],
        ..Inventory::default()
    };
    let planned = run(&inventory, &Existing::default());
    let whole = by_id(&planned, "dependency:app-01:sw-core");
    assert_eq!(whole.list, List::Exact);
    assert_eq!(whole.hosts, ["app-01", "sw-core"]);
    assert_eq!(whole.needs, ["server:app-01", "server:sw-core"]);
    assert_eq!(
        whole.create,
        Some(Create::Dependency {
            server: "app-01".into(),
            check: None,
            parent: "sw-core".into(),
            parent_check: None,
        })
    );
    let checks: Vec<&Item> = planned
        .items
        .iter()
        .filter(|i| i.kind == "dependency" && i.id != whole.id)
        .collect();
    assert_eq!(checks.len(), 1, "{checks:#?}");
    let Some(Create::Dependency {
        check: Some(check),
        parent_check: Some(parent_check),
        ..
    }) = &checks[0].create
    else {
        panic!("{:?}", checks[0].create);
    };
    assert_eq!(checks[0].list, List::Approximated);
    // Both checks are made by this import: it needs them.
    for name in [&check.name, &parent_check.name] {
        assert!(
            checks[0]
                .needs
                .iter()
                .any(|n| n.ends_with(&format!(":{name}"))),
            "{:?}",
            checks[0].needs
        );
    }
}
