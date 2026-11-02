use chrono::{NaiveDate, NaiveTime};
use chrono_tz::Tz;
use monitorflow_common::redact::Secret;
use serde_json::json;

use super::*;
use crate::notify::chat::Service;
use crate::notify::group::{self, render_group, Outgoing};
use crate::notify::message::{render, with_acknowledge};
use crate::notify::on_call::{Person, Rota, Rotation};
use crate::notify::{Chat, Email, Webhook};

const ALICE: Uuid = Uuid::from_u128(1);
const BOB: Uuid = Uuid::from_u128(2);
const CAROL: Uuid = Uuid::from_u128(3);
const BOSS: Uuid = Uuid::from_u128(4);
const ACME: Uuid = Uuid::from_u128(10);
const OTHER: Uuid = Uuid::from_u128(11);
const PARIS: Uuid = Uuid::from_u128(20);
const LYON: Uuid = Uuid::from_u128(21);
const WEB: &str = "https://mf.example";

fn utc(text: &str) -> DateTime<Utc> {
    text.parse().unwrap()
}

fn person(id: Uuid, grants: Vec<(bool, Option<Uuid>, Option<Uuid>)>) -> Person {
    Person {
        id,
        username: ["", "alice", "bob", "carol", "boss"][id.as_u128() as usize].into(),
        disabled: false,
        operates: grants.iter().map(|(_, c, s)| (*c, *s)).collect(),
        grants,
        email: Some(format!("u{}@x.io", id.as_u128())),
        chat: None,
    }
}

fn directory() -> Directory {
    let people = vec![
        person(ALICE, vec![(false, Some(ACME), None)]),
        person(BOB, vec![(false, None, Some(PARIS))]),
        person(CAROL, vec![(false, None, Some(LYON))]),
        person(BOSS, vec![(true, Some(ACME), None)]),
    ];
    Directory {
        people: people.into_iter().map(|p| (p.id, p)).collect(),
        site_customer: [(PARIS, ACME), (LYON, ACME)].into(),
    }
}

fn channel(id: u128, destination: Destination, coverage: Coverage) -> Channel {
    Channel {
        id: Uuid::from_u128(id),
        destination,
        secret: Some(Secret::new("pw")),
        coverage,
        public_only: false,
        person: false,
    }
}

fn mail(id: u128, coverage: Coverage) -> Channel {
    channel(
        id,
        Destination::Email(Email {
            host: "smtp.x.io".into(),
            port: 587,
            username: "mf".into(),
            from: "mf@x.io".into(),
            to: vec!["team@x.io".into()],
        }),
        coverage,
    )
}

fn slack(id: u128, name: &str, coverage: Coverage) -> Channel {
    channel(
        id,
        Destination::Slack(Chat {
            name: name.into(),
            host: "hooks.slack.com".into(),
        }),
        coverage,
    )
}

/// A daily rotation of Alice then Bob, handing off at 09:00 UTC, the first
/// turn Alice's on 2026-11-02.
fn schedule(id: u128, coverage: Coverage) -> Schedule {
    Schedule {
        id: Uuid::from_u128(id),
        coverage,
        rota: Rota {
            time_zone: Tz::UTC,
            rotation: Rotation::Daily,
            handoff: NaiveTime::from_hms_opt(9, 0, 0).unwrap(),
            anchor: NaiveDate::from_ymd_opt(2026, 11, 2).unwrap(),
            anchor_position: 0,
            members: vec![Some(ALICE), Some(BOB)],
        },
        overrides: Vec::new(),
    }
}

#[test]
fn the_default_tells_the_administrators_after_15_minutes_once() {
    let steps = default_steps();
    assert_eq!(steps.len(), 1);
    assert_eq!(steps[0].after_minutes, 15);
    assert!(steps[0].administrators);
    assert!(steps[0].on_call.is_empty() && steps[0].users.is_empty());
    assert_eq!(steps_for(&[], Some(ACME), Some(PARIS)), steps);
}

#[test]
fn the_narrowest_follow_up_applies() {
    let policy = |id: u128, coverage, after| Policy {
        id: Uuid::from_u128(id),
        coverage,
        steps: vec![Step {
            after_minutes: after,
            administrators: true,
            ..Step::default()
        }],
    };
    let policies = vec![
        policy(1, Coverage::Everything, 30),
        policy(2, Coverage::Customer(ACME), 20),
        policy(3, Coverage::Site(PARIS), 5),
    ];
    let wait = |customer, site| steps_for(&policies, customer, site)[0].after_minutes;
    assert_eq!(wait(Some(ACME), Some(PARIS)), 5);
    assert_eq!(wait(Some(ACME), Some(LYON)), 20);
    assert_eq!(wait(Some(OTHER), None), 30);
    assert_eq!(policy_for(&policies[2..], Some(ACME), Some(LYON)), None);
    // An empty follow-up says "nobody after the first message".
    let silent = vec![Policy {
        id: Uuid::from_u128(4),
        coverage: Coverage::Site(PARIS),
        steps: Vec::new(),
    }];
    assert!(steps_for(&silent, Some(ACME), Some(PARIS)).is_empty());
    assert_eq!(steps_for(&silent, Some(ACME), Some(LYON)), default_steps());
}

#[test]
fn a_step_tells_only_who_sees_the_place() {
    let d = directory();
    let channels = vec![
        mail(1, Coverage::Customer(ACME)),
        slack(2, "#ops", Coverage::Customer(ACME)),
        slack(3, "#lyon", Coverage::Site(LYON)),
        slack(4, "#other", Coverage::Customer(OTHER)),
    ];
    let step = Step {
        after_minutes: 10,
        on_call: vec![Uuid::from_u128(50), Uuid::from_u128(51)],
        users: vec![BOB, CAROL, Uuid::from_u128(77)],
        channels: vec![Uuid::from_u128(2), Uuid::from_u128(3), Uuid::from_u128(4)],
        administrators: true,
    };
    let schedules = vec![
        schedule(50, Coverage::Customer(ACME)),
        schedule(51, Coverage::Site(LYON)),
    ];
    // On 2026-11-02 at 10:00 Alice is on call; Carol only sees Lyon, the
    // Lyon schedule does not cover Paris, nor do #lyon and #other; Boss is
    // the nearest administrator; Bob is named twice and told once.
    let told = recipients(
        &step,
        &schedules,
        &d,
        &channels,
        (Some(ACME), Some(PARIS)),
        utc("2026-11-02T10:00:00Z"),
    );
    assert_eq!(told.users, vec![ALICE, BOB, BOSS]);
    assert_eq!(told.channels, vec![Uuid::from_u128(2)]);
    assert_eq!(
        names(&told, &d, &channels),
        ["alice", "bob", "boss", "Slack #ops"]
    );
    // A day later the rotation has handed off to Bob, who sees only Paris:
    // the customer's schedule has a gap, which its administrator covers.
    let told = recipients(
        &step,
        &schedules,
        &d,
        &channels,
        (Some(ACME), Some(PARIS)),
        utc("2026-11-03T10:00:00Z"),
    );
    assert_eq!(told.users, vec![BOSS, BOB]);
}

#[test]
fn channels_are_named_without_their_addresses() {
    assert_eq!(
        label(&slack(1, "#ops", Coverage::Everything).destination),
        "Slack #ops"
    );
    assert_eq!(
        label(&slack(1, "", Coverage::Everything).destination),
        "Slack"
    );
    assert_eq!(
        label(&mail(1, Coverage::Everything).destination),
        "an email channel"
    );
    let hook = Destination::Webhook(Webhook {
        url: "https://secret.example/abc".into(),
    });
    assert_eq!(label(&hook), "a webhook");
}

/// A follow-up step about a server of Paris, as `take_due` writes it.
fn step_event(id: i64, host: &str, minutes: i64) -> Event {
    Event {
        id,
        event: "incident.opened".into(),
        customer_id: Some(ACME),
        site_id: Some(PARIS),
        details: json!({
            "customer": {"id": ACME, "name": "acme"},
            "site": {"id": PARIS, "name": "paris"},
            "server": {"id": format!("srv-{host}"), "name": host},
            "incident": {"id": format!("inc-{host}"), "metric": "offline", "instance": "",
                         "severity": "critical"},
            "follow_up": {"step": 1, "steps": 2, "minutes": minutes},
        }),
        created_at: utc("2026-11-02T10:00:00Z"),
    }
}

#[test]
fn a_step_says_how_long_nobody_acknowledged() {
    let message = render(&step_event(7, "web-1", 15), WEB);
    assert_eq!(
        message.text,
        "Not acknowledged after 15 minutes. Critical: web-1 (acme / paris) is offline"
    );
    assert_eq!(message.payload["follow_up"]["step"], 1);
    assert_eq!(message.payload["event"], "incident.opened");
    assert!(is_step(&message.payload));
    let one = render(&step_event(7, "web-1", 1), WEB);
    assert!(one.text.starts_with("Not acknowledged after 1 minute. "));
}

#[test]
fn steps_are_grouped_apart_from_fresh_news() {
    let mut fresh = step_event(1, "db-1", 0);
    fresh.details.as_object_mut().unwrap().remove("follow_up");
    let steps = [step_event(2, "web-1", 15), step_event(3, "web-2", 16)];
    let events: Vec<&Event> = std::iter::once(&fresh).chain(steps.iter()).collect();
    let out = group::group(&events);
    assert_eq!(out.len(), 2, "{out:?}");
    assert!(matches!(out[0], Outgoing::One(e) if e.id == 1));
    let Outgoing::Group(group) = &out[1] else {
        panic!("{out:?}")
    };
    assert_eq!(group.members.len(), 2);
    let messages = render_group(group, Uuid::from_u128(9), WEB, Some(Service::Slack));
    assert_eq!(
        messages[0].text,
        "Not acknowledged after 15 minutes. Critical: 2 servers are offline at acme / paris"
    );
}

#[test]
fn a_personal_message_carries_the_link_that_acknowledges() {
    let url = "https://mf.example/acknowledge/#abc";
    let message = with_acknowledge(render(&step_event(7, "web-1", 15), WEB), url);
    assert_eq!(message.payload["acknowledge_url"], url);
    let lines: Vec<&str> = message.body.lines().collect();
    assert_eq!(lines[2], "https://mf.example/incidents/view/?id=inc-web-1");
    assert_eq!(
        lines[4],
        format!("Acknowledge, to say you are on it: {url}")
    );
    let slack = Service::Slack.payload(&message).to_string();
    assert!(slack.contains("|Acknowledge>"), "{slack}");
    let teams = Service::Teams.payload(&message);
    let actions = &teams["attachments"][0]["content"]["actions"];
    assert_eq!(actions[1]["title"], "Acknowledge");
    assert_eq!(actions[1]["url"], url);
    let discord = Service::Discord.payload(&message).to_string();
    assert!(
        discord.contains("[Acknowledge](https://mf.example/acknowledge/#abc)"),
        "{discord}"
    );
    // A channel's message has no such link.
    let plain = Service::Slack.payload(&render(&step_event(7, "web-1", 15), WEB));
    assert!(!plain.to_string().contains("Acknowledge"));
}

#[test]
fn only_operators_and_above_of_the_place_acknowledge() {
    let mut d = directory();
    let alice = d.people[&ALICE].clone();
    assert!(d.acknowledges(&alice, Coverage::Site(PARIS)));
    let bob = d.people[&BOB].clone();
    assert!(d.acknowledges(&bob, Coverage::Site(PARIS)));
    assert!(!d.acknowledges(&bob, Coverage::Site(LYON)));
    let viewer = d.people.get_mut(&CAROL).unwrap();
    viewer.operates.clear();
    let viewer = viewer.clone();
    assert!(!d.acknowledges(&viewer, Coverage::Site(LYON)));
    let mut disabled = alice;
    disabled.disabled = true;
    assert!(!d.acknowledges(&disabled, Coverage::Site(PARIS)));
}

#[test]
fn steps_parse_strictly() {
    let step: Step =
        serde_json::from_value(json!({"after_minutes": 5, "administrators": true})).unwrap();
    assert_eq!(step.after_minutes, 5);
    assert!(serde_json::from_value::<Step>(json!({"after_minutes": 5, "pager": true})).is_err());
}
