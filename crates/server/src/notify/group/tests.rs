use chrono::{DateTime, Utc};
use serde_json::json;

use super::*;

const WEB: &str = "https://mf.example";
const ACME: Uuid = Uuid::from_u128(1);
const OTHER: Uuid = Uuid::from_u128(2);
const PARIS: Uuid = Uuid::from_u128(10);
const LYON: Uuid = Uuid::from_u128(11);
const GROUP_ID: Uuid = Uuid::from_u128(99);

fn at() -> DateTime<Utc> {
    DateTime::from_timestamp(1_800_000_000, 0).unwrap()
}

/// An incident event of server `host` at `site` of `customer`.
fn incident(
    id: i64,
    kind: &str,
    customer: Uuid,
    site: Uuid,
    host: &str,
    metric: &str,
    severity: &str,
) -> Event {
    let site_name = if site == PARIS { "paris" } else { "lyon" };
    Event {
        id,
        event: kind.into(),
        customer_id: Some(customer),
        site_id: Some(site),
        details: json!({
            "customer": {"id": customer, "name": if customer == ACME { "acme" } else { "globex" }},
            "site": {"id": site, "name": site_name},
            "server": {"id": format!("srv-{host}"), "name": host},
            "relay": null,
            "incident": {"id": format!("inc-{host}-{metric}"), "metric": metric, "instance": "",
                         "severity": severity, "value": 97.0},
        }),
        created_at: at(),
    }
}

fn offline(id: i64, kind: &str, site: Uuid, host: &str) -> Event {
    incident(id, kind, ACME, site, host, "offline", "critical")
}

fn relay_offline(id: i64, kind: &str, name: &str) -> Event {
    let mut event = offline(id, kind, PARIS, name);
    event.details["server"] = Value::Null;
    event.details["relay"] = json!({"id": format!("relay-{name}"), "name": name});
    event
}

fn owed(mut event: Event) -> Event {
    event.details["silence"] = json!({"ended_at": "2027-01-15T09:00:00Z"});
    event
}

fn groups<'a>(out: &'a [Outgoing<'a>]) -> Vec<(Kind, Vec<i64>)> {
    out.iter()
        .filter_map(|o| match o {
            Outgoing::Group(g) => Some((g.kind, g.members.iter().map(|e| e.id).collect())),
            Outgoing::One(_) => None,
        })
        .collect()
}

fn ones(out: &[Outgoing]) -> Vec<i64> {
    out.iter()
        .filter_map(|o| match o {
            Outgoing::One(e) => Some(e.id),
            Outgoing::Group(_) => None,
        })
        .collect()
}

fn first_group<'a>(events: &[&'a Event]) -> Group<'a> {
    match group(events).into_iter().next() {
        Some(Outgoing::Group(g)) => g,
        other => panic!("not a group: {other:?}"),
    }
}

#[test]
fn a_lone_event_is_sent_as_before() {
    let e = offline(1, "incident.opened", PARIS, "web-1");
    assert_eq!(group(&[&e]), [Outgoing::One(&e)]);
}

#[test]
fn the_same_check_on_many_servers_is_one_group_across_sites() {
    let events: Vec<Event> = (1..=4)
        .map(|n| {
            let site = if n % 2 == 0 { PARIS } else { LYON };
            incident(
                n,
                "incident.opened",
                ACME,
                site,
                &format!("web-{n}"),
                "monitorflow_disk_used_percent",
                "warning",
            )
        })
        .collect();
    let refs: Vec<&Event> = events.iter().collect();
    let out = group(&refs);
    assert_eq!(groups(&out), [(Kind::Check, vec![1, 2, 3, 4])]);
    let g = first_group(&refs);
    assert_eq!(
        title(&g),
        "Warning: disk_used_percent on 4 servers in 2 sites of acme"
    );
}

#[test]
fn what_is_left_of_one_site_is_one_group() {
    let disk = incident(
        1,
        "incident.opened",
        ACME,
        PARIS,
        "web-1",
        "monitorflow_disk_used_percent",
        "warning",
    );
    let cpu = incident(
        2,
        "incident.opened",
        ACME,
        PARIS,
        "web-2",
        "monitorflow_cpu_usage_percent",
        "critical",
    );
    let elsewhere = incident(
        3,
        "incident.opened",
        ACME,
        LYON,
        "web-3",
        "monitorflow_load",
        "warning",
    );
    let out = group(&[&disk, &cpu, &elsewhere]);
    assert_eq!(groups(&out), [(Kind::Site, vec![1, 2])]);
    assert_eq!(ones(&out), [3]);
    assert_eq!(
        title(&first_group(&[&disk, &cpu])),
        "Critical: 2 alerts on 2 servers at acme / paris"
    );
}

#[test]
fn resolutions_are_grouped_apart_from_problems() {
    let events = [
        offline(1, "incident.opened", PARIS, "web-1"),
        offline(2, "incident.opened", PARIS, "web-2"),
        offline(3, "incident.resolved", PARIS, "web-3"),
        offline(4, "incident.resolved", PARIS, "web-4"),
    ];
    let refs: Vec<&Event> = events.iter().collect();
    let out = group(&refs);
    assert_eq!(
        groups(&out),
        [(Kind::Check, vec![1, 2]), (Kind::Check, vec![3, 4])]
    );
    let Outgoing::Group(back) = &out[1] else {
        panic!()
    };
    assert_eq!(
        title(back),
        "Resolved: 2 servers are back online at acme / paris"
    );
    let mut removed = events[3].clone();
    removed.details["incident"]["reason"] = "revoked".into();
    let mixed = first_group(&[&events[2], &removed]);
    assert_eq!(
        title(&mixed),
        "Resolved: 2 servers no longer offline at acme / paris"
    );
}

#[test]
fn a_group_never_mixes_customers() {
    let a = offline(1, "incident.opened", PARIS, "web-1");
    let b = incident(
        2,
        "incident.opened",
        OTHER,
        PARIS,
        "web-2",
        "offline",
        "critical",
    );
    let out = group(&[&a, &b]);
    assert_eq!(ones(&out), [1, 2]);
    assert!(groups(&out).is_empty());
}

#[test]
fn other_events_and_events_of_no_customer_go_alone() {
    let a = offline(1, "incident.opened", PARIS, "web-1");
    let mut approval = offline(2, "approval.waiting", PARIS, "web-2");
    approval.details["approval"] = json!({"command": "uptime"});
    let mut backup = offline(3, "backup.failed", PARIS, "web-3");
    backup.customer_id = None;
    let mut nobody = offline(4, "incident.opened", PARIS, "web-4");
    nobody.customer_id = None;
    assert_eq!(
        ones(&group(&[&a, &approval, &backup, &nobody])),
        [1, 2, 3, 4]
    );
}

#[test]
fn a_relay_and_the_servers_behind_it_read_together() {
    let relay = relay_offline(1, "incident.opened", "gw-1");
    let a = offline(2, "incident.opened", PARIS, "web-1");
    let b = offline(3, "incident.opened", PARIS, "web-2");
    let g = first_group(&[&relay, &a, &b]);
    assert_eq!(
        title(&g),
        "Critical: relay gw-1 and 2 servers are offline at acme / paris"
    );
    let one = first_group(&[&relay, &a]);
    assert_eq!(
        title(&one),
        "Critical: relay gw-1 and 1 server are offline at acme / paris"
    );
}

#[test]
fn an_incident_that_got_worse_within_the_window_is_told_once() {
    let opened = incident(
        1,
        "incident.opened",
        ACME,
        PARIS,
        "web-1",
        "monitorflow_load",
        "warning",
    );
    let escalated = incident(
        2,
        "incident.escalated",
        ACME,
        PARIS,
        "web-1",
        "monitorflow_load",
        "critical",
    );
    let other = incident(
        3,
        "incident.opened",
        ACME,
        PARIS,
        "web-2",
        "monitorflow_load",
        "warning",
    );
    let out = group(&[&opened, &escalated, &other]);
    assert_eq!(groups(&out), [(Kind::Check, vec![2, 3])]);
    let Outgoing::Group(g) = &out[0] else {
        panic!()
    };
    assert_eq!(title(g), "Critical: load on 2 servers at acme / paris");
    // Alone, it keeps its latest change.
    assert_eq!(group(&[&opened, &escalated]), [Outgoing::One(&escalated)]);
}

#[test]
fn what_a_silence_held_back_is_one_group_per_customer() {
    let open = owed(incident(
        1,
        "incident.opened",
        ACME,
        PARIS,
        "web-1",
        "monitorflow_load",
        "critical",
    ));
    let back = owed(offline(2, "incident.resolved", LYON, "web-2"));
    let fresh = offline(3, "incident.opened", PARIS, "web-3");
    let out = group(&[&open, &back, &fresh]);
    assert_eq!(groups(&out), [(Kind::Silence, vec![1, 2])]);
    assert_eq!(ones(&out), [3]);
    let Outgoing::Group(g) = &out[0] else {
        panic!()
    };
    assert_eq!(
        title(g),
        "Critical after a silence in 2 sites of acme: 1 incident still open, 1 resolved"
    );
    // A lone owed event keeps its own message.
    assert_eq!(group(&[&open]), [Outgoing::One(&open)]);
}

#[test]
fn a_group_message_lists_its_members_and_links_to_them() {
    let events: Vec<Event> = (1..=3)
        .map(|n| offline(n, "incident.opened", PARIS, &format!("web-{n}")))
        .collect();
    let refs: Vec<&Event> = events.iter().collect();
    let g = first_group(&refs);
    let [email] = &render_group(&g, GROUP_ID, WEB, None)[..] else {
        panic!()
    };
    assert_eq!(
        email.text,
        "Critical: 3 servers are offline at acme / paris"
    );
    assert_eq!(email.subject, format!("[MonitorFlow] {}", email.text));
    assert_eq!(email.url, format!("{WEB}/incidents/?group={GROUP_ID}"));
    assert_eq!(email.lines[0], "Critical: web-1 (acme / paris) is offline");
    assert!(email
        .body
        .contains("- Critical: web-3 (acme / paris) is offline\n"));
    assert!(email.body.contains("Site: paris\n"));
    assert_eq!(email.payload["event"], GROUP);
    assert_eq!(email.payload["incident"]["severity"], "critical");
    assert_eq!(email.payload["group"]["size"], 3);
    assert_eq!(email.payload["group"]["hosts"], "3 servers");
    assert_eq!(rows(&email.payload), [1, 2, 3]);
    assert!(!silence::owed(&email.payload));
}

#[test]
fn a_long_list_ends_with_how_many_more() {
    let events: Vec<Event> = (1..=50)
        .map(|n| offline(n, "incident.opened", PARIS, &format!("web-{n}")))
        .collect();
    let refs: Vec<&Event> = events.iter().collect();
    let g = first_group(&refs);
    // Email lists everything.
    assert_eq!(render_group(&g, GROUP_ID, WEB, None)[0].lines.len(), 50);
    for service in [Service::Slack, Service::Teams, Service::Discord] {
        let messages = render_group(&g, GROUP_ID, WEB, Some(service));
        let [message] = &messages[..] else {
            panic!("{service:?}")
        };
        let max = service.group_lines();
        assert_eq!(message.lines.len(), max);
        assert_eq!(message.lines[max - 1], format!("and {} more", 51 - max));
        // Every member is counted, for a channel that must keep them.
        assert_eq!(rows(&message.payload).len(), 50);
    }
}

#[test]
fn what_is_owed_is_named_in_as_many_messages_as_needed() {
    let events: Vec<Event> = (1..=30)
        .map(|n| owed(offline(n, "incident.opened", PARIS, &format!("web-{n}"))))
        .collect();
    let refs: Vec<&Event> = events.iter().collect();
    let g = first_group(&refs);
    let messages = render_group(&g, GROUP_ID, WEB, Some(Service::Discord));
    assert_eq!(messages.len(), 3);
    assert_eq!(
        messages[0].text,
        "Critical after a silence at acme / paris: 30 incidents still open (1 of 3)"
    );
    let named: Vec<&String> = messages.iter().flat_map(|m| &m.lines).collect();
    assert_eq!(named.len(), 30);
    assert!(named.iter().all(|l| !l.starts_with("and ")));
    for message in &messages {
        assert!(silence::owed(&message.payload));
        assert_eq!(rows(&message.payload).len(), message.lines.len());
    }
}

#[test]
fn every_service_shows_a_group_within_its_limits() {
    let events: Vec<Event> = (1..=40)
        .map(|n| {
            offline(
                n,
                "incident.opened",
                PARIS,
                &format!("{}-{n}", "<!here>*_~`[x](y)".repeat(10)),
            )
        })
        .collect();
    let refs: Vec<&Event> = events.iter().collect();
    let g = first_group(&refs);
    for service in [Service::Slack, Service::Teams, Service::Discord] {
        for message in render_group(&g, GROUP_ID, WEB, Some(service)) {
            let payload = service.payload(&message);
            let body = payload.to_string();
            assert!(body.contains(&format!("group={GROUP_ID}")), "{body}");
            match service {
                Service::Slack => {
                    let blocks = payload["blocks"].as_array().unwrap();
                    assert!(blocks.len() <= 50);
                    for block in blocks {
                        let text = block["text"]["text"].as_str().unwrap_or_default();
                        assert!(text.encode_utf16().count() <= 3000);
                    }
                    assert!(body.contains("Affected: 40 servers"), "{body}");
                }
                Service::Teams => {
                    assert!(body.len() < 24 * 1024);
                    assert!(body.contains("\"color\":\"Attention\""), "{body}");
                }
                Service::Discord => {
                    let embed = &payload["embeds"][0];
                    let description = embed["description"].as_str().unwrap();
                    assert!(description.encode_utf16().count() <= 4096);
                    // The list ends with what is left, not cut short.
                    assert!(description.contains("and 29 more"), "{description}");
                }
            }
        }
    }
}

#[test]
fn a_resolved_group_looks_resolved_everywhere() {
    let events = [
        offline(1, "incident.resolved", PARIS, "web-1"),
        offline(2, "incident.resolved", PARIS, "web-2"),
    ];
    let g = first_group(&[&events[0], &events[1]]);
    let message = &render_group(&g, GROUP_ID, WEB, Some(Service::Teams))[0];
    let teams = Service::Teams.payload(message).to_string();
    assert!(teams.contains("\"color\":\"Good\""), "{teams}");
    assert!(teams.contains("Resolved"), "{teams}");
    let discord = Service::Discord.payload(message);
    assert_eq!(discord["embeds"][0]["color"], 0x002e_a043);
}
