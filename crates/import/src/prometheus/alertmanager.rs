//! From Alertmanager's receivers to notification channels, and from its
//! silences to silences of hosts.
//!
//! A receiver with several configs gives one channel per config. Secrets
//! Alertmanager's API hides (`<secret>`) or reads from a file are not in
//! the import: a mail server password is then typed after the import, and
//! a webhook or chat URL, being the channel itself, cannot be moved.
//! Routes are not moved: a MonitorFlow channel covers everything or a site.

use std::collections::BTreeSet;

use chrono::{DateTime, NaiveDate, NaiveTime, Utc};
use monitorflow_common::redact::Secret;
use serde_json::Value;

use crate::args;
use crate::model::{
    Channel, Fidelity, Inventory, Origin, Repeat, Rule, Silence, SilenceScope, Smtp, Target,
    Unmovable, Watch, Window, EVERY_MONTH,
};
use crate::notes::Note;
use crate::text::{host_of, port_of};

use super::config::{AmConfig, AmGlobal, IntervalSpec, Route};
use super::hosts::Fleet;
use super::promql::{MatchOp, Matcher};

/// How Alertmanager's API shows a secret.
const HIDDEN: &str = "<secret>";
/// Port of a smarthost written without one.
const SMTP_PORT: u16 = 25;

/// A silence, from Alertmanager's API.
#[derive(Debug, Clone, PartialEq)]
pub struct SilenceIn {
    pub id: String,
    pub matchers: Vec<Matcher>,
    pub starts_at: DateTime<Utc>,
    pub ends_at: DateTime<Utc>,
    pub comment: String,
}

/// The string at `key` of a config entry, if set.
fn string(entry: &Value, key: &str) -> Option<String> {
    entry
        .get(key)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_owned)
}

/// A secret of a config entry: its value, or why it is not in the import.
fn secret(
    entry: &Value,
    key: &str,
    file_key: &str,
    defaults: (Option<&String>, Option<&String>),
) -> Result<String, Note> {
    let value = string(entry, key).or_else(|| defaults.0.cloned());
    let file = string(entry, file_key).or_else(|| defaults.1.cloned());
    match (value, file) {
        (Some(v), _) if v == HIDDEN => Err(Note::plain("secret_hidden")),
        (Some(v), _) => Ok(v),
        (None, Some(file)) => Err(Note::new("secret_in_file", args! {"file" => file})),
        (None, None) => Err(Note::plain("secret_hidden")),
    }
}

/// The mail server of an email config, with the global defaults.
fn smtp(entry: &Value, global: &AmGlobal) -> Option<Smtp> {
    let smarthost = string(entry, "smarthost").or_else(|| global.smtp_smarthost.clone())?;
    let host = host_of(&smarthost);
    if host.is_empty() {
        return None;
    }
    let port = port_of(&smarthost).unwrap_or(SMTP_PORT);
    let username = string(entry, "auth_username")
        .or_else(|| global.smtp_auth_username.clone())
        .unwrap_or_default();
    let from = string(entry, "from")
        .or_else(|| global.smtp_from.clone())
        .unwrap_or_default();
    let password = secret(
        entry,
        "auth_password",
        "auth_password_file",
        (
            global.smtp_auth_password.as_ref(),
            global.smtp_auth_password_file.as_ref(),
        ),
    )
    .ok()
    .map(Secret::new);
    Some(Smtp {
        host,
        port,
        username,
        from,
        password,
    })
}

/// Channels of the receivers of `config`, and what cannot be moved.
pub fn channels(inventory: &mut Inventory, config: &AmConfig) {
    for receiver in &config.receivers {
        let entries: Vec<(&str, &Value)> = receiver
            .configs
            .iter()
            .filter(|(key, _)| key.ends_with("_configs"))
            .flat_map(|(key, list)| {
                list.as_array()
                    .into_iter()
                    .flatten()
                    .map(move |entry| (key.as_str(), entry))
            })
            .collect();
        let results: Vec<(&str, Result<Target, Note>)> = entries
            .into_iter()
            .map(|(key, entry)| (key, target(key, entry, &config.global)))
            .collect();
        // Channels are numbered when a receiver gives more than one.
        let many = results.iter().filter(|(_, r)| r.is_ok()).count() > 1;
        let mut n = 0;
        for (key, result) in results {
            let origin = Origin::new("receiver", &receiver.name).with_detail(key);
            match result {
                Ok(target) => {
                    n += 1;
                    let name = if many {
                        format!("{} {n}", receiver.name)
                    } else {
                        receiver.name.clone()
                    };
                    inventory.channels.push(Channel {
                        name,
                        target,
                        origin,
                        fidelity: Fidelity::Exact,
                    });
                }
                Err(why) => inventory.unmovable.push(Unmovable {
                    hosts: vec![],
                    origin,
                    why,
                }),
            }
        }
    }
    if config.route.as_ref().is_some_and(|r| r.routes_by_labels()) {
        inventory.unmovable.push(Unmovable {
            hosts: vec![],
            origin: Origin::new("receiver", "route"),
            why: Note::plain("routes"),
        });
    }
}

/// Where one config of a receiver sends, or why it cannot be moved.
fn target(key: &str, entry: &Value, global: &AmGlobal) -> Result<Target, Note> {
    let url =
        |key: &str, file_key: &str| secret(entry, key, file_key, (None, None)).map(Secret::new);
    match key {
        "email_configs" => {
            let to: Vec<String> = string(entry, "to")
                .unwrap_or_default()
                .split(',')
                .map(|t| t.trim().to_owned())
                .filter(|t| !t.is_empty())
                .collect();
            if to.is_empty() || to.iter().any(|t| t.contains("{{")) {
                return Err(Note::plain("no_equivalent"));
            }
            Ok(Target::Email {
                to,
                smtp: smtp(entry, global),
            })
        }
        "webhook_configs" => Ok(Target::Webhook {
            url: url("url", "url_file")?,
        }),
        "slack_configs" => {
            if string(entry, "app_token").is_some() || string(entry, "app_token_file").is_some() {
                return Err(Note::plain("chat_token"));
            }
            let url = secret(
                entry,
                "api_url",
                "api_url_file",
                (
                    global.slack_api_url.as_ref(),
                    global.slack_api_url_file.as_ref(),
                ),
            )?;
            Ok(Target::Slack {
                url: Secret::new(url),
            })
        }
        "msteams_configs" | "msteamsv2_configs" => Ok(Target::Teams {
            url: url("webhook_url", "webhook_url_file")?,
        }),
        "discord_configs" => Ok(Target::Discord {
            url: url("webhook_url", "webhook_url_file")?,
        }),
        _ => Err(Note::plain("no_target")),
    }
}

/// Labels that say which host an alert is about.
const HOST_LABELS: &[&str] = &[
    "instance",
    "host",
    "hostname",
    "nodename",
    "instance_name",
    "job",
];

/// Whether `matcher` keeps a host known by `names`: a positive matcher
/// when one name matches, a negative one when every name does.
fn keeps(matcher: &Matcher, names: &BTreeSet<String>) -> bool {
    match matcher.op {
        MatchOp::Eq | MatchOp::Re => names.iter().any(|n| matcher.matches(n)),
        MatchOp::Ne | MatchOp::Nre => names.iter().all(|n| matcher.matches(n)),
    }
}

/// Silences of the hosts their matchers name.
pub fn silences(fleet: &mut Fleet, silences: &[SilenceIn]) {
    for silence in silences {
        let shown = silence
            .matchers
            .iter()
            .map(Matcher::to_string)
            .collect::<Vec<_>>()
            .join(", ");
        let name = if silence.comment.trim().is_empty() {
            silence.id.clone()
        } else {
            silence.comment.trim().to_owned()
        };
        let origin = Origin::new("silence", name).with_detail(&shown);
        let (on_hosts, others): (Vec<&Matcher>, Vec<&Matcher>) = silence
            .matchers
            .iter()
            .partition(|m| HOST_LABELS.contains(&m.label.as_str()));
        let (alertnames, others): (Vec<&Matcher>, Vec<&Matcher>) =
            others.into_iter().partition(|m| m.label == ALERTNAME);
        let hosts: Vec<String> = if on_hosts.is_empty() {
            vec![]
        } else {
            fleet
                .inventory
                .hosts
                .iter()
                .map(|h| h.key.clone())
                .filter(|key| {
                    let names = fleet.names_of(key);
                    let jobs = fleet.jobs_of(key);
                    on_hosts.iter().all(|m| {
                        if m.label == "job" {
                            keeps(m, &jobs)
                        } else {
                            keeps(m, &names)
                        }
                    })
                })
                .collect()
        };
        if hosts.is_empty() {
            fleet.inventory.unmovable.push(Unmovable {
                hosts: vec![],
                origin,
                why: Note::plain("no_hosts"),
            });
            continue;
        }
        let (checks, fidelity) = checks_of(&fleet.inventory, &hosts, &alertnames);
        let fidelity = if others.is_empty() {
            fidelity
        } else {
            let matchers = others
                .iter()
                .map(|m| m.to_string())
                .collect::<Vec<_>>()
                .join(", ");
            Fidelity::Approximated(Note::new(
                "silence_whole_server",
                args! {"matchers" => matchers},
            ))
        };
        fleet.inventory.silences.push(Silence {
            scope: SilenceScope::Hosts(hosts),
            checks,
            starts_at: silence.starts_at,
            ends_at: silence.ends_at,
            reason: silence.comment.trim().to_owned(),
            origin,
            fidelity,
        });
    }
}

/// The label naming an alerting rule.
const ALERTNAME: &str = "alertname";

/// The checks the alerting rules `alertnames` select made on `hosts`,
/// and how faithful that is; no matcher is every check.
fn checks_of(
    inventory: &Inventory,
    hosts: &[String],
    alertnames: &[&Matcher],
) -> (Vec<Watch>, Fidelity) {
    if alertnames.is_empty() {
        return (vec![], Fidelity::Exact);
    }
    let named = |origin: &Origin| {
        origin.kind == "alerting_rule" && alertnames.iter().all(|m| m.matches(&origin.name))
    };
    let mut checks: Vec<Watch> = Vec::new();
    let mut add = |watch: Watch| {
        if !checks.contains(&watch) {
            checks.push(watch);
        }
    };
    for check in &inventory.checks {
        if hosts.contains(&check.host) && named(&check.origin) {
            add(check.watch.clone());
        }
    }
    for threshold in &inventory.thresholds {
        if threshold.hosts.iter().any(|h| hosts.contains(h)) && named(&threshold.origin) {
            add(threshold.measure.watch(&inventory.checks));
        }
    }
    let offline = checks.contains(&Watch::Agent);
    checks.retain(|w| *w != Watch::Agent);
    let shown = alertnames
        .iter()
        .map(|m| m.to_string())
        .collect::<Vec<_>>()
        .join(", ");
    if checks.is_empty() {
        let note = if offline {
            Note::plain("window_offline_kept")
        } else {
            Note::new("silence_whole_server", args! {"matchers" => shown})
        };
        return (vec![], Fidelity::Approximated(note));
    }
    let fidelity = if offline {
        Fidelity::Approximated(Note::plain("window_offline_kept"))
    } else {
        Fidelity::Exact
    };
    (checks, fidelity)
}

/// A matcher of a route's `matchers`: `severity="critical"`,
/// `alertname=~"Disk.*"`.
pub fn parse_matcher(text: &str) -> Option<Matcher> {
    // The first operator in the text; `=~` before `=` at the same place.
    let mut found: Option<(usize, MatchOp, usize)> = None;
    for (token, op) in [
        ("=~", MatchOp::Re),
        ("!~", MatchOp::Nre),
        ("!=", MatchOp::Ne),
        ("=", MatchOp::Eq),
    ] {
        if let Some(at) = text.find(token) {
            if found.as_ref().is_none_or(|(best, ..)| at < *best) {
                found = Some((at, op, token.len()));
            }
        }
    }
    let (at, op, len) = found?;
    let label = text[..at].trim().trim_start_matches('{').trim().to_owned();
    let value = text[at + len..].trim().trim_end_matches('}').trim();
    let value = value
        .strip_prefix('"')
        .and_then(|v| v.strip_suffix('"'))
        .unwrap_or(value)
        .to_owned();
    (!label.is_empty()).then_some(Matcher { label, op, value })
}

/// What a route selects: its own matchers and its parents'.
fn route_matchers(route: &Route, inherited: &[Matcher]) -> Vec<Matcher> {
    let mut all = inherited.to_vec();
    all.extend(route.matchers.iter().filter_map(|m| parse_matcher(m)));
    all.extend(route.match_.iter().map(|(label, value)| Matcher {
        label: label.clone(),
        op: MatchOp::Eq,
        value: value.clone(),
    }));
    all.extend(route.match_re.iter().map(|(label, value)| Matcher {
        label: label.clone(),
        op: MatchOp::Re,
        value: value.clone(),
    }));
    all
}

const WEEKDAYS: [&str; 7] = [
    "monday",
    "tuesday",
    "wednesday",
    "thursday",
    "friday",
    "saturday",
    "sunday",
];
const MONTHS: [&str; 12] = [
    "january",
    "february",
    "march",
    "april",
    "may",
    "june",
    "july",
    "august",
    "september",
    "october",
    "november",
    "december",
];

/// `monday:friday`, `1:5`, `june`: the inclusive range of an entry, by
/// name or number (1-based for names). `None` when it is not one.
fn range(entry: &str, names: &[&str]) -> Option<(i32, i32)> {
    let value = |text: &str| -> Option<i32> {
        let text = text.trim().to_ascii_lowercase();
        names
            .iter()
            .position(|n| *n == text)
            .and_then(|i| i32::try_from(i + 1).ok())
            .or_else(|| text.parse().ok())
    };
    match entry.split_once(':') {
        Some((a, b)) => Some((value(a)?, value(b)?)),
        None => value(entry).map(|v| (v, v)),
    }
}

/// Minutes since midnight of `HH:MM`, `24:00` included.
fn minutes(text: &str) -> Option<u32> {
    let (h, m) = text.trim().split_once(':')?;
    let (h, m): (u32, u32) = (h.parse().ok()?, m.parse().ok()?);
    (h <= 24 && m < 60 && h * 60 + m <= 1440).then_some(h * 60 + m)
}

/// The rules of one time interval, and what is approximated; or why it
/// cannot be moved.
pub fn interval_rules(spec: &IntervalSpec) -> Result<(Vec<Rule>, Vec<Note>), Note> {
    let unsaid = |detail: &str| Note::new("am_interval", args! {"detail" => detail});
    let mut notes = Vec::new();
    let mut times = Vec::new();
    for t in &spec.times {
        let (Some(start), Some(end)) = (minutes(&t.start_time), minutes(&t.end_time)) else {
            return Err(unsaid("a time is not HH:MM"));
        };
        if end <= start {
            return Err(unsaid("a time range ends before it starts"));
        }
        times.push((start, end - start));
    }
    if times.is_empty() {
        times.push((0, 1440));
    }
    let mut weekdays = 0u8;
    for entry in &spec.weekdays {
        let (a, b) = range(entry, &WEEKDAYS).ok_or_else(|| unsaid("an unknown weekday"))?;
        for day in a.max(1)..=b.min(7) {
            weekdays |= 1 << (day - 1);
        }
    }
    let mut months = 0u16;
    for entry in &spec.months {
        let (a, b) = range(entry, &MONTHS).ok_or_else(|| unsaid("an unknown month"))?;
        for month in a.max(1)..=b.min(12) {
            months |= 1 << (month - 1);
        }
    }
    let months = if months == 0 { EVERY_MONTH } else { months };
    let mut days = Vec::new();
    for entry in &spec.days_of_month {
        let (a, b) = range(entry, &[]).ok_or_else(|| unsaid("an unknown day of the month"))?;
        if a < 1 || b < 1 {
            return Err(unsaid("days counted from the end of the month"));
        }
        days.extend((a..=b.min(31)).filter_map(|d| u8::try_from(d).ok()));
    }
    let (mut starts_on, mut ends_on) = (None, None);
    if !spec.years.is_empty() {
        let mut years: Vec<i32> = Vec::new();
        for entry in &spec.years {
            let (a, b) = range(entry, &[]).ok_or_else(|| unsaid("an unknown year"))?;
            years.extend(a..=b);
        }
        years.sort_unstable();
        years.dedup();
        let (first, last) = (years[0], years[years.len() - 1]);
        if usize::try_from(last - first + 1).ok() != Some(years.len()) {
            notes.push(Note::plain("am_years"));
        }
        starts_on = NaiveDate::from_ymd_opt(first, 1, 1);
        ends_on = NaiveDate::from_ymd_opt(last, 12, 31);
    }
    // Alertmanager reads times in UTC unless told; `Local` is its host's
    // zone, the one chosen for the import.
    let time_zone = match spec.location.as_deref() {
        None => Some("UTC".to_owned()),
        Some("Local") => None,
        Some(zone) => Some(zone.to_owned()),
    };
    let base = Rule {
        repeat: Repeat::Daily,
        every: 1,
        weekdays: 0,
        month_day: None,
        month_week: None,
        months: EVERY_MONTH,
        start_time: NaiveTime::MIN,
        duration_minutes: 0,
        time_zone,
        starts_on,
        ends_on,
    };
    let shapes: Vec<Rule> = if !days.is_empty() {
        if weekdays != 0 {
            notes.push(unsaid(
                "weekdays and days of the month together; the days are kept",
            ));
        }
        days.iter()
            .map(|d| Rule {
                repeat: Repeat::Monthly,
                month_day: Some(*d),
                months,
                ..base.clone()
            })
            .collect()
    } else if weekdays != 0 {
        if months != EVERY_MONTH {
            notes.push(Note::plain("am_months_weekly"));
        }
        vec![Rule {
            repeat: Repeat::Weekly,
            weekdays,
            ..base.clone()
        }]
    } else {
        if months != EVERY_MONTH {
            notes.push(Note::plain("am_months_weekly"));
        }
        vec![base.clone()]
    };
    let mut rules = Vec::new();
    for shape in shapes {
        for (start, length) in &times {
            rules.push(Rule {
                start_time: NaiveTime::from_num_seconds_from_midnight_opt(start * 60 % 86_400, 0)
                    .unwrap_or_default(),
                duration_minutes: *length,
                ..shape.clone()
            });
        }
    }
    Ok((rules, notes))
}

/// The time intervals routes are muted in, as maintenance windows of the
/// hosts and checks the routes select (ADR-0151).
pub fn windows(fleet: &mut Fleet, config: &AmConfig) {
    let Some(root) = &config.route else { return };
    let mut stack: Vec<(&Route, Vec<Matcher>)> = vec![(root, vec![])];
    while let Some((route, inherited)) = stack.pop() {
        let matchers = route_matchers(route, &inherited);
        for child in &route.routes {
            stack.push((child, matchers.clone()));
        }
        for name in &route.active_time_intervals {
            fleet.inventory.unmovable.push(Unmovable {
                hosts: vec![],
                origin: Origin::new("time_interval", name),
                why: Note::plain("am_active_intervals"),
            });
        }
        if route.mute_time_intervals.is_empty() {
            continue;
        }
        let (on_hosts, others): (Vec<&Matcher>, Vec<&Matcher>) = matchers
            .iter()
            .partition(|m| HOST_LABELS.contains(&m.label.as_str()));
        let (alertnames, others): (Vec<&Matcher>, Vec<&Matcher>) =
            others.into_iter().partition(|m| m.label == ALERTNAME);
        let hosts: Vec<String> = fleet
            .inventory
            .hosts
            .iter()
            .map(|h| h.key.clone())
            .filter(|key| {
                let names = fleet.names_of(key);
                let jobs = fleet.jobs_of(key);
                on_hosts.iter().all(|m| {
                    if m.label == "job" {
                        keeps(m, &jobs)
                    } else {
                        keeps(m, &names)
                    }
                })
            })
            .collect();
        let scope = if on_hosts.is_empty() {
            SilenceScope::Everything
        } else {
            SilenceScope::Hosts(hosts.clone())
        };
        let (checks, mut fidelity) = checks_of(&fleet.inventory, &hosts, &alertnames);
        if !others.is_empty() {
            let shown = others
                .iter()
                .map(|m| m.to_string())
                .collect::<Vec<_>>()
                .join(", ");
            fidelity =
                Fidelity::Approximated(Note::new("am_route_labels", args! {"matchers" => shown}));
        }
        for name in &route.mute_time_intervals {
            let origin = Origin::new("time_interval", name);
            let named = config
                .time_intervals
                .iter()
                .chain(&config.mute_time_intervals)
                .find(|i| i.name == *name);
            let Some(named) = named else {
                fleet.inventory.unmovable.push(Unmovable {
                    hosts: vec![],
                    origin,
                    why: Note::new("am_interval", args! {"detail" => "it is not defined"}),
                });
                continue;
            };
            if matches!(scope, SilenceScope::Hosts(ref h) if h.is_empty()) {
                fleet.inventory.unmovable.push(Unmovable {
                    hosts: vec![],
                    origin,
                    why: Note::plain("no_hosts"),
                });
                continue;
            }
            for spec in &named.time_intervals {
                match interval_rules(spec) {
                    Ok((rules, notes)) => {
                        let fidelity = match (&fidelity, notes.into_iter().next()) {
                            (Fidelity::Exact, Some(note)) => Fidelity::Approximated(note),
                            (f, _) => f.clone(),
                        };
                        for rule in rules {
                            fleet.inventory.windows.push(Window {
                                scope: scope.clone(),
                                checks: checks.clone(),
                                rule,
                                reason: name.clone(),
                                origin: origin.clone(),
                                fidelity: fidelity.clone(),
                            });
                        }
                    }
                    Err(why) => fleet.inventory.unmovable.push(Unmovable {
                        hosts: vec![],
                        origin: origin.clone(),
                        why,
                    }),
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::super::targets::Target as ScrapeTarget;
    use super::super::Collected;
    use super::*;
    use crate::model::Measure;

    fn config(text: &str) -> AmConfig {
        serde_norway::from_str(text).unwrap()
    }

    #[test]
    fn receivers_become_channels() {
        let config = config(
            r#"
global:
  smtp_smarthost: mail.example.com:587
  smtp_from: alerts@example.com
  smtp_auth_username: alerts@example.com
  smtp_auth_password: s3cret
route:
  receiver: ops
  routes:
    - matchers: ['severity="critical"']
      receiver: pager
receivers:
  - name: ops
    email_configs:
      - to: ops@example.com, admin@example.com
      - to: night@example.com
        smarthost: relay.example.com
        auth_username: ''
    slack_configs:
      - api_url: https://hooks.slack.com/services/T0/B0/X
  - name: pager
    pagerduty_configs:
      - routing_key: abc
    webhook_configs:
      - url: http://hooks.example.com/pager
  - name: hidden
    webhook_configs:
      - url: <secret>
    discord_configs:
      - webhook_url_file: /etc/alertmanager/discord
"#,
        );
        let mut inventory = Inventory::default();
        channels(&mut inventory, &config);
        let names: Vec<_> = inventory.channels.iter().map(|c| c.name.as_str()).collect();
        // One movable config of two is not numbered.
        assert_eq!(names, vec!["ops 1", "ops 2", "ops 3", "pager"]);
        let Target::Email {
            to,
            smtp: Some(smtp),
        } = &inventory.channels[0].target
        else {
            panic!("email");
        };
        assert_eq!(
            to,
            &vec!["ops@example.com".to_owned(), "admin@example.com".into()]
        );
        assert_eq!((smtp.host.as_str(), smtp.port), ("mail.example.com", 587));
        assert_eq!(smtp.username, "alerts@example.com");
        assert_eq!(smtp.password.as_ref().unwrap().expose(), "s3cret");
        let Target::Email {
            smtp: Some(smtp), ..
        } = &inventory.channels[1].target
        else {
            panic!("email");
        };
        assert_eq!((smtp.host.as_str(), smtp.port), ("relay.example.com", 25));
        let Target::Slack { url } = &inventory.channels[2].target else {
            panic!("slack");
        };
        assert_eq!(url.expose(), "https://hooks.slack.com/services/T0/B0/X");
        let why: Vec<_> = inventory
            .unmovable
            .iter()
            .map(|u| (u.origin.name.as_str(), u.why.text()))
            .collect();
        assert_eq!(why[0], ("pager", Note::plain("no_target").text()));
        // Configs of a receiver come in the order of their kinds' names.
        assert_eq!(why[1].0, "hidden");
        assert!(why[1].1.contains("/etc/alertmanager/discord"));
        assert_eq!(why[2], ("hidden", Note::plain("secret_hidden").text()));
        assert_eq!(why[3], ("route", Note::plain("routes").text()));
    }

    #[test]
    fn hidden_passwords_are_left_out() {
        let config = config(
            "global: {smtp_smarthost: 'mail:25', smtp_auth_username: u, smtp_auth_password: <secret>}\nreceivers: [{name: m, email_configs: [{to: a@example.com}]}]",
        );
        let mut inventory = Inventory::default();
        channels(&mut inventory, &config);
        let Target::Email {
            smtp: Some(smtp), ..
        } = &inventory.channels[0].target
        else {
            panic!("email");
        };
        assert!(smtp.password.is_none());
        assert_eq!(smtp.username, "u");
    }

    fn matcher(label: &str, op: MatchOp, value: &str) -> Matcher {
        Matcher {
            label: label.into(),
            op,
            value: value.into(),
        }
    }

    #[test]
    fn silences_cover_the_hosts_they_match() {
        let mut collected = Collected::default();
        for (job, address) in [
            ("node", "web-01:9100"),
            ("node", "db-01:9100"),
            ("db", "db-02:9100"),
        ] {
            collected.targets.push(ScrapeTarget {
                job: job.into(),
                address: address.into(),
                labels: [("job".to_owned(), job.to_owned())].into(),
                ..ScrapeTarget::default()
            });
        }
        let mut fleet = Fleet::new(&collected);
        let at = |h| DateTime::parse_from_rfc3339(h).unwrap().with_timezone(&Utc);
        let silence = |matchers, comment: &str| SilenceIn {
            id: "1".into(),
            matchers,
            starts_at: at("2026-11-17T10:00:00Z"),
            ends_at: at("2026-11-17T12:00:00Z"),
            comment: comment.into(),
        };
        silences(
            &mut fleet,
            &[
                silence(vec![matcher("instance", MatchOp::Re, "db-.*")], "Upgrade"),
                silence(
                    vec![
                        matcher("instance", MatchOp::Eq, "web-01:9100"),
                        matcher("alertname", MatchOp::Eq, "HostHighCpuLoad"),
                    ],
                    "Batch",
                ),
                silence(vec![matcher("job", MatchOp::Ne, "node")], "Not node"),
                silence(vec![matcher("instance", MatchOp::Eq, "nowhere")], "None"),
                silence(vec![matcher("alertname", MatchOp::Eq, "X")], ""),
            ],
        );
        let s = &fleet.inventory.silences;
        assert_eq!(s.len(), 3);
        assert_eq!(
            s[0].scope,
            SilenceScope::Hosts(vec!["db-01".into(), "db-02".into()])
        );
        assert_eq!(s[0].reason, "Upgrade");
        assert_eq!(s[0].fidelity, Fidelity::Exact);
        assert_eq!(s[0].ends_at, at("2026-11-17T12:00:00Z"));
        assert_eq!(s[1].scope, SilenceScope::Hosts(vec!["web-01".into()]));
        assert_eq!(
            s[1].fidelity,
            Fidelity::Approximated(Note::new(
                "silence_whole_server",
                args! {"matchers" => "alertname=\"HostHighCpuLoad\""}
            ))
        );
        assert_eq!(s[2].scope, SilenceScope::Hosts(vec!["db-02".into()]));
        let unmovable: Vec<_> = fleet
            .inventory
            .unmovable
            .iter()
            .map(|u| (u.origin.name.as_str(), u.why.code.as_str()))
            .collect();
        assert_eq!(unmovable, vec![("None", "no_hosts"), ("1", "no_hosts")]);
    }

    fn fleet() -> Fleet {
        let mut collected = Collected::default();
        for address in ["web-01:9100", "db-01:9100"] {
            collected.targets.push(ScrapeTarget {
                job: "node".into(),
                address: address.into(),
                labels: [("job".to_owned(), "node".to_owned())].into(),
                ..ScrapeTarget::default()
            });
        }
        let mut fleet = Fleet::new(&collected);
        fleet.inventory.thresholds.push(crate::model::Threshold {
            hosts: vec!["web-01".into(), "db-01".into()],
            measure: Measure::CpuUsage,
            direction: crate::model::Direction::Above,
            warning: Some(0.8),
            critical: None,
            for_s: 300,
            recovery: None,
            origin: Origin::new("alerting_rule", "HostHighCpuLoad"),
            fidelity: Fidelity::Exact,
        });
        fleet
    }

    #[test]
    fn a_silence_of_an_alert_rule_silences_its_check() {
        let mut fleet = fleet();
        let at = |h| DateTime::parse_from_rfc3339(h).unwrap().with_timezone(&Utc);
        silences(
            &mut fleet,
            &[SilenceIn {
                id: "1".into(),
                matchers: vec![
                    matcher("instance", MatchOp::Eq, "web-01:9100"),
                    matcher("alertname", MatchOp::Eq, "HostHighCpuLoad"),
                ],
                starts_at: at("2026-11-17T10:00:00Z"),
                ends_at: at("2026-11-17T12:00:00Z"),
                comment: "Batch".into(),
            }],
        );
        let s = &fleet.inventory.silences[0];
        assert_eq!(s.scope, SilenceScope::Hosts(vec!["web-01".into()]));
        assert_eq!(s.checks, vec![Watch::Cpu]);
        assert_eq!(s.fidelity, Fidelity::Exact);
    }

    #[test]
    fn route_matchers_are_read() {
        let m = parse_matcher(r#"alertname=~"Host.*""#).unwrap();
        assert_eq!(
            (m.label.as_str(), m.op, m.value.as_str()),
            ("alertname", MatchOp::Re, "Host.*")
        );
        let m = parse_matcher("severity != critical").unwrap();
        assert_eq!(
            (m.label.as_str(), m.op, m.value.as_str()),
            ("severity", MatchOp::Ne, "critical")
        );
        let m = parse_matcher(r#"instance="db-01:9100""#).unwrap();
        assert_eq!((m.op, m.value.as_str()), (MatchOp::Eq, "db-01:9100"));
        assert!(parse_matcher("nothing").is_none());
    }

    #[test]
    fn time_intervals_become_rules() {
        let spec = |yaml: &str| -> IntervalSpec { serde_norway::from_str(yaml).unwrap() };
        // Weeknights in Paris.
        let (rules, notes) = interval_rules(&spec(
            "times: [{start_time: '22:00', end_time: '24:00'}]\nweekdays: ['monday:friday']\nlocation: Europe/Paris",
        ))
        .unwrap();
        assert!(notes.is_empty());
        assert_eq!(rules.len(), 1);
        assert_eq!(
            (rules[0].repeat, rules[0].weekdays),
            (Repeat::Weekly, 0b1_1111)
        );
        assert_eq!(
            (
                rules[0].start_time.to_string().as_str(),
                rules[0].duration_minutes
            ),
            ("22:00:00", 120)
        );
        assert_eq!(rules[0].time_zone.as_deref(), Some("Europe/Paris"));
        // The 1st and 15th of January to March, all day, in UTC by default.
        let (rules, _) = interval_rules(&spec(
            "days_of_month: ['1', '15']\nmonths: ['january:march']",
        ))
        .unwrap();
        assert_eq!(rules.len(), 2);
        assert_eq!(
            (rules[1].repeat, rules[1].month_day, rules[1].months),
            (Repeat::Monthly, Some(15), 0b111)
        );
        assert_eq!(
            (rules[1].duration_minutes, rules[1].time_zone.as_deref()),
            (1440, Some("UTC"))
        );
        // Years bound the dates; weekdays of some months are approximated.
        let (rules, notes) = interval_rules(&spec(
            "weekdays: [saturday]\nmonths: [december]\nyears: ['2026:2027']",
        ))
        .unwrap();
        assert_eq!(rules[0].starts_on, NaiveDate::from_ymd_opt(2026, 1, 1));
        assert_eq!(rules[0].ends_on, NaiveDate::from_ymd_opt(2027, 12, 31));
        assert_eq!(notes, vec![Note::plain("am_months_weekly")]);
        // `Local` is the zone chosen for the import.
        assert_eq!(
            interval_rules(&spec("location: Local")).unwrap().0[0].time_zone,
            None
        );
        // Days from the end of the month cannot be said.
        assert_eq!(
            interval_rules(&spec("days_of_month: ['-1']"))
                .unwrap_err()
                .code,
            "am_interval"
        );
    }

    #[test]
    fn muted_routes_become_maintenance_windows() {
        let mut fleet = fleet();
        let config: AmConfig = serde_norway::from_str(
            r#"
route:
  receiver: ops
  routes:
    - matchers: ['instance=~"db-.*"']
      mute_time_intervals: [backups]
      routes:
        - matchers: ['alertname="HostHighCpuLoad"']
          mute_time_intervals: [batch]
    - matchers: ['team="web"']
      mute_time_intervals: [nights]
    - matchers: ['instance="web-01:9100"']
      active_time_intervals: [office]
time_intervals:
  - name: backups
    time_intervals:
      - weekdays: [sunday]
        times: [{start_time: '01:00', end_time: '05:00'}]
  - name: batch
    time_intervals:
      - days_of_month: ['1']
mute_time_intervals:
  - name: nights
    time_intervals:
      - times: [{start_time: '00:00', end_time: '06:00'}]
receivers: [{name: ops}]
"#,
        )
        .unwrap();
        windows(&mut fleet, &config);
        let w = &fleet.inventory.windows;
        let backups = w.iter().find(|w| w.reason == "backups").unwrap();
        assert_eq!(backups.scope, SilenceScope::Hosts(vec!["db-01".into()]));
        assert!(backups.checks.is_empty());
        assert_eq!(backups.fidelity, Fidelity::Exact);
        let batch = w.iter().find(|w| w.reason == "batch").unwrap();
        assert_eq!(batch.checks, vec![Watch::Cpu]);
        assert_eq!(batch.rule.month_day, Some(1));
        let nights = w.iter().find(|w| w.reason == "nights").unwrap();
        assert_eq!(nights.scope, SilenceScope::Everything);
        assert!(
            matches!(&nights.fidelity, Fidelity::Approximated(n) if n.code == "am_route_labels")
        );
        assert!(fleet
            .inventory
            .unmovable
            .iter()
            .any(|u| u.origin.name == "office" && u.why.code == "am_active_intervals"));
    }
}
