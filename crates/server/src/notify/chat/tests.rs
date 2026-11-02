use chrono::Utc;
use serde_json::json;

use super::*;
use crate::notify::message::{render, Event};

const WEB: &str = "https://10.1.10.16";
const SLACK: &str = "https://hooks.slack.com/services/T0/B0/XXXX";
const DISCORD: &str = "https://discord.com/api/webhooks/123/tok-en_1";
const TEAMS: &str = "https://abc.de.environment.api.powerplatform.com:443/powerautomate/automations/direct/workflows/f00/triggers/manual/paths/invoke?api-version=1&sp=%2Ftriggers%2Fmanual%2Frun&sv=1.0&sig=SIGNATURE";

/// A server name that tries every trick each service knows.
const HOSTILE: &str = "<!here> @everyone <@123> *b* _i_ ~s~ `c` [x](https://evil.example) **t** {{DATE(2026)}} &amp; | > # :smile:";

fn event(kind: &str, details: Value) -> Message {
    render(
        &Event {
            id: 42,
            event: kind.into(),
            customer_id: None,
            site_id: None,
            details,
            created_at: Utc::now(),
        },
        WEB,
    )
}

fn incident(kind: &str, server: &str, severity: &str) -> Message {
    event(
        kind,
        json!({
            "customer": {"id": "c", "name": "acme"},
            "site": {"id": "s", "name": "paris"},
            "server": {"id": "v", "name": server},
            "relay": null,
            "incident": {"id": "0f5e", "metric": "monitorflow_cpu_usage_percent",
                         "instance": "", "severity": severity, "value": 97.5},
        }),
    )
}

fn plain_texts(value: &Value, out: &mut Vec<String>) {
    match value {
        Value::Object(map) => {
            if map.get("type") == Some(&json!("plain_text")) {
                out.push(map["text"].as_str().unwrap().to_owned());
            }
            map.values().for_each(|v| plain_texts(v, out));
        }
        Value::Array(items) => items.iter().for_each(|v| plain_texts(v, out)),
        _ => {}
    }
}

#[test]
fn slack_gets_blocks_with_title_severity_server_and_link() {
    let message = incident("incident.opened", "web-1", "critical");
    let payload = Service::Slack.payload(&message);
    assert_eq!(
        payload["text"],
        "Critical: cpu_usage_percent is 97.5 on web-1 (acme / paris)"
    );
    let blocks = payload["blocks"].as_array().unwrap();
    assert_eq!(blocks[0]["type"], "header");
    assert_eq!(blocks[0]["text"]["type"], "plain_text");
    assert_eq!(blocks[0]["text"]["text"], payload["text"]);
    let fields: Vec<&str> = blocks[1]["fields"]
        .as_array()
        .unwrap()
        .iter()
        .map(|f| f["text"].as_str().unwrap())
        .collect();
    assert_eq!(
        fields,
        [
            "Severity: Critical",
            "Customer: acme",
            "Site: paris",
            "Server: web-1"
        ]
    );
    assert_eq!(
        blocks[2]["text"]["text"],
        "<https://10.1.10.16/incidents/view/?id=0f5e|Open in MonitorFlow>"
    );
}

#[test]
fn slack_text_cannot_mention_or_link() {
    let payload = Service::Slack.payload(&incident("incident.opened", HOSTILE, "warning"));
    let mut texts = vec![payload["text"].as_str().unwrap().to_owned()];
    plain_texts(&payload, &mut texts);
    assert!(texts.len() > 3);
    for text in texts {
        assert!(!text.contains('<') && !text.contains('>'), "{text}");
    }
    assert!(payload["blocks"][0]["text"]["text"]
        .as_str()
        .unwrap()
        .contains("&lt;!here&gt;"));
    assert_eq!(payload["parse"], "none");
    assert_eq!(payload["mrkdwn"], false);
    // The only mrkdwn in the message is the link, built here.
    let mrkdwn = payload["blocks"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|b| b["text"]["type"] == "mrkdwn")
        .count();
    assert_eq!(mrkdwn, 1);
}

#[test]
fn slack_escapes_the_ampersand_of_a_link() {
    let message = event(
        "update.failed",
        json!({"server": {"id": "v", "name": "db-1"}, "update": {"kind": "apply_updates"}}),
    );
    let payload = Service::Slack.payload(&message);
    let blocks = payload["blocks"].as_array().unwrap();
    let link = blocks.last().unwrap()["text"]["text"].as_str().unwrap();
    assert_eq!(
        link,
        "<https://10.1.10.16/servers/view/?id=v&amp;tab=updates|Open in MonitorFlow>"
    );
}

#[test]
fn slack_stays_within_its_block_limits() {
    let long = "<&>".repeat(5000);
    let message = event("backup.failed", json!({ "backup": { "error": long } }));
    let payload = Service::Slack.payload(&message);
    let blocks = payload["blocks"].as_array().unwrap();
    assert!(length(blocks[0]["text"]["text"].as_str().unwrap()) <= 150);
    assert!(length(blocks[1]["text"]["text"].as_str().unwrap()) <= 3000);
    let text = payload["text"].as_str().unwrap();
    assert!(length(text) <= 3000);
    // A cut never leaves half an entity.
    assert!(text.trim_end_matches('…').ends_with(';'), "{text}");
}

#[test]
fn teams_gets_an_adaptive_card_with_the_required_envelope() {
    let payload = Service::Teams.payload(&incident("incident.opened", "web-1", "critical"));
    assert_eq!(payload["type"], "message");
    let attachment = &payload["attachments"][0];
    assert_eq!(
        attachment["contentType"],
        "application/vnd.microsoft.card.adaptive"
    );
    assert!(attachment["contentUrl"].is_null());
    assert!(attachment.as_object().unwrap().contains_key("contentUrl"));
    let card = &attachment["content"];
    assert_eq!(card["type"], "AdaptiveCard");
    assert_eq!(card["version"], "1.2");
    assert_eq!(card["msteams"]["width"], "Full");
    let title = &card["body"][0]["inlines"][0];
    assert_eq!(title["color"], "Attention");
    assert_eq!(
        title["text"],
        "Critical: cpu_usage_percent is 97.5 on web-1 (acme / paris)"
    );
    let action = &card["actions"][0];
    assert_eq!(action["type"], "Action.OpenUrl");
    assert_eq!(action["url"], "https://10.1.10.16/incidents/view/?id=0f5e");
    let server = &card["body"][4]["inlines"];
    assert_eq!(server[0]["text"], "Server: ");
    assert_eq!(server[1]["text"], "web-1");
}

#[test]
fn teams_text_is_never_where_markdown_renders() {
    let payload = Service::Teams.payload(&incident("incident.opened", HOSTILE, "warning"));
    let card = &payload["attachments"][0]["content"];
    for block in card["body"].as_array().unwrap() {
        assert_eq!(block["type"], "RichTextBlock", "{block}");
        for run in block["inlines"].as_array().unwrap() {
            assert_eq!(run["type"], "TextRun");
            assert!(!run["text"].as_str().unwrap().contains("{{"), "{run}");
        }
    }
    assert!(card.get("entities").is_none() && card["msteams"].get("entities").is_none());
}

#[test]
fn teams_stays_well_under_its_size_limit() {
    let wide = "😀\"".repeat(20_000);
    let details = json!({
        "customer": {"name": wide}, "site": {"name": wide},
        "server": {"name": wide}, "relay": {"name": wide},
        "backup": {"error": wide},
    });
    let body = serde_json::to_vec(&Service::Teams.payload(&event("backup.failed", details)));
    assert!(body.unwrap().len() < 24_000);
}

#[test]
fn discord_gets_an_embed_with_mentions_off() {
    let payload = Service::Discord.payload(&incident("incident.resolved", "web-1", "critical"));
    assert_eq!(payload["allowed_mentions"], json!({ "parse": [] }));
    assert!(payload.get("content").is_none());
    let embed = &payload["embeds"][0];
    assert_eq!(embed["color"], 0x002e_a043);
    assert_eq!(
        embed["title"],
        "Resolved\\: cpu\\_usage\\_percent on web\\-1 \\(acme \\/ paris\\)"
    );
    assert_eq!(
        embed["description"],
        "[Open in MonitorFlow](https://10.1.10.16/incidents/view/?id=0f5e)"
    );
    assert!(embed.get("url").is_none());
    let fields = embed["fields"].as_array().unwrap();
    assert_eq!(
        fields[0],
        json!({"name": "Severity", "value": "Resolved", "inline": true})
    );
    assert_eq!(fields[3]["value"], "web\\-1");
}

#[test]
fn discord_marks_are_all_escaped() {
    let payload = Service::Discord.payload(&incident("incident.opened", HOSTILE, "warning"));
    let embed = &payload["embeds"][0];
    let server = embed["fields"].as_array().unwrap().last().unwrap()["value"]
        .as_str()
        .unwrap();
    for mark in [
        "\\<\\!here\\>",
        "\\@everyone",
        "\\<\\@123\\>",
        "\\*b\\*",
        "\\_i\\_",
    ] {
        assert!(server.contains(mark), "{mark} in {server}");
    }
    assert!(
        server.contains("\\[x\\]\\(https\\:\\/\\/evil\\.example\\)"),
        "{server}"
    );
    // No mark is left without its backslash.
    let bare = server
        .char_indices()
        .any(|(i, c)| c.is_ascii_punctuation() && !escaped_at(server, i));
    assert!(!bare, "{server}");
}

/// Whether the character at `index` follows an odd run of backslashes, or
/// is itself one escaping the next.
fn escaped_at(text: &str, index: usize) -> bool {
    let before = text[..index]
        .bytes()
        .rev()
        .take_while(|&b| b == b'\\')
        .count();
    before % 2 == 1 || text.as_bytes()[index] == b'\\'
}

#[test]
fn discord_stays_within_its_embed_limits() {
    let long = "*".repeat(6000);
    let details = json!({
        "customer": {"name": long}, "site": {"name": long},
        "server": {"name": long}, "relay": {"name": long},
        "backup": {"error": long},
    });
    let payload = Service::Discord.payload(&event("backup.failed", details));
    let embed = &payload["embeds"][0];
    let title = embed["title"].as_str().unwrap();
    let description = embed["description"].as_str().unwrap();
    assert!(length(title) <= 256);
    assert!(length(description) <= 4096);
    let mut total = length(title) + length(description);
    for field in embed["fields"].as_array().unwrap() {
        let value = field["value"].as_str().unwrap();
        assert!(length(value) <= 1024);
        total += length(value) + length(field["name"].as_str().unwrap());
    }
    assert!(total <= 6000, "{total}");
    // A cut never strands a backslash that would escape the ellipsis.
    assert!(title.ends_with("\\*…"), "{title}");
}

#[test]
fn every_event_renders_for_every_service() {
    let server = json!({"id": "v", "name": "db-1"});
    let messages = [
        event("test", json!({})),
        incident("incident.escalated", "web-1", "critical"),
        event(
            "approval.waiting",
            json!({"server": server, "approval": {"command": "rm -rf /", "tier": "high_impact"}}),
        ),
        event(
            "update.failed",
            json!({"update": {"kind": "campaign", "campaign_id": "c", "name": "May"}}),
        ),
        event("backup.failed", json!({"backup": {"error": "disk full"}})),
        event(
            "update.available",
            json!({"release": {"version": "0.3.0", "running_version": "0.2.0"}}),
        ),
    ];
    for message in &messages {
        for service in [Service::Slack, Service::Teams, Service::Discord] {
            let body = service.payload(message).to_string();
            assert!(body.contains(WEB), "{body}");
        }
    }
    let test = Service::Discord.payload(&messages[0]);
    assert!(test["embeds"][0]["fields"].as_array().unwrap().is_empty());
    let approval = Service::Slack.payload(&messages[2]);
    assert!(approval["text"].as_str().unwrap().contains("rm -rf /"));
}

#[test]
fn multi_line_text_becomes_one_line() {
    assert_eq!(one_line("a\n  b\tc\u{7}d\r\n"), "a b c d");
}

#[test]
fn a_burst_gathers_into_one_message() {
    let messages: Vec<Message> = (0..30)
        .map(|i| incident("incident.opened", &format!("web-{i}"), "warning"))
        .collect();
    let few = gather(&messages[..3], 4, WEB);
    assert_eq!(few, messages[..3]);
    let gathered = gather(&messages, 4, WEB);
    assert_eq!(gathered.len(), 5);
    assert_eq!(gathered[..4], messages[..4]);
    let last = &gathered[4];
    assert_eq!(last.text, "26 more notifications");
    assert_eq!(last.url, "https://10.1.10.16/incidents/");
    assert_eq!(last.body.lines().count(), GATHERED_LINES + 1);
    assert!(last.body.ends_with("and 11 more"));
    assert!(last.body.contains("web-4 "));
    for service in [Service::Slack, Service::Teams, Service::Discord] {
        let body = service.payload(last).to_string();
        assert!(body.contains("26 more notifications"), "{body}");
        assert!(body.contains("web"), "{body}");
    }
}

#[test]
fn slack_urls_are_checked() {
    let checked = Service::Slack.check_url(&format!(" {SLACK} ")).unwrap();
    assert_eq!(checked.url, SLACK);
    assert_eq!(checked.host, "hooks.slack.com");
    assert!(Service::Slack
        .check_url("https://hooks.slack-gov.com/services/T/B/X")
        .is_ok());
    for url in [
        "http://hooks.slack.com/services/T0/B0/XXXX",
        "https://hooks.slack.com:8443/services/T0/B0/XXXX",
        "https://user@hooks.slack.com/services/T0/B0/XXXX",
        "https://hooks.slack.com/workflows/T0/A0/1/XXXX",
        "https://hooks.slack.com/services/T0",
        "https://hooks.slack.com.evil.example/services/T0/B0/XXXX",
        "https://evil.example/hooks.slack.com/services/T0/B0/XXXX",
        "https://hooks.slack.com@evil.example/services/T0/B0/XXXX",
        "https://127.0.0.1/services/T0/B0/XXXX",
        "https://[::1]/services/T0/B0/XXXX",
        "https://169.254.169.254/services/T0/B0/XXXX",
        "https://2130706433/services/T0/B0/XXXX",
        DISCORD,
        "not a url",
    ] {
        let error = Service::Slack.check_url(url).unwrap_err();
        assert!(error.contains("hooks.slack.com/services"), "{url}: {error}");
    }
}

#[test]
fn discord_urls_are_checked() {
    for url in [
        DISCORD,
        "https://discord.com/api/v10/webhooks/123/token",
        "https://discordapp.com/api/webhooks/123/token",
        "https://canary.discord.com/api/webhooks/123/token",
        "https://ptb.discord.com/api/webhooks/123/token?thread_id=9",
    ] {
        assert!(Service::Discord.check_url(url).is_ok(), "{url}");
    }
    for url in [
        "http://discord.com/api/webhooks/123/token",
        "https://discord.com/api/webhooks/123/token/slack",
        "https://discord.com/api/webhooks/123/token/github",
        "https://discord.com/api/webhooks/123",
        "https://discord.com/api/webhooks/abc/token",
        "https://discord.com/channels/1/2",
        "https://discord.gg/api/webhooks/123/token",
        "https://discord.com.evil.example/api/webhooks/123/token",
        "https://162.159.128.233/api/webhooks/123/token",
        SLACK,
    ] {
        assert!(Service::Discord.check_url(url).is_err(), "{url}");
    }
}

#[test]
fn discord_is_asked_to_report_failures() {
    let url = Service::Discord.request_url(DISCORD).unwrap();
    assert_eq!(url.query(), Some("wait=true"));
    let threaded = "https://discord.com/api/webhooks/1/t?thread_id=9";
    let url = Service::Discord.request_url(threaded).unwrap();
    assert_eq!(url.query(), Some("thread_id=9&wait=true"));
    let url = Service::Slack.request_url(SLACK).unwrap();
    assert_eq!(url.as_str(), SLACK);
}

#[test]
fn teams_urls_are_checked() {
    let checked = Service::Teams.check_url(TEAMS).unwrap();
    assert!(checked.url.contains("sig=SIGNATURE"));
    assert!(checked.url.contains("sp=%2Ftriggers%2Fmanual%2Frun"));
    assert_eq!(checked.host, "abc.de.environment.api.powerplatform.com");
    for url in [
        "https://prod-12.westus.logic.azure.com:443/workflows/a/triggers/manual/paths/invoke?sig=x",
        "https://abc.environment.api.powerplatform.com/powerautomate/automations/direct/cu/20/workflows/a/triggers/manual/paths/invoke?sig=x",
        "https://a.api.gov.powerplatform.microsoft.us/powerautomate/automations/direct/workflows/a",
        "https://prod.usgovtexas.logic.azure.us/workflows/a/triggers/manual/paths/invoke?sig=x",
    ] {
        assert!(Service::Teams.check_url(url).is_ok(), "{url}");
    }
    for url in [
        "https://logic.azure.com/workflows/a",
        "https://evil.example/workflows/a?x=.logic.azure.com",
        "https://a.logic.azure.com.evil.example/workflows/a",
        "https://notlogic.azure.com/workflows/a",
        "https://a.logic.azure.com/other",
        "https://10.0.0.1/workflows/a",
        "https://[fd00::1]/workflows/a",
        SLACK,
    ] {
        let error = Service::Teams.check_url(url).unwrap_err();
        assert!(error.contains("Teams workflow URL"), "{url}: {error}");
    }
}

#[test]
fn retired_teams_connectors_are_refused_by_name() {
    for url in [
        "https://contoso.webhook.office.com/webhookb2/a@b/IncomingWebhook/c/d",
        "https://outlook.office.com/webhook/a@b/IncomingWebhook/c/d",
        "https://outlook.office365.com/webhook/a/IncomingWebhook/c/d",
    ] {
        let error = Service::Teams.check_url(url).unwrap_err();
        assert!(error.contains("retired"), "{url}: {error}");
        assert!(error.contains("Send webhook alerts to a channel"));
    }
}

#[test]
fn errors_never_repeat_the_url() {
    let token = "sEcReTtOkEn";
    for service in [Service::Slack, Service::Teams, Service::Discord] {
        for url in [
            format!("http://hooks.slack.com/services/T/B/{token}"),
            format!("https://evil.example/{token}"),
            format!("nonsense {token}"),
        ] {
            let error = service.check_url(&url).unwrap_err();
            assert!(!error.contains(token), "{error}");
        }
    }
}

#[test]
fn the_pace_keeps_within_each_service() {
    assert!(Service::Slack.pace() >= Duration::from_secs(1));
    // 25 posts in 5 minutes.
    assert!(Service::Teams.pace().as_secs() * 25 >= 300);
    for service in [Service::Slack, Service::Teams, Service::Discord] {
        assert!(service.burst() >= 2);
        assert!((service.pace() * service.burst() as u32).as_secs() <= 60);
    }
}
