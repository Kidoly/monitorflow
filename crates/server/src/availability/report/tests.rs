use std::collections::HashMap;

use super::*;
use crate::availability::data::{Dependency, Member, Parent, Service};
use crate::availability::Rule;

fn t(m: i64) -> DateTime<Utc> {
    DateTime::from_timestamp(1_790_000_000 + m * 60, 0).unwrap()
}

const WEB1: Uuid = Uuid::from_u128(1);
const WEB2: Uuid = Uuid::from_u128(2);
const RELAY: Uuid = Uuid::from_u128(3);
const SITE: Uuid = Uuid::from_u128(4);
const CUSTOMER: Uuid = Uuid::from_u128(5);

fn machine(id: Uuid, name: &str, relays: Vec<Uuid>) -> Machine {
    Machine {
        id,
        name: name.into(),
        device: false,
        site_id: SITE,
        site_name: "paris".into(),
        customer_id: CUSTOMER,
        relays,
        created_at: t(-10_000),
        revoked_at: None,
    }
}

fn incident(server: Option<Uuid>, relay: Option<Uuid>, metric: &str, a: i64, b: i64) -> Incident {
    Incident {
        id: Uuid::new_v4(),
        server_id: server,
        relay_id: relay,
        metric: metric.into(),
        instance: if metric.contains("disk") {
            "/".into()
        } else {
            String::new()
        },
        critical: Spans::one(t(a), t(b)),
    }
}

fn silence(server: Option<Uuid>, checks: Option<Vec<CheckRef>>, a: i64, b: i64) -> Silence {
    Silence {
        server_id: server,
        relay_id: None,
        site_id: None,
        customer_id: None,
        checks,
        span: (t(a), t(b)),
        reason: "Patches".into(),
        window: true,
    }
}

fn disk() -> CheckRef {
    CheckRef {
        name: "disk".into(),
        instance: String::new(),
    }
}

fn data() -> Data {
    Data {
        machines: vec![
            machine(WEB1, "web-1", vec![RELAY]),
            machine(WEB2, "web-2", vec![]),
        ],
        relays: HashMap::from([(RELAY, "relay-paris".to_owned())]),
        incidents: vec![
            incident(Some(WEB1), None, "monitorflow_disk_used_ratio", 100, 130),
            incident(Some(WEB1), None, "offline", 200, 260),
            incident(None, Some(RELAY), "offline", 300, 320),
            incident(Some(WEB2), None, "monitorflow_cpu_usage_ratio", 110, 115),
        ],
        silences: vec![
            silence(Some(WEB1), None, 250, 400),
            silence(Some(WEB1), Some(vec![disk()]), 120, 125),
        ],
        services: vec![Service {
            id: Uuid::from_u128(9),
            customer_id: CUSTOMER,
            name: "Website".into(),
            rule: Rule::All,
            target: Some(99.9),
            members: vec![
                Member {
                    server_id: WEB1,
                    check: Some(disk()),
                },
                Member {
                    server_id: WEB2,
                    check: None,
                },
            ],
        }],
        dependencies: vec![],
        parents: vec![],
        names: vec!["cpu".into(), "disk".into(), "memory".into()],
    }
}

fn day() -> Span {
    (t(0), t(1440))
}

#[test]
fn the_check_of_a_metric_is_the_longest_name() {
    let names = vec!["disk".to_owned(), "disk_data".to_owned()];
    assert_eq!(
        check_of("monitorflow_disk_data_used_ratio", &names),
        "disk_data"
    );
    assert_eq!(check_of("monitorflow_disk_used_ratio", &names), "disk");
    assert_eq!(check_of("monitorflow_other_value", &names), "other_value");
}

#[test]
fn a_server_counts_its_incidents_without_planned_time() {
    let report = build(&data(), day(), t(2000));
    let web1 = &report.machines[0];
    // Disk 100-130 less its check silence 120-125, offline 200-250 before
    // the window; the relay's outage falls in the window.
    assert_eq!(web1.numbers.down_seconds, 75.0 * 60.0);
    assert_eq!(web1.numbers.maintenance_seconds, 150.0 * 60.0);
    assert_eq!(web1.numbers.counted_seconds, 1290.0 * 60.0);
    assert_eq!(web1.numbers.outages, 3);
    assert_eq!(web1.numbers.longest_seconds, 50.0 * 60.0);
    assert_eq!(web1.outage_list[2].causes[0].kind, "offline");
    assert_eq!(web1.outage_list[0].causes[0].check.as_deref(), Some("disk"));
    let expected = 100.0 * (1290.0 - 75.0) / 1290.0;
    assert!((web1.numbers.availability.unwrap() - expected).abs() < 1e-9);
}

#[test]
fn a_relay_outage_makes_what_is_behind_it_unreachable() {
    let mut data = data();
    data.silences.clear();
    let report = build(&data, day(), t(2000));
    let web1 = &report.machines[0];
    let last = web1.outage_list.last().unwrap();
    assert_eq!((last.start, last.end), (t(300), t(320)));
    assert_eq!(last.causes[0].kind, "unreachable");
    assert_eq!(last.causes[0].via.as_deref(), Some("relay-paris"));
    // A silence of the relay is planned for what is behind it.
    data.silences.push(Silence {
        relay_id: Some(RELAY),
        ..silence(None, None, 290, 330)
    });
    let report = build(&data, day(), t(2000));
    assert!(report.machines[0]
        .outage_list
        .iter()
        .all(|o| o.start != t(300)));
}

#[test]
fn checks_with_outages_are_listed() {
    let report = build(&data(), day(), t(2000));
    let names: Vec<_> = report
        .checks
        .iter()
        .map(|c| (c.server_name.as_str(), c.name.as_str()))
        .collect();
    assert_eq!(names, [("web-1", "disk"), ("web-2", "cpu")]);
    let disk = &report.checks[0];
    // The disk's own time, and its server being offline.
    assert_eq!(disk.numbers.down_seconds, 75.0 * 60.0);
    assert_eq!(disk.numbers.maintenance_seconds, 155.0 * 60.0);
}

#[test]
fn a_service_with_a_target() {
    let report = build(&data(), day(), t(2000));
    let service = &report.services[0];
    // Web-1's disk check (100-120, 125-130, 200-250) or web-2 (110-115).
    assert_eq!(service.numbers.down_seconds, 75.0 * 60.0);
    assert_eq!(service.met, Some(false));
    assert!(service.budget_seconds.unwrap() < 0.0);
    assert_eq!(service.outage_list[0].causes.len(), 2);
    assert_eq!(report.summary.services_missed, 1);
    assert_eq!(report.summary.outages, 4);
}

#[test]
fn a_member_of_another_customer_is_left_out() {
    let mut data = data();
    data.machines[1].customer_id = Uuid::from_u128(77);
    let report = build(&data, day(), t(2000));
    assert_eq!(report.services[0].members.len(), 1);
}

#[test]
fn a_report_of_now_stops_at_now_and_says_what_goes_on() {
    let mut data = data();
    data.incidents = vec![incident(Some(WEB2), None, "offline", 100, 200)];
    let report = build(&data, day(), t(200));
    let web2 = &report.machines[1];
    assert_eq!(web2.numbers.counted_seconds, 200.0 * 60.0);
    assert!(web2.outage_list[0].ongoing);
}

#[test]
fn maintenance_lists_what_it_covered() {
    let report = build(&data(), day(), t(2000));
    assert_eq!(report.maintenance.len(), 2);
    assert_eq!(report.maintenance[0].servers, ["web-1"]);
    // The window and the check silence, each minute once.
    assert_eq!(report.summary.maintenance_seconds, 155.0 * 60.0);
}

#[test]
fn a_server_added_later_counts_from_then() {
    let mut data = data();
    data.machines[1].created_at = t(720);
    data.incidents.clear();
    let report = build(&data, day(), t(2000));
    assert_eq!(report.machines[1].numbers.counted_seconds, 720.0 * 60.0);
}

#[test]
fn what_depends_on_a_parent_is_unreachable_while_it_is_down() {
    let switch = Uuid::from_u128(30);
    let router = Uuid::from_u128(31);
    let mut data = data();
    data.silences.clear();
    data.incidents.clear();
    data.machines[0].relays.clear();
    let device = |id, name: &str| Machine {
        device: true,
        ..machine(id, name, vec![])
    };
    // The router is outside the report, the switch in it.
    data.machines.push(device(switch, "sw-core"));
    data.parents.push(device(router, "rt-edge"));
    data.dependencies = vec![
        Dependency {
            server_id: WEB1,
            check: None,
            parent: Parent::Server(switch),
        },
        Dependency {
            server_id: switch,
            check: None,
            parent: Parent::Server(router),
        },
        // Only web-2's disk check is behind web-1's cpu check.
        Dependency {
            server_id: WEB2,
            check: Some(disk()),
            parent: Parent::Check(
                WEB1,
                CheckRef {
                    name: "cpu".into(),
                    instance: String::new(),
                },
            ),
        },
    ];
    data.names.push("ping".into());
    data.incidents = vec![
        // The switch's ping fails 100-110; its cpu check does not count.
        incident(Some(switch), None, "monitorflow_ping_loss_ratio", 100, 110),
        incident(Some(switch), None, "monitorflow_cpu_usage_ratio", 200, 230),
        // The router behind it, 300-320.
        incident(Some(router), None, "monitorflow_ping_loss_ratio", 300, 320),
        incident(Some(WEB1), None, "monitorflow_cpu_usage_ratio", 400, 405),
        incident(Some(WEB2), None, "monitorflow_disk_used_ratio", 500, 510),
    ];
    let report = build(&data, day(), t(2000));
    let web1 = report.machines.iter().find(|m| m.id == WEB1).unwrap();
    // Behind the switch (100-110) and, through it, the router (300-320),
    // and its own cpu (400-405).
    assert_eq!(web1.numbers.down_seconds, 35.0 * 60.0);
    let via: Vec<_> = web1
        .outage_list
        .iter()
        .map(|o| o.causes[0].via.clone())
        .collect();
    assert_eq!(via, [Some("sw-core".into()), Some("sw-core".into()), None]);
    // The router is read, never reported.
    assert!(report.machines.iter().all(|m| m.id != router));
    // Web-2 is not behind web-1 as a whole: only its disk check is.
    let web2 = report.machines.iter().find(|m| m.id == WEB2).unwrap();
    assert_eq!(web2.numbers.down_seconds, 10.0 * 60.0);
    let disk_check = report.checks.iter().find(|c| c.server_id == WEB2).unwrap();
    // Its own 10 minutes, the cpu check's 5, and the 30 that check was
    // unreachable itself, behind web-1's switch and router.
    assert_eq!(disk_check.numbers.down_seconds, 45.0 * 60.0);
    assert_eq!(
        disk_check.outage_list[0].causes[0].via.as_deref(),
        Some("web-1 cpu")
    );
}

#[test]
fn planned_work_on_a_parent_is_planned_for_what_is_behind_it() {
    let switch = Uuid::from_u128(30);
    let mut data = data();
    data.silences.clear();
    data.machines[0].relays.clear();
    data.machines.push(Machine {
        device: true,
        ..machine(switch, "sw-core", vec![])
    });
    data.dependencies = vec![Dependency {
        server_id: WEB1,
        check: None,
        parent: Parent::Server(switch),
    }];
    data.names.push("ping".into());
    data.incidents = vec![incident(
        Some(switch),
        None,
        "monitorflow_ping_loss_ratio",
        100,
        160,
    )];
    data.silences = vec![silence(Some(switch), None, 90, 130)];
    let report = build(&data, day(), t(2000));
    let web1 = report.machines.iter().find(|m| m.id == WEB1).unwrap();
    assert_eq!(web1.numbers.down_seconds, 30.0 * 60.0);
}

#[test]
fn a_cycle_written_past_the_api_ends() {
    let mut data = data();
    data.dependencies = vec![
        Dependency {
            server_id: WEB1,
            check: None,
            parent: Parent::Server(WEB2),
        },
        Dependency {
            server_id: WEB2,
            check: None,
            parent: Parent::Server(WEB1),
        },
    ];
    let report = build(&data, day(), t(2000));
    assert_eq!(report.machines.len(), 2);
}
