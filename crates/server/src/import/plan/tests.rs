use chrono::TimeZone;
use monitorflow_common::redact::Secret;
use monitorflow_import::model::{
    Alert, Channel, Condition, Host, Judged, Origin, Repeat, Rule, Silence, Smtp, Threshold,
    Unmovable, Website, Window, WindowFunction, EVERY_MONTH,
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
            no_data: false,
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
        judged: Judged::Value,
        conditions: Vec::new(),
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
            device: false,
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
            device: false,
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

fn scan(name: &str, targets: &[&str]) -> monitorflow_import::model::Scan {
    monitorflow_import::model::Scan {
        name: name.into(),
        targets: targets.iter().map(|t| (*t).to_owned()).collect(),
        ports: Some(vec![22, 80]),
        every: Some(monitorflow_import::model::Every::Day),
        snmp: None,
        group: None,
        origin: Origin::new("discovery_rule", name),
        fidelity: Fidelity::Exact,
    }
}

#[test]
fn network_scans_go_to_the_site_from_its_probe() {
    use monitorflow_import::model::ScanSnmp;
    let mut office = scan("Office", &["192.0.2.0/24"]);
    office.snmp = Some(ScanSnmp {
        version: "v2c".into(),
        port: 161,
        community: Some(Secret::new("recorded-community")),
        ..ScanSnmp::default()
    });
    let mut lab = scan("Lab", &["198.51.100.1-10"]);
    (lab.ports, lab.every) = (None, None);
    // Too short a password for SNMPv3.
    lab.snmp = Some(ScanSnmp {
        version: "v3".into(),
        port: 161,
        user: Some("monitor".into()),
        auth_protocol: Some("sha".into()),
        auth_password: Some(Secret::new("short")),
        ..ScanSnmp::default()
    });
    let inventory = Inventory {
        scans: vec![
            office,
            scan("Everything", &["10.0.0.0/16"]),
            lab,
            scan("Branch", &["203.0.113.5"]),
        ],
        ..Inventory::default()
    };
    let existing = Existing {
        scans: HashSet::from(["branch".to_owned()]),
        probe: "relay-paris".into(),
        ..Existing::default()
    };
    let planned = run(&inventory, &existing);

    let office = by_id(&planned, "network_scan:Office");
    assert_eq!(
        (office.kind.as_str(), office.list),
        ("network_scan", List::Exact)
    );
    assert_eq!(
        office.create,
        Some(Create::NetworkScan {
            name: "Office".into(),
            targets: vec!["192.0.2.0/24".into()],
            addresses: 254,
            ports: vec![22, 80],
            every: Some("day".into()),
            probe: "relay-paris".into(),
            has_snmp: true,
        })
    );
    // The community is sealed with the run, never in the preview.
    let secret = planned.secrets["network_scan:Office"]
        .secret
        .as_deref()
        .unwrap();
    let request: crate::snmp::CredentialsRequest = serde_json::from_str(secret).unwrap();
    assert_eq!(request.community.as_deref(), Some("recorded-community"));
    assert!(!serde_json::to_string(&planned.items)
        .unwrap()
        .contains("recorded-community"));

    // More than a scan tries: refused as the API would.
    let everything = by_id(&planned, "other:discovery_rule:Everything");
    assert_eq!(everything.list, List::NotMovable);
    assert_eq!(everything.note.as_ref().unwrap().code, "refused");

    // Default ports, when asked only, and credentials refused said so.
    let lab = by_id(&planned, "network_scan:Lab");
    assert_eq!(lab.list, List::Approximated);
    assert_eq!(lab.note.as_ref().unwrap().code, "scan_snmp_refused");
    let Some(Create::NetworkScan {
        ports,
        every,
        has_snmp,
        addresses,
        ..
    }) = &lab.create
    else {
        panic!("{lab:?}");
    };
    assert_eq!(ports, &crate::netscan::targets::DEFAULT_PORTS.to_vec());
    assert_eq!((every, *has_snmp, *addresses), (&None, false, 10));
    assert!(!planned.secrets.contains_key("network_scan:Lab"));

    // A scan of that name in the site already: nothing to create.
    assert!(by_id(&planned, "network_scan:Branch").present);
    // Without a relay, the central server.
    let planned = run(&inventory, &Existing::default());
    assert!(matches!(
        &by_id(&planned, "network_scan:Office").create,
        Some(Create::NetworkScan { probe, .. }) if probe == "central"
    ));
}

#[test]
fn services_take_the_servers_and_checks_the_import_makes() {
    use monitorflow_import::model::{Member, Service};
    let postgres = Watch::Port {
        address: None,
        port: 5432,
    };
    let inventory = Inventory {
        hosts: vec![
            host("db-01", "db-01", Os::Debian),
            host("db-02", "db-02", Os::Debian),
        ],
        checks: vec![
            check("db-01", postgres.clone(), Some(Severity::Critical)),
            check("db-02", postgres.clone(), Some(Severity::Critical)),
        ],
        services: vec![
            Service {
                name: "Database".into(),
                members: vec![
                    Member {
                        host: "db-01".into(),
                        check: Some(postgres.clone()),
                    },
                    Member {
                        host: "db-02".into(),
                        check: None,
                    },
                    // A host the import does not have.
                    Member {
                        host: "gone".into(),
                        check: None,
                    },
                ],
                at_least: Some(1),
                target: Some(99.9),
                origin: Origin::new("service", "Database"),
                fidelity: Fidelity::Exact,
            },
            Service {
                name: "Nothing".into(),
                members: vec![Member {
                    host: "gone".into(),
                    check: None,
                }],
                at_least: None,
                target: None,
                origin: Origin::new("service", "Nothing"),
                fidelity: Fidelity::Exact,
            },
        ],
        ..Inventory::default()
    };
    let planned = run(&inventory, &Existing::default());
    let item = by_id(&planned, "service:Database");
    assert_eq!(item.list, List::Exact);
    assert_eq!(item.needs, ["server:db-01", "server:db-02"]);
    let Create::Service {
        at_least,
        target,
        members,
        ..
    } = create_of(item)
    else {
        panic!("a service");
    };
    assert_eq!((*at_least, *target), (Some(1), Some(99.9)));
    assert_eq!(members.len(), 2);
    assert_eq!(members[0].server, "db-01");
    assert_eq!(members[0].check.as_ref().unwrap().name, "port_5432");
    assert_eq!(members[1].check, None);
    let nothing = planned
        .items
        .iter()
        .find(|i| i.origin.name == "Nothing")
        .unwrap();
    assert_eq!(nothing.list, List::NotMovable);
    // A service of that name is there already.
    let existing = Existing {
        services: HashSet::from(["database".to_owned()]),
        ..Existing::default()
    };
    assert!(by_id(&run(&inventory, &existing), "service:Database").present);
}

#[test]
fn richer_thresholds_keep_what_they_judge() {
    let mut forecast = threshold(
        &["1"],
        Measure::DiskUsed { mount: None },
        Some(259_200.0),
        Some(86_400.0),
        "Disk full in 3 days",
    );
    forecast.judged = Judged::Forecast {
        target: 1.0,
        window_s: 86_400,
    };
    forecast.for_s = 0;
    let mut window = threshold(&["1"], Measure::CpuUsage, Some(0.9), None, "CPU average");
    window.judged = Judged::Window {
        function: WindowFunction::Avg,
        window_s: 900,
    };
    window.for_s = 0;
    window.conditions = vec![Condition {
        measure: Measure::CpuLoad { minutes: 1 },
        direction: Direction::Above,
        value: 8.0,
        window: None,
    }];
    // A plain threshold on the same disk is a row of its own.
    let plain = threshold(
        &["1"],
        Measure::DiskUsed { mount: None },
        Some(0.85),
        None,
        "Disk",
    );
    let inventory = Inventory {
        hosts: vec![host("1", "web-01", Os::Linux)],
        thresholds: vec![forecast, window, plain],
        ..Inventory::default()
    };
    let planned = run(&inventory, &Existing::default());
    let item = by_id(
        &planned,
        "threshold:web-01:monitorflow_disk_used_ratio:*:forecast",
    );
    assert_eq!(item.list, List::Exact, "{:?}", item.note);
    let Some(Create::Threshold {
        comparison,
        richer,
        warning,
        critical,
        ..
    }) = &item.create
    else {
        panic!()
    };
    assert_eq!(comparison, ">=");
    assert_eq!(
        (richer.kind(), richer.target, richer.window_s),
        ("forecast", Some(1.0), Some(86_400))
    );
    assert_eq!((*warning, *critical), (Some(259_200.0), Some(86_400.0)));
    assert!(
        by_id(&planned, "threshold:web-01:monitorflow_disk_used_ratio:*")
            .create
            .is_some()
    );
    let Some(Create::Threshold { richer, metric, .. }) = &by_id(
        &planned,
        "threshold:web-01:monitorflow_cpu_usage_ratio:total:window",
    )
    .create
    else {
        panic!()
    };
    assert_eq!(metric, "monitorflow_cpu_usage_ratio");
    assert_eq!(
        (richer.function.as_deref(), richer.window_s),
        (Some("avg"), Some(900))
    );
    assert_eq!(richer.conditions.len(), 1);
    assert_eq!(richer.conditions[0].metric, "monitorflow_cpu_load1");
    assert_eq!(richer.conditions[0].function, "value");
    // Present when the server has that kind already, not another one.
    let existing = Existing {
        thresholds: HashSet::from([(
            "web-01".to_owned(),
            "monitorflow_disk_used_ratio".to_owned(),
            None,
            "forecast".to_owned(),
        )]),
        ..Existing::default()
    };
    let planned = run(&inventory, &existing);
    assert!(
        by_id(
            &planned,
            "threshold:web-01:monitorflow_disk_used_ratio:*:forecast"
        )
        .present
    );
    assert!(!by_id(&planned, "threshold:web-01:monitorflow_disk_used_ratio:*").present);
}

#[test]
fn a_check_that_goes_silent_gets_a_no_data_rule_beside_its_alert() {
    let mut port = check(
        "1",
        Watch::Port {
            address: None,
            port: 443,
        },
        Some(Severity::Critical),
    );
    port.alert = Some(Alert {
        severity: Severity::Warning,
        for_s: 600,
        no_data: true,
    });
    let inventory = Inventory {
        hosts: vec![host("1", "web-01", Os::Linux)],
        checks: vec![port],
        ..Inventory::default()
    };
    let planned = run(&inventory, &Existing::default());
    let Some(Create::Threshold {
        metric,
        comparison,
        warning,
        duration_s,
        richer,
        ..
    }) = &by_id(&planned, "threshold:web-01:monitorflow_port_443_up:nodata").create
    else {
        panic!()
    };
    assert_eq!(
        (
            metric.as_str(),
            comparison.as_str(),
            *warning,
            *duration_s,
            richer.kind()
        ),
        ("monitorflow_port_443_up", ">", Some(600.0), 0, "nodata")
    );
}

#[test]
fn a_second_rule_of_a_kind_on_a_measure_says_why_it_stays() {
    let window = |name: &str, function, window_s| {
        let mut t = threshold(&["1"], Measure::CpuUsage, Some(0.9), None, name);
        t.judged = Judged::Window { function, window_s };
        t.for_s = 0;
        t
    };
    let inventory = Inventory {
        hosts: vec![host("1", "web-01", Os::Linux)],
        thresholds: vec![
            window("Busy for 5 minutes", WindowFunction::Min, 300),
            window("Busy this hour", WindowFunction::Avg, 3_600),
            // The same rule again merges, as before.
            window("Busy for 5 minutes, critical", WindowFunction::Min, 300),
        ],
        ..Inventory::default()
    };
    let planned = run(&inventory, &Existing::default());
    let kept = by_id(
        &planned,
        "threshold:web-01:monitorflow_cpu_usage_ratio:total:window",
    );
    assert_eq!(
        kept.origin.name,
        "Busy for 5 minutes + Busy for 5 minutes, critical"
    );
    let left: Vec<&Item> = planned
        .items
        .iter()
        .filter(|i| i.list == List::NotMovable)
        .collect();
    assert_eq!(left.len(), 1);
    assert_eq!(left[0].origin.name, "Busy this hour");
    assert_eq!(left[0].note.as_ref().unwrap().code, "kind_taken");
}

#[test]
fn a_host_already_a_server_with_its_agent_stays_one() {
    // The source watches db-01 by ping only, but MonitorFlow has it as a
    // server with an agent: its ping names the host, as for any server.
    let mut db = host("1", "db-01", Os::Linux);
    db.agentless = true;
    let inventory = Inventory {
        hosts: vec![db],
        checks: vec![check("1", Watch::Ping { address: None }, None)],
        ..Inventory::default()
    };
    let existing = Existing {
        servers: vec![ExistingServer {
            peer_id: "db-01".into(),
            hostname: "db-01".into(),
            site_id: site(),
            may_change: true,
            device: false,
        }],
        ..Existing::default()
    };
    let planned = run(&inventory, &existing);
    let ping = planned.items.iter().find(|i| i.kind == "check").unwrap();
    let Some(Create::Check { params, probe, .. }) = &ping.create else {
        panic!("{ping:?}")
    };
    assert!(*probe);
    assert_eq!(params["host"], "10.0.0.5");
}

/// VMware and Hyper-V (ADR-0156): a vCenter is a device with its
/// `vmware` check waiting for its password, a Hyper-V host a Windows
/// server with its `hyperv` check, and a datastore threshold a vCenter's.
#[test]
fn vcenters_and_hyperv_hosts_get_their_checks() {
    let vcenter = Host {
        agentless: true,
        addresses: vec!["vcenter-1.example.test".into()],
        ..host("1", "vcenter-1", Os::Unknown)
    };
    let mut low = threshold(
        &["1"],
        Measure::DatastoreFree,
        Some(0.2),
        Some(0.1),
        "Free space is low",
    );
    low.direction = Direction::Below;
    let inventory = Inventory {
        hosts: vec![
            vcenter,
            host("2", "hv-01", Os::Windows),
            host("3", "web-01", Os::Debian),
        ],
        checks: vec![
            check("1", Watch::Vmware, None),
            check("2", Watch::HyperV, None),
            check("3", Watch::HyperV, None),
            check("3", Watch::Vmware, None),
        ],
        thresholds: vec![low],
        ..Inventory::default()
    };
    let planned = run(&inventory, &Existing::default());
    let vmware = by_id(&planned, "check:vcenter-1:vmware");
    assert_eq!(vmware.list, List::Approximated);
    assert_eq!(vmware.note.as_ref().unwrap().code, "vmware_password");
    let Some(Create::Check {
        kind,
        probe,
        params,
        ..
    }) = &vmware.create
    else {
        panic!("{vmware:?}")
    };
    assert_eq!((kind.as_str(), *probe), ("vmware", true));
    assert!(
        params.is_empty(),
        "the credentials come from the device's page"
    );
    let hyperv = by_id(&planned, "check:hv-01:hyperv");
    assert_eq!(hyperv.note.as_ref().unwrap().code, "hyperv_group");
    let Some(Create::Check { kind, probe, .. }) = &hyperv.create else {
        panic!()
    };
    assert_eq!((kind.as_str(), *probe), ("hyperv", false));
    let refused: Vec<&str> = planned
        .items
        .iter()
        .filter(|i| i.list == List::NotMovable && i.hosts == ["web-01"])
        .filter_map(|i| i.note.as_ref().map(|n| n.code.as_str()))
        .collect();
    assert_eq!(refused, ["hyperv_windows", "vmware_of_agent"]);
    let datastore = planned
        .items
        .iter()
        .find(|i| {
            i.id.starts_with("threshold:vcenter-1:monitorflow_vmware_datastore_free_ratio")
        })
        .unwrap_or_else(|| {
            panic!(
                "{:#?}",
                planned.items.iter().map(|i| &i.id).collect::<Vec<_>>()
            )
        });
    assert!(matches!(
        &datastore.create,
        Some(Create::Threshold { comparison, warning: Some(w), critical: Some(c), .. })
            if comparison == "<" && *w == 0.2 && *c == 0.1
    ));
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

/// Custom SNMP checks (ADR-0157): one check of the device with its
/// channels, the lookups it comes with, and its channels' rules.
#[test]
fn custom_snmp_checks_with_their_lookups_and_rules() {
    use monitorflow_import::model::{
        Num, SnmpChannel, SnmpCustom, SnmpLimits, SnmpLookup, SnmpLookupEntry, SnmpState, SnmpValue,
    };
    let nas = Host {
        agentless: true,
        addresses: vec!["192.0.2.41".into()],
        ..host("nas", "nas-files", Os::Unknown)
    };
    let entry = |value: i64, name: &str, state| SnmpLookupEntry {
        value: Some(value),
        max: None,
        text: None,
        name: name.into(),
        state,
    };
    let channel = |oid: &str, name: &str| SnmpChannel {
        oid: oid.into(),
        name: name.into(),
        value: SnmpValue::Gauge,
        unit: None,
        multiply: None,
        divide: None,
        lookup: None,
        expect: None,
        limits: None,
    };
    let custom = SnmpCustom {
        table: None,
        name_column: None,
        channels: vec![
            SnmpChannel {
                divide: Some(Num(10.0)),
                unit: Some("°C".into()),
                limits: Some(SnmpLimits {
                    above: true,
                    warning: Some(Num(50.0)),
                    critical: Some(Num(60.0)),
                }),
                ..channel("1.3.6.1.4.1.6574.1.2.0", "Temperature")
            },
            SnmpChannel {
                lookup: Some(SnmpLookup {
                    name: "oid.test.status".into(),
                    builtin: None,
                    entries: vec![
                        entry(1, "Normal", SnmpState::Ok),
                        entry(2, "Failed", SnmpState::Critical),
                    ],
                    otherwise: SnmpState::Warning,
                }),
                ..channel("1.3.6.1.4.1.6574.1.1.0", "System status")
            },
        ],
    };
    let mut item = check("nas", Watch::SnmpCustom(Box::new(custom.clone())), None);
    item.origin = Origin::new("sensor", "NAS health");
    let on_agent = check("web", Watch::SnmpCustom(Box::new(custom)), None);
    let inventory = Inventory {
        hosts: vec![nas, host("web", "web-01", Os::Debian)],
        checks: vec![item, on_agent],
        ..Inventory::default()
    };
    let planned = run(&inventory, &Existing::default());
    let check = by_id(&planned, "check:nas-files:snmp_nas_health");
    assert_eq!(check.list, List::Approximated, "credentials come after");
    let Some(Create::Check {
        kind,
        params,
        probe,
        lookups,
        ..
    }) = &check.create
    else {
        panic!()
    };
    assert_eq!((kind.as_str(), *probe), ("snmp_custom", true));
    assert_eq!(params["divide1"], json!("10"));
    assert_eq!(params["name2"], json!("System status"));
    assert!(!params.contains_key("lookup2"));
    assert_eq!(
        (lookups[0].channel, lookups[0].name.as_str()),
        (2, "oid.test.status")
    );
    let rule = |metric: &str| {
        by_id(&planned, &format!("threshold:nas-files:{metric}"))
            .create
            .clone()
            .unwrap()
    };
    let Create::Threshold {
        comparison,
        warning,
        critical,
        ..
    } = rule("monitorflow_snmp_nas_health_temperature")
    else {
        panic!()
    };
    assert_eq!(
        (comparison.as_str(), warning, critical),
        (">", Some(50.0), Some(60.0))
    );
    let Create::Threshold {
        comparison,
        critical,
        ..
    } = rule("monitorflow_snmp_nas_health_system_status_state")
    else {
        panic!()
    };
    assert_eq!((comparison.as_str(), critical), (">=", Some(2.0)));
    // A server with its agent has no SNMP of its own to read.
    assert!(planned.items.iter().any(|i| i.list == List::NotMovable
        && i.note.as_ref().is_some_and(|n| n.code == "snmp_of_agent")));
}

/// A server of a site one does not administer, matched by name, is never
/// an end of a dependency nor a member of a service: either would hold
/// its alerts or tell its state to someone of another site.
#[test]
fn dependencies_and_services_leave_out_servers_of_other_sites() {
    use monitorflow_import::model::{Dependency, DependencyEnd, Member, Service};

    let elsewhere = Existing {
        servers: vec![ExistingServer {
            peer_id: "victim".into(),
            hostname: "victim".into(),
            site_id: Uuid::from_u128(2),
            may_change: false,
            device: false,
        }],
        ..Existing::default()
    };
    let dependency = |child: &str, parent: &str| Dependency {
        child: DependencyEnd::host(child),
        parent: DependencyEnd::host(parent),
        origin: Origin::new("trigger", format!("{child} behind {parent}")),
        fidelity: Fidelity::Exact,
    };
    let member = |host: &str| Member {
        host: host.into(),
        check: None,
    };
    let inventory = Inventory {
        hosts: vec![
            host("mine", "mine", Os::Debian),
            host("victim", "victim", Os::Debian),
        ],
        dependencies: vec![dependency("victim", "mine"), dependency("mine", "victim")],
        services: vec![
            Service {
                name: "Theirs".into(),
                members: vec![member("victim")],
                at_least: None,
                target: None,
                origin: Origin::new("service", "Theirs"),
                fidelity: Fidelity::Exact,
            },
            Service {
                name: "Both".into(),
                members: vec![member("mine"), member("victim")],
                at_least: None,
                target: None,
                origin: Origin::new("service", "Both"),
                fidelity: Fidelity::Exact,
            },
        ],
        ..Inventory::default()
    };
    let planned = run(&inventory, &elsewhere);
    let dependencies: Vec<&Item> = planned
        .items
        .iter()
        .filter(|i| i.kind == "dependency")
        .collect();
    assert_eq!(dependencies.len(), 2);
    for item in dependencies {
        assert_eq!(item.list, List::NotMovable);
        assert_eq!(item.note.as_ref().unwrap().code, "server_elsewhere");
        assert!(item.create.is_none());
    }
    let theirs = planned
        .items
        .iter()
        .find(|i| i.kind == "service" && i.origin.name == "Theirs")
        .unwrap();
    assert!(theirs.create.is_none());
    let both = by_id(&planned, "service:Both");
    let Some(Create::Service { members, .. }) = &both.create else {
        panic!("{:?}", both.create);
    };
    assert_eq!(members.len(), 1);
    assert_eq!(members[0].server, "mine");
}

/// Ready-made application checks (ADR-0183): on the server itself its
/// agent runs them, elsewhere a probe does; they wait for their password
/// and come with their template's alerts, which the source's own alert
/// on `up` does not repeat. Docker needs an agent.
#[test]
fn application_checks_come_with_their_alerts() {
    use monitorflow_import::model::{App, AppKind};
    let device = Host {
        agentless: true,
        addresses: vec!["10.0.0.9".into()],
        ..host("3", "db-device", Os::Unknown)
    };
    let mut own = App::new(AppKind::Postgresql);
    // The server's own address: its agent connects to 127.0.0.1.
    own.address = Some("10.0.0.5".into());
    own.database = Some("shop".into());
    let mut remote = App::new(AppKind::Mysql);
    remote.address = Some("10.0.0.20".into());
    remote.port = Some(3307);
    let mut docker = App::new(AppKind::Docker);
    docker.containers = Some("^shop$".into());
    let inventory = Inventory {
        hosts: vec![host("1", "db-01", Os::Debian), device],
        checks: vec![
            check("1", Watch::app(own), Some(Severity::Critical)),
            check("1", Watch::app(remote), None),
            check("1", Watch::app(docker.clone()), None),
            check("3", Watch::app(App::new(AppKind::Redis)), None),
            check("3", Watch::app(docker), None),
        ],
        ..Inventory::default()
    };
    let planned = run(&inventory, &Existing::default());
    let create = |id: &str| {
        let item = by_id(&planned, id);
        let Some(Create::Check {
            kind,
            probe,
            params,
            ..
        }) = &item.create
        else {
            panic!("{item:?}")
        };
        (item, kind.clone(), *probe, Value::Object(params.clone()))
    };
    let (item, kind, probe, params) = create("check:db-01:postgresql");
    assert_eq!((kind.as_str(), probe), ("postgresql", false));
    assert_eq!(params, json!({"database": "shop"}));
    assert_eq!(item.list, List::Approximated);
    assert_eq!(item.note.as_ref().unwrap().code, "app_password");

    let (_, kind, probe, params) = create("check:db-01:mysql_10_0_0_20");
    assert_eq!((kind.as_str(), probe), ("mysql", true));
    assert_eq!(params, json!({"host": "10.0.0.20", "port": "3307"}));

    let (item, kind, probe, params) = create("check:db-01:docker_shop");
    assert_eq!((kind.as_str(), probe), ("docker", false));
    assert_eq!(params, json!({"containers": "^shop$"}));
    assert_eq!(item.list, List::Exact);

    // A device's Redis from a probe, at the device's address.
    let (_, kind, probe, params) = create("check:db-device:redis");
    assert_eq!((kind.as_str(), probe), ("redis", true));
    assert_eq!(params, json!({}));

    // Every alert of the template, once, the source's critical on `up`
    // included.
    let alerts: Vec<&Item> = planned
        .items
        .iter()
        .filter(|i| i.id.starts_with("threshold:db-01:monitorflow_postgresql_"))
        .collect();
    assert_eq!(
        alerts.len(),
        monitorflow_apps::templates::POSTGRESQL_TEMPLATE
            .alerts
            .len()
    );
    let up = by_id(&planned, "threshold:db-01:monitorflow_postgresql_up");
    assert_eq!(up.needs, ["check:db-01:postgresql", "server:db-01"]);
    assert!(matches!(
        &up.create,
        Some(Create::Threshold { comparison, warning: None, critical: Some(c), duration_s: 120, .. })
            if comparison == "<" && *c == 1.0
    ));
    by_id(
        &planned,
        "threshold:db-device:monitorflow_redis_memory_usage_ratio",
    );

    let refused = planned
        .items
        .iter()
        .find(|i| i.list == List::NotMovable)
        .unwrap();
    assert_eq!(refused.hosts, ["db-device"]);
    assert_eq!(refused.note.as_ref().unwrap().code, "app_docker_agent");
}

/// A Kubernetes cluster is read from a probe only (ADR-0183): at a
/// device's address, at the address of a server with an agent, or where
/// the source said; it waits for its token and brings its alerts.
#[test]
fn kubernetes_checks_run_from_a_probe() {
    use monitorflow_import::model::{App, AppKind};
    let device = Host {
        agentless: true,
        addresses: vec!["10.0.0.70".into()],
        ..host("3", "k8s-prod", Os::Unknown)
    };
    let mut elsewhere = App::new(AppKind::Kubernetes);
    elsewhere.address = Some("10.0.0.71".into());
    elsewhere.port = Some(16443);
    let inventory = Inventory {
        hosts: vec![host("1", "cp-01", Os::Debian), device],
        checks: vec![
            check("3", Watch::app(App::new(AppKind::Kubernetes)), None),
            check("1", Watch::app(App::new(AppKind::Kubernetes)), None),
            check("1", Watch::app(elsewhere), None),
        ],
        ..Inventory::default()
    };
    let planned = run(&inventory, &Existing::default());
    let creates: Vec<(&Item, bool, Value)> = planned
        .items
        .iter()
        .filter_map(|item| match &item.create {
            Some(Create::Check {
                kind,
                probe,
                params,
                ..
            }) if kind == "kubernetes" => Some((item, *probe, Value::Object(params.clone()))),
            _ => None,
        })
        .collect();
    assert_eq!(creates.len(), 3, "{:#?}", planned.items);
    assert!(creates.iter().all(|(_, probe, _)| *probe));
    let params: Vec<&Value> = creates.iter().map(|(_, _, p)| p).collect();
    assert!(params.contains(&&json!({})), "{params:?}");
    assert!(params.contains(&&json!({"host": "10.0.0.5"})), "{params:?}");
    assert!(
        params.contains(&&json!({"host": "10.0.0.71", "port": "16443"})),
        "{params:?}"
    );
    for (item, _, _) in &creates {
        assert_eq!(item.list, List::Approximated);
        assert_eq!(item.note.as_ref().unwrap().code, "app_kubernetes");
    }
    let alerts = planned
        .items
        .iter()
        .filter(|i| {
            i.id.starts_with("threshold:k8s-prod:monitorflow_kubernetes_")
        })
        .count();
    assert_eq!(
        alerts,
        monitorflow_apps::templates::KUBERNETES_TEMPLATE
            .alerts
            .len()
    );
}
