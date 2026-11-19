use std::collections::HashMap;

use chrono::TimeDelta;
use uuid::Uuid;

use super::*;
use crate::availability::data::{Data, Incident, Machine, Member, Service};
use crate::availability::report::build;
use crate::availability::{Rule, Spans};

fn at(text: &str) -> DateTime<Utc> {
    DateTime::parse_from_rfc3339(text).unwrap().to_utc()
}

/// October 2026 with `outages` offline incidents of 10 minutes on web-1.
fn report(outages: usize) -> Report {
    let web = Uuid::from_u128(1);
    let start = at("2026-10-01T00:00:00Z");
    let data = Data {
        machines: vec![Machine {
            id: web,
            name: "web-1".into(),
            device: false,
            site_id: Uuid::from_u128(2),
            site_name: "paris".into(),
            customer_id: Uuid::from_u128(3),
            relays: vec![],
            created_at: at("2026-01-01T00:00:00Z"),
            revoked_at: None,
        }],
        relays: HashMap::new(),
        incidents: (0..outages)
            .map(|i| {
                let from = start + TimeDelta::hours(i64::try_from(i).unwrap() + 1);
                Incident {
                    id: Uuid::new_v4(),
                    server_id: Some(web),
                    relay_id: None,
                    metric: "offline".into(),
                    instance: String::new(),
                    critical: Spans::one(from, from + TimeDelta::minutes(10)),
                    hidden: false,
                }
            })
            .collect(),
        silences: vec![],
        services: vec![Service {
            id: Uuid::from_u128(4),
            customer_id: Uuid::from_u128(3),
            name: "Équipe web".into(),
            rule: Rule::All,
            target: Some(99.9),
            members: vec![Member {
                server_id: web,
                check: None,
            }],
        }],
        dependencies: vec![],
        parents: vec![],
        pings: HashMap::new(),
        names: vec![],
    };
    let period = (start, at("2026-11-01T00:00:00Z"));
    build(&data, period, at("2026-11-01T06:00:00Z"))
}

fn contains(haystack: &[u8], needle: &[u8]) -> bool {
    haystack.windows(needle.len()).any(|w| w == needle)
}

/// Whether `pdf` shows `text` as one string, as a content stream writes it.
fn shows(pdf: &[u8], text: &str) -> bool {
    let mut content = Content::new();
    content.show(Str(&encode(text)));
    let written = content.finish().into_vec();
    let string = written.strip_suffix(b" Tj").unwrap_or(&written);
    contains(pdf, string)
}

#[test]
fn text_is_encoded_for_the_base_fonts() {
    assert_eq!(
        encode("Rapport de disponibilité"),
        b"Rapport de disponibilit\xe9"
    );
    assert_eq!(encode("l’avance"), b"l\x92avance");
    assert_eq!(encode("\u{a0}%"), b"\xa0%");
    assert_eq!(encode("日本"), b"??");
    // "Hello" in Helvetica at 10 points: H 722, e 556, l 222, l 222, o 556.
    assert!((width("Hello", 10.0, false) - 22.78).abs() < 1e-3);
    assert!(width("Hello", 10.0, true) > width("Hello", 10.0, false));
}

#[test]
fn long_text_is_wrapped_and_cut() {
    let lines = wrap("one two three four five six", 10.0, false, 60.0);
    assert!(lines.len() > 1);
    assert!(lines.iter().all(|l| width(l, 10.0, false) <= 60.0));
    let cut = fit("a-very-long-server-name.example", 10.0, false, 50.0);
    assert!(
        cut.ends_with("...") && width(&cut, 10.0, false) <= 50.0,
        "{cut}"
    );
}

#[test]
fn a_cut_is_the_longest_start_that_fits_however_long_the_text() {
    // What popping one character at a time found, for every room.
    let slow = |text: &str, room: f32| {
        let mut cut = text.to_owned();
        while !cut.is_empty() && width(&format!("{cut}..."), 10.0, false) > room {
            cut.pop();
        }
        format!("{}...", cut.trim_end())
    };
    let text = "web-1, web-2, café-é, srv 3";
    for room in (0..120).map(|r| r as f32 * 0.7) {
        if width(text, 10.0, false) > room {
            assert_eq!(fit(text, 10.0, false, room), slow(text, room), "{room}");
        }
    }
    // The servers a site-wide maintenance covered, in one cell.
    let names: Vec<String> = (0..20_000).map(|n| format!("server-{n:05}")).collect();
    let covered = names.join(", ");
    let started = std::time::Instant::now();
    let cut = fit(&covered, BODY, false, 120.0);
    assert!(started.elapsed() < std::time::Duration::from_secs(2));
    assert!(
        cut.starts_with("server-00000") && cut.ends_with("..."),
        "{cut}"
    );
    assert!(width(&cut, BODY, false) <= 120.0);
}

#[test]
fn periods_in_words() {
    let paris = chrono_tz::Europe::Paris;
    let october = (at("2026-09-30T22:00:00Z"), at("2026-10-31T23:00:00Z"));
    assert_eq!(
        period_words(october.0, october.1, paris, Language::En),
        "October 2026"
    );
    assert_eq!(
        period_words(october.0, october.1, paris, Language::Fr),
        "octobre 2026"
    );
    let week = (at("2026-11-09T00:00:00Z"), at("2026-11-16T00:00:00Z"));
    assert_eq!(
        period_words(week.0, week.1, Tz::UTC, Language::En),
        "9 November 2026 to 15 November 2026"
    );
    let hours = (at("2026-11-09T08:00:00Z"), at("2026-11-09T12:30:00Z"));
    assert_eq!(
        period_words(hours.0, hours.1, Tz::UTC, Language::Fr),
        "9 novembre 2026 08:00 au 9 novembre 2026 12:30"
    );
}

#[test]
fn a_report_reads_summary_first() {
    let pdf = render(&report(3), Tz::UTC, Language::En);
    assert!(pdf.starts_with(b"%PDF-"));
    for text in [
        "Availability report",
        "October 2026",
        "Summary",
        "Met",
        "How availability is measured",
        "web-1: offline",
        "Page 1 / 1",
    ] {
        assert!(shows(&pdf, text), "{text}");
    }
    assert!(contains(
        &pdf,
        b"There were 3 outages, 30 min in all; the longest lasted 10 min."
    ));
    let summary = pdf.windows(9).position(|w| w == b"(Summary)").unwrap();
    let outages = pdf.windows(9).position(|w| w == b"(Outages)").unwrap();
    assert!(summary < outages);
    // Nothing internal: no IDs.
    assert!(!contains(&pdf, Uuid::from_u128(1).to_string().as_bytes()));
}

#[test]
fn a_french_report_in_many_pages() {
    let pdf = render(&report(120), chrono_tz::Europe::Paris, Language::Fr);
    for text in ["Rapport de disponibilité", "Résumé", "Équipe web", "Manqué"] {
        assert!(shows(&pdf, text), "{text}");
    }
    // Every page has its number, of the same total.
    let pages = (1..10)
        .take_while(|n| contains(&pdf, format!("(Page {n} / ").as_bytes()))
        .count();
    assert!(pages >= 3, "{pages} pages");
    assert!(contains(
        &pdf,
        format!("(Page {pages} / {pages})").as_bytes()
    ));
}
