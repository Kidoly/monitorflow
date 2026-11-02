//! Slack, Teams and Discord channels (ADR-0077): which webhook URLs each
//! accepts, its message format and how fast it may be sent to. Event text
//! holds names and errors from machines and the AI's commands, so nothing
//! of it may reach a service as markup: Slack and Teams get it as plain
//! text objects, Discord, which has none, with every mark escaped.
//!
//! Only each service's own hosts are accepted, never an address, so a chat
//! channel cannot reach the central server's network whoever saves it
//! (ADR-0078's `public_only` holds by construction).

use std::net::IpAddr;
use std::time::Duration;

use reqwest::Url;
use serde_json::{json, Value};

use super::group::GROUP;
use super::message::{capitalized, str_at, Message};

/// Label of the link to the interface.
const OPEN: &str = "Open in MonitorFlow";

/// Longest title; Slack's header block takes 150 characters.
const TITLE: usize = 150;
/// Longest text under the title, within every service's limit.
const DETAIL: usize = 2800;
/// Longest value of a fact.
const FACT: usize = 200;
/// Longest line of a gathered message, and how many it lists.
const GATHERED_LINE: usize = 160;
/// Longest line of a group message.
pub const GROUP_LINE: usize = 150;
/// Lines per section of a Slack message, so that one stays under 3000.
const SLACK_LINES: usize = 10;
const GATHERED_LINES: usize = 15;

/// What a burst gathers under, in place of the notifications it replaces.
const GATHERED: &str = "gathered";

/// A chat service.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Service {
    Slack,
    Teams,
    Discord,
}

/// A webhook URL that passed [`Service::check_url`].
#[derive(Debug)]
pub struct CheckedUrl {
    /// As stored, and as secret as what was pasted.
    pub url: String,
    pub host: String,
}

impl Service {
    /// The service of a channel kind, `None` for email and webhooks.
    pub fn parse(kind: &str) -> Option<Self> {
        match kind {
            "slack" => Some(Self::Slack),
            "teams" => Some(Self::Teams),
            "discord" => Some(Self::Discord),
            _ => None,
        }
    }

    /// The name on screen.
    pub fn name(self) -> &'static str {
        match self {
            Self::Slack => "Slack",
            Self::Teams => "Teams",
            Self::Discord => "Discord",
        }
    }

    /// Time between two messages to one channel. Slack allows one a second
    /// per webhook; Discord says not to hard code limits, so this stays
    /// under the 30 a minute reported per channel; a Teams workflow posts
    /// through an action limited to 25 per 5 minutes.
    pub fn pace(self) -> Duration {
        match self {
            Self::Slack => Duration::from_secs(1),
            Self::Teams => Duration::from_secs(12),
            Self::Discord => Duration::from_secs(2),
        }
    }

    /// Most messages to one channel per pass of the dispatcher; more are
    /// gathered into the last, so that a burst neither holds the pass nor
    /// meets the rate limit.
    pub fn burst(self) -> usize {
        match self {
            Self::Slack => 20,
            Self::Teams => 5,
            Self::Discord => 15,
        }
    }

    /// Most lines a group message lists (ADR-0080), each within
    /// [`GROUP_LINE`]: well inside Slack's and Teams' limits, and inside
    /// Discord's 4096 characters even if every other character needs a
    /// backslash.
    pub fn group_lines(self) -> usize {
        match self {
            Self::Slack | Self::Teams => 20,
            Self::Discord => 12,
        }
    }

    /// Checks a pasted URL: https, this service's host, the default port,
    /// no credentials, the path of an incoming webhook. These hosts are the
    /// only places a chat channel can post to, so it cannot reach inside
    /// the network. Errors never repeat the URL: it may be a live token
    /// pasted in the wrong form.
    pub fn check_url(self, url: &str) -> Result<CheckedUrl, String> {
        let url = url.trim();
        let parsed = Url::parse(url).map_err(|_| self.wrong_url())?;
        let host = parsed.host_str().unwrap_or_default().to_owned();
        // Only names: an address would bypass the hosts below.
        if host.trim_matches(['[', ']']).parse::<IpAddr>().is_ok() {
            return Err(self.wrong_url());
        }
        if parsed.scheme() != "https"
            || parsed.port().is_some()
            || !parsed.username().is_empty()
            || parsed.password().is_some()
        {
            return Err(self.wrong_url());
        }
        if self == Self::Teams && retired_teams_host(&host) {
            return Err(
                "This is an old Teams connector URL, which Microsoft has retired: \
                 messages sent to it are lost. In Teams, create a workflow from the \
                 \"Send webhook alerts to a channel\" template and paste its URL."
                    .into(),
            );
        }
        let path: Vec<&str> = parsed.path_segments().into_iter().flatten().collect();
        let accepted = match self {
            Self::Slack => {
                matches!(host.as_str(), "hooks.slack.com" | "hooks.slack-gov.com")
                    && matches!(path.as_slice(), ["services", a, b, c]
                        if !a.is_empty() && !b.is_empty() && !c.is_empty())
            }
            Self::Teams => {
                TEAMS_HOSTS.iter().any(|suffix| host.ends_with(suffix))
                    && path.contains(&"workflows")
            }
            Self::Discord => DISCORD_HOSTS.contains(&host.as_str()) && discord_webhook_path(&path),
        };
        if !accepted {
            return Err(self.wrong_url());
        }
        Ok(CheckedUrl {
            url: parsed.to_string(),
            host,
        })
    }

    fn wrong_url(self) -> String {
        match self {
            Self::Slack => {
                "This is not a Slack incoming webhook URL: it starts with \
                 https://hooks.slack.com/services/."
            }
            Self::Teams => {
                "This is not a Teams workflow URL: copy the whole HTTP URL from the \
                 workflow's trigger. It ends in powerplatform.com or logic.azure.com \
                 before the first slash."
            }
            Self::Discord => {
                "This is not a Discord webhook URL: it starts with \
                 https://discord.com/api/webhooks/. Use Copy Webhook URL and change nothing."
            }
        }
        .to_owned()
    }

    /// The URL to post to. Discord only reports a message it failed to
    /// save when asked to wait for it.
    pub fn request_url(self, url: &str) -> Option<Url> {
        let mut url = Url::parse(url).ok()?;
        if self == Self::Discord {
            url.query_pairs_mut().append_pair("wait", "true");
        }
        Some(url)
    }

    /// The JSON body for `message`.
    pub fn payload(self, message: &Message) -> Value {
        let card = Card::new(message);
        match self {
            Self::Slack => slack(&card),
            Self::Teams => teams(&card),
            Self::Discord => discord(&card),
        }
    }

    /// A refused message in plain words, when the answer is one to explain.
    pub fn explain(self, status: u16, answer: &str) -> Option<&'static str> {
        match (self, status) {
            (Self::Slack, _) => match answer.trim() {
                "no_service" | "no_service_id" | "no_team" | "no_active_hooks"
                | "invalid_token" | "team_disabled" => Some(
                    "Slack no longer knows this webhook: it was removed, or its app was. \
                     Create a new one and paste its URL.",
                ),
                "channel_is_archived" => {
                    Some("The Slack channel of this webhook is archived. Unarchive it or create a new webhook.")
                }
                "channel_not_found" => {
                    Some("The Slack channel of this webhook no longer exists. Create a new webhook.")
                }
                "posting_to_general_channel_denied" | "action_prohibited" => {
                    Some("Slack does not allow this webhook to post in its channel.")
                }
                _ => None,
            },
            (Self::Discord, 401 | 403 | 404) => Some(
                "Discord no longer knows this webhook: it was deleted, or its URL changed. \
                 Copy its URL again from the channel's Integrations settings.",
            ),
            (Self::Teams, 401 | 403 | 404) => Some(
                "Teams refused this URL: the workflow was deleted or its URL changed, or it \
                 does not accept requests from anyone. Check the workflow's trigger.",
            ),
            _ => None,
        }
    }
}

/// Hosts of the HTTP trigger of a workflow, per Microsoft cloud.
const TEAMS_HOSTS: [&str; 8] = [
    ".api.powerplatform.com",
    ".logic.azure.com",
    ".api.gov.powerplatform.microsoft.us",
    ".api.high.powerplatform.microsoft.us",
    ".api.appsplatform.us",
    ".logic.azure.us",
    ".api.powerplatform.partner.microsoftonline.cn",
    ".logic.azure.cn",
];

const DISCORD_HOSTS: [&str; 6] = [
    "discord.com",
    "ptb.discord.com",
    "canary.discord.com",
    "discordapp.com",
    "ptb.discordapp.com",
    "canary.discordapp.com",
];

/// Office 365 connectors answer 200 to anything since their retirement, so
/// a test against one would pass and every message would be lost.
fn retired_teams_host(host: &str) -> bool {
    host == "webhook.office.com"
        || host.ends_with(".webhook.office.com")
        || host == "outlook.office.com"
        || host == "outlook.office365.com"
}

/// `/api[/vN]/webhooks/{id}/{token}`; a further `/slack` or `/github` would
/// expect another format.
fn discord_webhook_path(path: &[&str]) -> bool {
    let rest = match path {
        ["api", version, rest @ ..]
            if version.len() > 1
                && version.starts_with('v')
                && version[1..].bytes().all(|b| b.is_ascii_digit()) =>
        {
            rest
        }
        ["api", rest @ ..] => rest,
        _ => return false,
    };
    matches!(rest, ["webhooks", id, token]
        if !id.is_empty() && id.bytes().all(|b| b.is_ascii_digit()) && !token.is_empty())
}

/// Keeps `keep` messages and gathers the rest into one, so that at most
/// `keep + 1` go out. Nothing is lost that delivery would have kept: the
/// gathered message counts them all and lists the first few.
pub fn gather(messages: &[Message], keep: usize, web_url: &str) -> Vec<Message> {
    let (first, rest) = messages.split_at(keep.min(messages.len()));
    let mut gathered = first.to_vec();
    if rest.is_empty() {
        return gathered;
    }
    let mut lines: Vec<String> = rest
        .iter()
        .take(GATHERED_LINES)
        .map(|m| cut(&one_line(&m.text), GATHERED_LINE))
        .collect();
    if rest.len() > GATHERED_LINES {
        lines.push(format!("and {} more", rest.len() - GATHERED_LINES));
    }
    let text = format!("{} more notifications", rest.len());
    gathered.push(Message {
        subject: format!("[MonitorFlow] {text}"),
        text,
        url: format!("{web_url}/incidents/"),
        body: lines.join("\n"),
        payload: json!({ "event": GATHERED }),
        lines,
    });
    gathered
}

/// How serious a message looks, where a service has colors.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Tone {
    Danger,
    Warning,
    Good,
    Neutral,
}

/// What every service shows, as plain text.
#[derive(Debug)]
struct Card {
    title: String,
    /// Lines under the title: the whole text when the title cut it, then
    /// what a group or a gathered message lists.
    detail: Vec<String>,
    facts: Vec<(&'static str, String)>,
    link: String,
    tone: Tone,
    /// The whole text in one line, for notifications.
    summary: String,
}

impl Card {
    fn new(message: &Message) -> Self {
        let p = &message.payload;
        let event = str_at(p, &["event"]).unwrap_or_default();
        let severity = str_at(p, &["incident", "severity"]);
        let summary = one_line(&message.text);
        let title = cut(&summary, TITLE);
        let mut detail = if title == summary {
            Vec::new()
        } else {
            vec![cut(&summary, DETAIL)]
        };
        detail.extend(message.lines.iter().map(|l| cut(&one_line(l), GROUP_LINE)));
        let resolved = event == "incident.resolved"
            || (event == GROUP && p["group"]["resolved"].as_bool() == Some(true));
        let tone = match (event, severity) {
            _ if resolved => Tone::Good,
            (GROUP, Some("critical")) => Tone::Danger,
            (GROUP, _) => Tone::Warning,
            ("incident.opened" | "incident.escalated", Some("critical")) => Tone::Danger,
            ("incident.opened" | "incident.escalated", _) | ("approval.waiting", _) => {
                Tone::Warning
            }
            ("update.failed" | "backup.failed", _) => Tone::Danger,
            _ => Tone::Neutral,
        };
        let severity = match event {
            _ if resolved => Some("Resolved".to_owned()),
            e if e.starts_with("incident.") => severity.map(capitalized),
            _ => None,
        };
        let customer = str_at(p, &["customer", "name"]).filter(|&c| c != "default");
        let facts = [
            ("Severity", severity.as_deref()),
            ("Customer", customer),
            ("Site", str_at(p, &["site", "name"])),
            ("Server", str_at(p, &["server", "name"])),
            ("Relay", str_at(p, &["relay", "name"])),
            ("Affected", str_at(p, &["group", "hosts"])),
        ]
        .into_iter()
        .filter_map(|(label, value)| Some((label, cut(&one_line(value?), FACT))))
        .filter(|(_, value)| !value.is_empty())
        .collect();
        Self {
            title,
            detail,
            facts,
            link: message.url.clone(),
            tone,
            summary,
        }
    }
}

/// `text` without control characters, in one line.
fn one_line(text: &str) -> String {
    let text: String = text
        .chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect();
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Length as the services may count it: never less than in characters.
fn length(text: &str) -> usize {
    text.encode_utf16().count()
}

/// `text` within `limit`, marked when cut.
fn cut(text: &str, limit: usize) -> String {
    within(text, limit, |c, out| out.push(c))
}

/// `text` with each character written by `write`, within `limit` once
/// written. It cuts between characters, so an escape stays whole.
fn within(text: &str, limit: usize, write: impl Fn(char, &mut String)) -> String {
    let mut whole = String::new();
    text.chars().for_each(|c| write(c, &mut whole));
    if length(&whole) <= limit {
        return whole;
    }
    let mut out = String::new();
    let mut used = 0;
    let mut piece = String::new();
    for c in text.chars() {
        piece.clear();
        write(c, &mut piece);
        used += length(&piece);
        if used >= limit {
            break;
        }
        out.push_str(&piece);
    }
    out.truncate(out.trim_end().len());
    out.push('…');
    out
}

/// Slack's control characters, which also make mentions (`<!here>`) and
/// links.
fn slack_char(c: char, out: &mut String) {
    match c {
        '&' => out.push_str("&amp;"),
        '<' => out.push_str("&lt;"),
        '>' => out.push_str("&gt;"),
        c => out.push(c),
    }
}

/// `lines` in sections of at most [`SLACK_LINES`] lines and 2800
/// characters once escaped, so that a list is never cut at Slack's 3000.
fn sections(lines: &[String]) -> Vec<String> {
    let escaped = |text: &str| length(&within(text, usize::MAX, slack_char));
    let mut sections: Vec<(usize, String)> = Vec::new();
    for line in lines {
        match sections.last_mut() {
            Some((count, text))
                if *count < SLACK_LINES && escaped(text) + escaped(line) < DETAIL =>
            {
                *count += 1;
                text.push('\n');
                text.push_str(line);
            }
            _ => sections.push((1, line.clone())),
        }
    }
    sections.into_iter().map(|(_, text)| text).collect()
}

fn slack_plain(text: &str, limit: usize) -> Value {
    json!({ "type": "plain_text", "text": within(text, limit, slack_char), "emoji": false })
}

/// Slack: Block Kit with every text from the event in `plain_text`, which
/// Slack neither formats nor scans for mentions. `text` is the notification
/// fallback, with formatting off.
fn slack(card: &Card) -> Value {
    let mut blocks = vec![json!({ "type": "header", "text": slack_plain(&card.title, TITLE) })];
    for text in sections(&card.detail) {
        blocks.push(json!({ "type": "section", "text": slack_plain(&text, 3000) }));
    }
    if !card.facts.is_empty() {
        let fields: Vec<Value> = card
            .facts
            .iter()
            .map(|(label, value)| slack_plain(&format!("{label}: {value}"), 2000))
            .collect();
        blocks.push(json!({ "type": "section", "fields": fields }));
    }
    let link = within(&card.link, 2900, slack_char);
    blocks.push(json!({
        "type": "section",
        "text": { "type": "mrkdwn", "text": format!("<{link}|{OPEN}>"), "verbatim": true },
    }));
    json!({
        "text": within(&card.summary, 3000, slack_char),
        "parse": "none",
        "mrkdwn": false,
        "unfurl_links": false,
        "unfurl_media": false,
        "blocks": blocks,
    })
}

/// `{{DATE(…)}}` is a function in card text: a zero-width space keeps it
/// text.
fn teams_text(text: &str) -> String {
    text.replace("{{", "{\u{200b}{")
}

fn teams_run(text: &str) -> Value {
    json!({ "type": "TextRun", "text": teams_text(text) })
}

/// Teams: an Adaptive Card, the format of Workflows webhooks. Event text
/// goes in `RichTextBlock`s because `TextBlock` and `FactSet` render
/// Markdown with no documented escape, and text runs do not.
fn teams(card: &Card) -> Value {
    let color = match card.tone {
        Tone::Danger => "Attention",
        Tone::Warning => "Warning",
        Tone::Good => "Good",
        Tone::Neutral => "Default",
    };
    let mut title = teams_run(&card.title);
    title["weight"] = "Bolder".into();
    title["size"] = "Medium".into();
    title["color"] = color.into();
    let mut body = vec![json!({ "type": "RichTextBlock", "inlines": [title] })];
    for line in &card.detail {
        body.push(json!({ "type": "RichTextBlock", "inlines": [teams_run(line)] }));
    }
    for (label, value) in &card.facts {
        let mut name = teams_run(&format!("{label}: "));
        name["weight"] = "Bolder".into();
        body.push(json!({
            "type": "RichTextBlock",
            "spacing": "None",
            "inlines": [name, teams_run(value)],
        }));
    }
    json!({
        "type": "message",
        "attachments": [{
            "contentType": "application/vnd.microsoft.card.adaptive",
            "contentUrl": null,
            "content": {
                "$schema": "http://adaptivecards.io/schemas/adaptive-card.json",
                "type": "AdaptiveCard",
                "version": "1.2",
                "msteams": { "width": "Full" },
                "body": body,
                "actions": [{ "type": "Action.OpenUrl", "title": OPEN, "url": card.link }],
            },
        }],
    })
}

/// Discord has no plain text: a backslash keeps a mark literal. `:` stops
/// links from being recognized, `@` and `<` mentions, the rest formatting.
fn discord_char(c: char, out: &mut String) {
    if c.is_ascii_punctuation() {
        out.push('\\');
    }
    out.push(c);
}

/// Discord: an embed, with mentions off. The link goes in the description
/// rather than the embed's `url`: Discord rejects a whole message over a
/// `url` it thinks malformed, and the interface's address is often private.
fn discord(card: &Card) -> Value {
    let color = match card.tone {
        Tone::Danger => 0x00d1_3438,
        Tone::Warning => 0x00f2_a900,
        Tone::Good => 0x002e_a043,
        Tone::Neutral => 0x003b_82f6,
    };
    let link = card.link.replace('(', "%28").replace(')', "%29");
    let link = format!("[{OPEN}]({link})");
    // Each line gets its share once escaped, so that a list keeps its
    // last line, which says how many more there are.
    let share = 3000 / card.detail.len().max(1);
    let description = if card.detail.is_empty() {
        link
    } else {
        let lines: Vec<String> = card
            .detail
            .iter()
            .map(|line| within(line, share, discord_char))
            .collect();
        format!("{}\n\n{link}", lines.join("\n"))
    };
    let fields: Vec<Value> = card
        .facts
        .iter()
        .map(|(label, value)| {
            json!({ "name": label, "value": within(value, 400, discord_char), "inline": true })
        })
        .collect();
    json!({
        "username": "MonitorFlow",
        "allowed_mentions": { "parse": [] },
        "embeds": [{
            "title": within(&card.title, 256, discord_char),
            "description": description,
            "color": color,
            "fields": fields,
        }],
    })
}

#[cfg(test)]
mod tests;
