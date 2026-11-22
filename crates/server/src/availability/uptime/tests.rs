use std::collections::HashMap;

use super::*;
use crate::availability::data::{Incident, Machine, Member, Service, Silence};
use crate::availability::Rule;

fn at(text: &str) -> DateTime<Utc> {
    text.parse().unwrap()
}

const WEB: Uuid = Uuid::from_u128(1);
const DB: Uuid = Uuid::from_u128(2);

fn machine(id: Uuid, name: &str, created: &str) -> Machine {
    Machine {
        id,
        name: name.into(),
        device: id == DB,
        site_id: Uuid::from_u128(4),
        site_name: "paris".into(),
        customer_id: Uuid::from_u128(5),
        relays: vec![],
        created_at: at(created),
        revoked_at: None,
    }
}

fn incident(server: Uuid, metric: &str, from: &str, to: &str) -> Incident {
    Incident {
        id: Uuid::new_v4(),
        server_id: Some(server),
        relay_id: None,
        metric: metric.into(),
        instance: String::new(),
        critical: Spans::one(at(from), at(to)),
        hidden: false,
    }
}

fn data() -> Data {
    Data {
        machines: vec![
            machine(WEB, "web-1", "2026-01-01T00:00:00Z"),
            // Added three days before the view.
            machine(DB, "switch-1", "2026-11-19T12:00:00Z"),
        ],
        relays: HashMap::new(),
        incidents: vec![
            // 2.4 hours down today, on its disk check.
            incident(
                WEB,
                "monitorflow_disk_used_ratio",
                "2026-11-22T10:00:00Z",
                "2026-11-22T12:24:00Z",
            ),
            // An hour offline 40 days ago: only the 90 days see it.
            incident(
                WEB,
                "offline",
                "2026-10-13T08:00:00Z",
                "2026-10-13T09:00:00Z",
            ),
        ],
        silences: vec![Silence {
            server_id: Some(WEB),
            relay_id: None,
            site_id: None,
            customer_id: None,
            checks: None,
            span: (at("2026-11-20T00:00:00Z"), at("2026-11-20T23:00:00Z")),
            reason: "Patches".into(),
            window: true,
        }],
        services: vec![Service {
            id: Uuid::from_u128(9),
            customer_id: Uuid::from_u128(5),
            name: "Website".into(),
            rule: Rule::All,
            target: None,
            members: vec![Member {
                server_id: WEB,
                check: None,
            }],
        }],
        dependencies: vec![],
        parents: vec![],
        pings: HashMap::new(),
        names: vec!["disk".into()],
    }
}

/// Sunday 22 November 2026, 18:00 in Paris.
fn now() -> DateTime<Utc> {
    at("2026-11-22T17:00:00Z")
}

#[test]
fn ninety_days_end_with_today_cut_at_local_midnight() {
    let (dates, start) = days(now(), chrono_tz::Europe::Paris);
    assert_eq!(dates.len(), 90);
    assert_eq!(dates.last(), Some(&"2026-11-22".parse().unwrap()));
    assert_eq!(dates[0], "2026-08-25".parse::<NaiveDate>().unwrap());
    // Paris is UTC+2 on 25 August.
    assert_eq!(start, at("2026-08-24T22:00:00Z"));
    // The 90-day window always starts before the first day's midnight.
    assert_eq!(period(now()), (at("2026-08-24T17:00:00Z"), now()));
}

#[test]
fn windows_and_days_come_from_the_same_down_time() {
    let zone = chrono_tz::Europe::Paris;
    let uptime = build(&data(), now(), zone, now());
    let kinds: Vec<(Kind, Option<&str>)> = uptime
        .rows
        .iter()
        .map(|r| (r.kind, r.check.as_deref()))
        .collect();
    assert_eq!(
        kinds,
        [
            (Kind::Service, None),
            (Kind::Server, None),
            (Kind::Check, Some("disk")),
            (Kind::Device, None),
        ]
    );
    let web = &uptime.rows[1];
    assert_eq!(web.outages, 2);
    // 2.4 h of the last 24 h: 90 %.
    let day = web.windows.day.unwrap();
    assert!((day - 90.0).abs() < 1e-9, "{day}");
    // 7 days minus the 23 h silence counted, 2.4 h down.
    let week = web.windows.week.unwrap();
    let counted = 7.0 * 24.0 - 23.0;
    assert!(
        (week - (counted - 2.4) / counted * 100.0).abs() < 1e-9,
        "{week}"
    );
    // The offline hour 40 days ago counts in the 90 days only.
    let quarter = web.windows.quarter.unwrap();
    let counted = 90.0 * 24.0 - 23.0;
    assert!(
        (quarter - (counted - 3.4) / counted * 100.0).abs() < 1e-9,
        "{quarter}"
    );

    // Today counts from Paris midnight (23:00 UTC) until now: 18 hours.
    let today = &web.days[89];
    assert_eq!(today.date, "2026-11-22".parse::<NaiveDate>().unwrap());
    assert_eq!(today.down_seconds, 2.4 * 3600.0);
    let expected = (18.0 - 2.4) / 18.0 * 100.0;
    assert!((today.availability.unwrap() - expected).abs() < 1e-9);
    assert_eq!(web.days[88].availability, Some(100.0));
    // The day of the silence: its 23 planned hours are left out, the hour
    // before it is the only time counted.
    let patched = &web.days[87];
    assert_eq!(patched.maintenance_seconds, 23.0 * 3600.0);
    assert_eq!(patched.availability, Some(100.0));
    // The service follows its one member.
    assert_eq!(uptime.rows[0].windows.day, web.windows.day);
}

#[test]
fn days_before_something_existed_are_not_measured() {
    let uptime = build(&data(), now(), chrono_tz::Europe::Paris, now());
    let switch = uptime.rows.last().unwrap();
    assert_eq!(switch.kind, Kind::Device);
    assert_eq!(switch.days[85].availability, None);
    assert_eq!(switch.days[86].availability, Some(100.0));
    assert_eq!(switch.windows.day, Some(100.0));
    assert_eq!(switch.windows.quarter, Some(100.0));
}
