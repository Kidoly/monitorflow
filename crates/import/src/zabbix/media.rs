//! Media types and users' media: notification channels.
//!
//! An email media type is a mail server; its recipients are the addresses
//! of the users who have it. A webhook media type posts to a URL: a
//! parameter of its own, or each user's "send to" when the parameter is
//! `{ALERT.SENDTO}` (the stock Microsoft Teams and Discord ones). Media
//! types that post with an app token (Slack, Telegram, Mattermost) need
//! an incoming webhook URL in MonitorFlow instead. A configuration read
//! from the API has users' media, and media types nobody uses are left
//! out; an export has none, so recipients are missing.

use monitorflow_common::redact::Secret;

use crate::args;
use crate::model::{Channel, Fidelity, Inventory, Origin, Smtp, Target, Unmovable};
use crate::notes::Note;

use super::config::{Config, MediaKind, MediaType};
use super::macros::Macros;

const SENDTO: &str = "{ALERT.SENDTO}";

/// Channels of `config` into `out`; `globals` resolves global macros in
/// parameters.
pub fn channels(config: &Config, globals: &Macros, out: &mut Inventory) {
    for media in &config.media_types {
        // Who receives it: `None` when the source cannot say.
        let recipients: Option<Vec<(String, String)>> = config.user_media.as_ref().map(|all| {
            all.iter()
                .filter(|m| m.enabled && m.media_type == media.name)
                .flat_map(|m| {
                    m.sendto
                        .iter()
                        .map(|to| (m.user.clone(), to.trim().to_owned()))
                        .filter(|(_, to)| !to.is_empty())
                        .collect::<Vec<_>>()
                })
                .collect()
        });
        if recipients.as_ref().is_some_and(Vec::is_empty) {
            // Nobody is sent anything through it.
            continue;
        }
        let origin = Origin::new("media_type", &media.name);
        let mut unmovable = |why: Note| {
            out.unmovable.push(Unmovable {
                hosts: vec![],
                origin: origin.clone(),
                why,
            });
        };
        if !media.enabled {
            unmovable(Note::plain("disabled"));
            continue;
        }
        match media.kind {
            MediaKind::Sms | MediaKind::Script => unmovable(Note::plain("no_target")),
            MediaKind::Email => {
                let Some(recipients) = recipients else {
                    unmovable(Note::plain("zabbix_no_recipients"));
                    continue;
                };
                let mut to: Vec<String> = recipients.into_iter().map(|(_, to)| to).collect();
                to.sort();
                to.dedup();
                out.channels.push(Channel {
                    name: media.name.clone(),
                    target: Target::Email {
                        to,
                        smtp: smtp(media),
                    },
                    origin,
                    fidelity: Fidelity::Exact,
                });
            }
            MediaKind::Webhook => webhook(media, recipients, globals, out),
        }
    }
}

fn smtp(media: &MediaType) -> Option<Smtp> {
    let host = media.smtp_server.trim();
    if host.is_empty() {
        return None;
    }
    let username = media.username.trim().to_owned();
    let from = media.smtp_email.trim();
    Some(Smtp {
        host: host.to_owned(),
        port: media.smtp_port,
        from: if from.is_empty() {
            username.clone()
        } else {
            from.to_owned()
        },
        username,
        password: media.password.as_deref().map(Secret::new),
    })
}

/// Whether a parameter name is that of the URL a webhook posts to.
fn is_url_name(name: &str) -> bool {
    let name = name.to_ascii_lowercase();
    matches!(
        name.as_str(),
        "url" | "uri" | "endpoint" | "webhook" | "webhook_url" | "hook" | "hook_url"
    ) || name.ends_with("_endpoint")
        || name.ends_with("_webhook")
        || name.ends_with("_webhook_url")
}

fn webhook(
    media: &MediaType,
    recipients: Option<Vec<(String, String)>>,
    globals: &Macros,
    out: &mut Inventory,
) {
    let origin = Origin::new("media_type", &media.name);
    let unmovable = |out: &mut Inventory, why: Note| {
        out.unmovable.push(Unmovable {
            hosts: vec![],
            origin: origin.clone(),
            why,
        });
    };
    let named = |words: &[&str]| {
        media
            .parameters
            .iter()
            .any(|(n, _)| words.iter().any(|w| n.to_ascii_lowercase().contains(w)))
    };
    let url_param = media.parameters.iter().find(|(n, _)| is_url_name(n));
    let url_param = match url_param {
        Some(p) => p,
        None if named(&["token"]) => return unmovable(out, Note::plain("chat_token")),
        None => {
            // A URL among the values, if nothing looks like a credential.
            let found = media.parameters.iter().find(|(_, v)| {
                let v = v.trim().to_ascii_lowercase();
                v.starts_with("http://") || v.starts_with("https://")
            });
            match found {
                Some(p) if !named(&["password", "secret", "key", "user"]) => p,
                _ => return unmovable(out, Note::plain("no_target")),
            }
        }
    };
    let (param, value) = url_param;
    let lower = media.name.to_ascii_lowercase();
    let param_lower = param.to_ascii_lowercase();
    let target = |url: String| {
        let url_lower = url.to_ascii_lowercase();
        if lower.contains("teams") || param_lower.starts_with("teams") {
            Target::Teams {
                url: Secret::new(url),
            }
        } else if lower.contains("discord") || param_lower.starts_with("discord") {
            Target::Discord {
                url: Secret::new(url),
            }
        } else if url_lower.contains("hooks.slack.com") {
            Target::Slack {
                url: Secret::new(url),
            }
        } else {
            Target::Webhook {
                url: Secret::new(url),
            }
        }
    };
    if value.trim() == SENDTO {
        let Some(recipients) = recipients else {
            return unmovable(out, Note::plain("zabbix_no_recipients"));
        };
        let mut seen: Vec<String> = Vec::new();
        for (user, to) in recipients {
            if seen.contains(&to) {
                continue;
            }
            seen.push(to.clone());
            let origin = Origin::new("user_media", format!("{} ({user})", media.name));
            if !to.starts_with("http://") && !to.starts_with("https://") {
                out.unmovable.push(Unmovable {
                    hosts: vec![],
                    origin,
                    why: Note::new("zabbix_not_set", args! {"setting" => param}),
                });
                continue;
            }
            out.channels.push(Channel {
                name: format!("{} ({user})", media.name),
                target: target(to),
                origin,
                fidelity: Fidelity::Exact,
            });
        }
        return;
    }
    let url = match globals.expand(value.trim()) {
        Ok(url) => url,
        Err(name) => return unmovable(out, Note::new("unresolved_macro", args! {"macro" => name})),
    };
    if !url.starts_with("http://") && !url.starts_with("https://") {
        return unmovable(out, Note::new("zabbix_not_set", args! {"setting" => param}));
    }
    out.channels.push(Channel {
        name: media.name.clone(),
        target: target(url),
        origin,
        fidelity: Fidelity::Exact,
    });
}

#[cfg(test)]
mod tests {
    use super::super::config::UserMedia;
    use super::*;

    fn media(name: &str, kind: MediaKind, parameters: &[(&str, &str)]) -> MediaType {
        MediaType {
            name: name.into(),
            kind,
            enabled: true,
            smtp_server: String::new(),
            smtp_port: 25,
            smtp_email: String::new(),
            username: String::new(),
            password: None,
            parameters: parameters
                .iter()
                .map(|(n, v)| ((*n).to_owned(), (*v).to_owned()))
                .collect(),
        }
    }

    fn mail() -> MediaType {
        let mut mail = media("Ops mail", MediaKind::Email, &[]);
        mail.smtp_server = "mail.example.com".into();
        mail.smtp_port = 587;
        mail.smtp_email = "zabbix@example.com".into();
        mail.username = "zabbix@example.com".into();
        mail.password = Some("example-only".into());
        mail
    }

    fn user(media: &str, sendto: &[&str]) -> UserMedia {
        UserMedia {
            user: "Admin".into(),
            media_type: media.into(),
            sendto: sendto.iter().map(|s| (*s).to_owned()).collect(),
            enabled: true,
        }
    }

    fn read(config: &Config) -> Inventory {
        let mut out = Inventory::default();
        channels(config, &Macros::default(), &mut out);
        out
    }

    fn reasons(out: &Inventory) -> Vec<(String, String)> {
        out.unmovable
            .iter()
            .map(|u| (u.origin.name.clone(), u.why.code.clone()))
            .collect()
    }

    #[test]
    fn email_with_users_addresses() {
        let config = Config {
            media_types: vec![mail(), media("Unused", MediaKind::Email, &[])],
            user_media: Some(vec![
                user("Ops mail", &["oncall@example.com", "admin@example.com"]),
                UserMedia {
                    enabled: false,
                    ..user("Ops mail", &["off@example.com"])
                },
            ]),
            ..Config::default()
        };
        let out = read(&config);
        assert_eq!(out.channels.len(), 1);
        let Target::Email { to, smtp } = &out.channels[0].target else {
            panic!("email expected")
        };
        assert_eq!(to, &["admin@example.com", "oncall@example.com"]);
        let smtp = smtp.as_ref().unwrap();
        assert_eq!(smtp.host, "mail.example.com");
        assert_eq!(smtp.port, 587);
        assert_eq!(smtp.from, "zabbix@example.com");
        assert_eq!(smtp.username, "zabbix@example.com");
        assert_eq!(smtp.password.as_ref().unwrap().expose(), "example-only");
        assert!(out.unmovable.is_empty());
    }

    #[test]
    fn exports_have_no_recipients() {
        let config = Config {
            media_types: vec![
                mail(),
                media(
                    "Ops webhook",
                    MediaKind::Webhook,
                    &[
                        ("URL", "https://hooks.example.com/zabbix/0123"),
                        ("Message", "{ALERT.MESSAGE}"),
                    ],
                ),
                media(
                    "MS Teams Workflow",
                    MediaKind::Webhook,
                    &[("teams_endpoint", SENDTO)],
                ),
                media(
                    "MS Teams",
                    MediaKind::Webhook,
                    &[("teams_endpoint", "<PLACE WEBHOOK URL HERE>")],
                ),
                media(
                    "Slack",
                    MediaKind::Webhook,
                    &[("bot_token", "<PLACE YOUR TOKEN>"), ("channel", SENDTO)],
                ),
                media(
                    "Jira",
                    MediaKind::Webhook,
                    &[
                        ("jira_url", "https://jira.example.com"),
                        ("jira_password", "x"),
                    ],
                ),
                media("SMS", MediaKind::Sms, &[]),
                MediaType {
                    enabled: false,
                    ..media("Off", MediaKind::Webhook, &[])
                },
            ],
            ..Config::default()
        };
        let out = read(&config);
        assert_eq!(out.channels.len(), 1);
        assert_eq!(out.channels[0].name, "Ops webhook");
        let Target::Webhook { url } = &out.channels[0].target else {
            panic!("webhook expected")
        };
        assert_eq!(url.expose(), "https://hooks.example.com/zabbix/0123");
        assert_eq!(
            reasons(&out),
            [
                ("Ops mail", "zabbix_no_recipients"),
                ("MS Teams Workflow", "zabbix_no_recipients"),
                ("MS Teams", "zabbix_not_set"),
                ("Slack", "chat_token"),
                ("Jira", "no_target"),
                ("SMS", "no_target"),
                ("Off", "disabled"),
            ]
            .map(|(a, b)| (a.to_owned(), b.to_owned()))
        );
    }

    #[test]
    fn chat_webhooks_from_users() {
        let config = Config {
            media_types: vec![
                media(
                    "MS Teams Workflow",
                    MediaKind::Webhook,
                    &[("teams_endpoint", SENDTO)],
                ),
                media(
                    "Discord",
                    MediaKind::Webhook,
                    &[("discord_endpoint", SENDTO)],
                ),
            ],
            user_media: Some(vec![
                user(
                    "MS Teams Workflow",
                    &["https://example.webhook.office.com/webhookb2/0123"],
                ),
                user("Discord", &["https://discord.com/api/webhooks/1/x"]),
            ]),
            ..Config::default()
        };
        let out = read(&config);
        assert_eq!(out.channels.len(), 2);
        assert_eq!(out.channels[0].name, "MS Teams Workflow (Admin)");
        assert_eq!(out.channels[0].origin.kind, "user_media");
        let Target::Teams { url } = &out.channels[0].target else {
            panic!("teams expected")
        };
        assert_eq!(
            url.expose(),
            "https://example.webhook.office.com/webhookb2/0123"
        );
        assert!(matches!(out.channels[1].target, Target::Discord { .. }));
    }
}
