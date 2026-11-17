//! From Alertmanager's receivers to notification channels, and from its
//! silences to silences of hosts.
//!
//! A receiver with several configs gives one channel per config. Secrets
//! Alertmanager's API hides (`<secret>`) or reads from a file are not in
//! the import: a mail server password is then typed after the import, and
//! a webhook or chat URL, being the channel itself, cannot be moved.
//! Routes are not moved: a MonitorFlow channel covers everything or a site.

use std::collections::BTreeSet;

use chrono::{DateTime, Utc};
use monitorflow_common::redact::Secret;
use serde_json::Value;

use crate::args;
use crate::model::{
    Channel, Fidelity, Inventory, Origin, Silence, SilenceScope, Smtp, Target, Unmovable,
};
use crate::notes::Note;
use crate::text::{host_of, port_of};

use super::config::{AmConfig, AmGlobal};
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
        let fidelity = if others.is_empty() {
            Fidelity::Exact
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
            starts_at: silence.starts_at,
            ends_at: silence.ends_at,
            reason: silence.comment.trim().to_owned(),
            origin,
            fidelity,
        });
    }
}

#[cfg(test)]
mod tests {
    use super::super::targets::Target as ScrapeTarget;
    use super::super::Collected;
    use super::*;

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
}
