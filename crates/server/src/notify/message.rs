//! What a notification says: one line of text, a link to the page that
//! shows it, an email body, and the webhook payload (version 1, documented
//! in ADR-0060).

use chrono::{DateTime, Utc};
use serde_json::{json, Map, Value};
use uuid::Uuid;

/// The page of notification settings, under the web interface's URL.
pub const SETTINGS_PATH: &str = "/admin/notifications/";

/// Version of the webhook payload; fields are only ever added.
pub const PAYLOAD_VERSION: u32 = 1;

/// A row of `notification_outbox`, or a test.
#[derive(Debug, Clone, PartialEq, sqlx::FromRow)]
pub struct Event {
    pub id: i64,
    /// `incident.opened`, `incident.escalated`, `incident.resolved`,
    /// `approval.waiting`, `update.failed`, `backup.failed`,
    /// `update.available`, or `test`.
    pub event: String,
    pub customer_id: Option<Uuid>,
    /// The objects of the event, written by the triggers.
    pub details: Value,
    pub created_at: DateTime<Utc>,
}

impl Event {
    /// What "Send test" delivers.
    pub fn test() -> Self {
        let now = Utc::now();
        Self {
            id: 0,
            event: "test".into(),
            customer_id: None,
            details: json!({ "occurred_at": now }),
            created_at: now,
        }
    }
}

/// An event rendered for delivery.
#[derive(Debug, Clone, PartialEq)]
pub struct Message {
    /// Email subject.
    pub subject: String,
    /// The event in one line; also the webhook's `text`.
    pub text: String,
    /// The page that shows it.
    pub url: String,
    /// Plain-text email body.
    pub body: String,
    /// The webhook's JSON body.
    pub payload: Value,
}

fn str_at<'a>(value: &'a Value, path: &[&str]) -> Option<&'a str> {
    path.iter()
        .try_fold(value, |v, key| v.get(key))
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
}

/// A number without trailing zeros, at most two decimals.
fn number(value: f64) -> String {
    let text = format!("{value:.2}");
    text.trim_end_matches('0').trim_end_matches('.').to_owned()
}

/// "web-1 (acme / paris)": the server or relay, and where it is; the
/// customer only when not the default one.
fn place(details: &Value) -> String {
    let peer = str_at(details, &["server", "name"])
        .map(str::to_owned)
        .or_else(|| str_at(details, &["relay", "name"]).map(|r| format!("relay {r}")));
    let site = str_at(details, &["site", "name"]);
    let customer = str_at(details, &["customer", "name"]).filter(|&c| c != "default");
    let at = match (customer, site) {
        (Some(c), Some(s)) => Some(format!("{c} / {s}")),
        (c, s) => c.or(s).map(str::to_owned),
    };
    match (peer, at) {
        (Some(p), Some(a)) => format!("{p} ({a})"),
        (Some(p), None) => p,
        (None, Some(a)) => a,
        (None, None) => "the server".into(),
    }
}

/// The sensor of an incident, like `disk_used_percent /var`.
fn sensor(incident: &Value) -> String {
    let metric = str_at(incident, &["metric"]).unwrap_or("a sensor");
    let metric = metric.strip_prefix("monitorflow_").unwrap_or(metric);
    match str_at(incident, &["instance"]) {
        Some(instance) => format!("{metric} {instance}"),
        None => metric.to_owned(),
    }
}

fn capitalized(word: &str) -> String {
    let mut chars = word.chars();
    chars.next().map_or_else(String::new, |c| {
        c.to_uppercase().chain(chars).collect::<String>()
    })
}

fn incident_text(event: &str, details: &Value) -> String {
    let incident = details.get("incident").unwrap_or(&Value::Null);
    let place = place(details);
    let severity = str_at(incident, &["severity"]).unwrap_or("warning");
    let value = incident.get("value").and_then(Value::as_f64).map(number);
    // Removing a peer closes its incidents of any metric (ADR-0062).
    if event == "incident.resolved" && str_at(incident, &["reason"]) == Some("revoked") {
        return format!("Resolved: {place} was removed");
    }
    // Offline incidents (ADR-0057's 'offline' metric) read as a state.
    if str_at(incident, &["metric"]) == Some("offline") {
        return match event {
            "incident.resolved" => match str_at(incident, &["reason"]) {
                Some("muted") => format!("Resolved: offline alerts for {place} were muted"),
                _ => format!("Resolved: {place} is back online"),
            },
            _ => format!("{}: {place} is offline", capitalized(severity)),
        };
    }
    let sensor = sensor(incident);
    match (event, value) {
        ("incident.resolved", _) => format!("Resolved: {sensor} on {place}"),
        ("incident.escalated", Some(v)) => {
            format!("Escalated to {severity}: {sensor} is {v} on {place}")
        }
        ("incident.escalated", None) => format!("Escalated to {severity}: {sensor} on {place}"),
        (_, Some(v)) => format!("{}: {sensor} is {v} on {place}", capitalized(severity)),
        (_, None) => format!("{}: {sensor} on {place}", capitalized(severity)),
    }
}

fn update_text(details: &Value) -> String {
    let update = details.get("update").unwrap_or(&Value::Null);
    let error = str_at(update, &["error"]);
    let because = |text: String| match error {
        Some(e) => format!("{text}: {e}"),
        None => text,
    };
    let version = str_at(update, &["version"]);
    match str_at(update, &["kind"]) {
        Some("campaign") => because(format!(
            "Update campaign \"{}\" failed",
            str_at(update, &["name"]).unwrap_or("unnamed")
        )),
        Some("automatic_rollout") => because(format!(
            "Automatic {} updates to {} halted",
            str_at(update, &["peers"]).unwrap_or("agent"),
            version.unwrap_or("a new release")
        )),
        Some("self_update") if details.get("relay").is_some_and(|r| !r.is_null()) => {
            because(format!(
                "Update of {} to {} failed",
                place(details),
                version.unwrap_or("a new release")
            ))
        }
        Some("self_update") => because(format!("Agent update failed on {}", place(details))),
        _ => because(format!("Package updates failed on {}", place(details))),
    }
}

/// The page that shows the event, under `web_url`.
fn path(event: &Event) -> String {
    let d = &event.details;
    let id = |path: &[&str]| str_at(d, path).unwrap_or_default().to_owned();
    match event.event.as_str() {
        e if e.starts_with("incident.") => {
            format!("/incidents/view/?id={}", id(&["incident", "id"]))
        }
        "approval.waiting" => "/remediation/approvals/".into(),
        "backup.failed" => "/admin/backups/".into(),
        "update.available" => "/admin/server-update/".into(),
        "update.failed" => match str_at(d, &["update", "kind"]) {
            Some("campaign") => format!("/campaigns/view/?id={}", id(&["update", "campaign_id"])),
            Some("automatic_rollout") => "/admin/updates/".into(),
            _ if str_at(d, &["relay", "id"]).is_some() => {
                format!("/admin/relays/view/?id={}", id(&["relay", "id"]))
            }
            _ => format!("/servers/view/?id={}&tab=updates", id(&["server", "id"])),
        },
        _ => SETTINGS_PATH.into(),
    }
}

/// Renders `event` with links under `web_url` (no trailing slash).
pub fn render(event: &Event, web_url: &str) -> Message {
    let d = &event.details;
    let text = match event.event.as_str() {
        "test" => "Test notification from MonitorFlow: this channel works.".to_owned(),
        "approval.waiting" => {
            let tier = match str_at(d, &["approval", "tier"]) {
                Some("high_impact") => "high impact",
                _ => "low impact",
            };
            format!(
                "Approval needed on {}: `{}` ({tier})",
                place(d),
                str_at(d, &["approval", "command"]).unwrap_or("a command")
            )
        }
        "update.failed" => update_text(d),
        "backup.failed" => match str_at(d, &["backup", "error"]) {
            Some(error) => format!("Backup of the server failed: {error}"),
            None => "Backup of the server failed".to_owned(),
        },
        "update.available" => format!(
            "MonitorFlow {} is available; this server runs {}",
            str_at(d, &["release", "version"]).unwrap_or("a new release"),
            str_at(d, &["release", "running_version"]).unwrap_or("an older one")
        ),
        e => incident_text(e, d),
    };
    let url = format!("{web_url}{}", path(event));
    let mut payload = Map::new();
    payload.insert("version".into(), PAYLOAD_VERSION.into());
    payload.insert("id".into(), event.id.to_string().into());
    payload.insert("event".into(), event.event.clone().into());
    payload.insert("text".into(), text.clone().into());
    payload.insert("url".into(), url.clone().into());
    if let Some(details) = d.as_object() {
        for (key, value) in details {
            payload.entry(key.clone()).or_insert_with(|| value.clone());
        }
    }
    payload
        .entry("occurred_at")
        .or_insert_with(|| json!(event.created_at));
    let mut body = format!("{text}\n\n{url}\n");
    let facts = [
        ("Customer", str_at(d, &["customer", "name"])),
        ("Site", str_at(d, &["site", "name"])),
        ("Server", str_at(d, &["server", "name"])),
        ("Relay", str_at(d, &["relay", "name"])),
    ];
    if facts.iter().any(|(_, v)| v.is_some()) {
        body.push('\n');
        for (label, value) in facts {
            if let Some(value) = value {
                body.push_str(&format!("{label}: {value}\n"));
            }
        }
    }
    body.push_str(&format!(
        "\n-- \nSent by MonitorFlow. Notification settings: {web_url}{SETTINGS_PATH}\n"
    ));
    Message {
        subject: format!("[MonitorFlow] {text}"),
        text,
        url,
        body,
        payload: Value::Object(payload),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const WEB: &str = "https://mf.example";

    fn event(kind: &str, details: Value) -> Event {
        Event {
            id: 42,
            event: kind.into(),
            customer_id: None,
            details,
            created_at: DateTime::from_timestamp(1_800_000_000, 0).unwrap(),
        }
    }

    fn incident(metric: &str, severity: &str, value: Option<f64>) -> Value {
        json!({
            "customer": {"id": Uuid::nil(), "name": "default"},
            "site": {"id": Uuid::nil(), "name": "paris"},
            "server": {"id": Uuid::nil(), "name": "web-1"},
            "relay": null,
            "incident": {"id": "0f5e", "metric": metric, "instance": "/var",
                         "severity": severity, "value": value},
        })
    }

    #[test]
    fn incidents_read_in_one_line() {
        let opened = render(
            &event(
                "incident.opened",
                incident("monitorflow_disk_used_percent", "critical", Some(97.25)),
            ),
            WEB,
        );
        assert_eq!(
            opened.text,
            "Critical: disk_used_percent /var is 97.25 on web-1 (paris)"
        );
        assert_eq!(opened.subject, format!("[MonitorFlow] {}", opened.text));
        assert_eq!(opened.url, "https://mf.example/incidents/view/?id=0f5e");
        let escalated = render(
            &event(
                "incident.escalated",
                incident("load", "critical", Some(4.0)),
            ),
            WEB,
        );
        assert_eq!(
            escalated.text,
            "Escalated to critical: load /var is 4 on web-1 (paris)"
        );
        let resolved = render(
            &event("incident.resolved", incident("load", "warning", None)),
            WEB,
        );
        assert_eq!(resolved.text, "Resolved: load /var on web-1 (paris)");
    }

    #[test]
    fn offline_incidents_read_as_a_state() {
        let mut details = incident("offline", "critical", None);
        details["server"] = Value::Null;
        details["relay"] = json!({"id": Uuid::nil(), "name": "gw-1"});
        details["customer"]["name"] = "acme".into();
        let down = render(&event("incident.opened", details.clone()), WEB);
        assert_eq!(down.text, "Critical: relay gw-1 (acme / paris) is offline");
        let up = render(&event("incident.resolved", details), WEB);
        assert_eq!(
            up.text,
            "Resolved: relay gw-1 (acme / paris) is back online"
        );
    }

    #[test]
    fn an_offline_incident_only_says_back_online_when_it_is() {
        let resolved = |reason: Option<&str>| {
            let mut details = incident("offline", "critical", None);
            details["incident"]["reason"] = json!(reason);
            render(&event("incident.resolved", details), WEB).text
        };
        let back = "Resolved: web-1 (paris) is back online";
        assert_eq!(resolved(Some("back_online")), back);
        assert_eq!(resolved(None), back);
        assert_eq!(
            resolved(Some("revoked")),
            "Resolved: web-1 (paris) was removed"
        );
        assert_eq!(
            resolved(Some("muted")),
            "Resolved: offline alerts for web-1 (paris) were muted"
        );
    }

    #[test]
    fn a_failed_backup_links_to_the_backups_page() {
        let failed = render(
            &event(
                "backup.failed",
                json!({"backup": {"error": "pg_dump: no space left"}}),
            ),
            WEB,
        );
        assert_eq!(
            failed.text,
            "Backup of the server failed: pg_dump: no space left"
        );
        assert_eq!(failed.url, "https://mf.example/admin/backups/");
    }

    #[test]
    fn a_new_release_links_to_the_update_page() {
        let available = render(
            &event(
                "update.available",
                json!({"release": {"version": "0.3.0", "running_version": "0.2.0"}}),
            ),
            WEB,
        );
        assert_eq!(
            available.text,
            "MonitorFlow 0.3.0 is available; this server runs 0.2.0"
        );
        assert_eq!(available.url, "https://mf.example/admin/server-update/");
        assert_eq!(available.payload["release"]["version"], "0.3.0");
    }

    #[test]
    fn approvals_and_failed_updates_link_to_their_pages() {
        let approval = render(
            &event(
                "approval.waiting",
                json!({"server": {"id": Uuid::nil(), "name": "db-1"},
                       "approval": {"command": "systemctl restart nginx", "tier": "high_impact"}}),
            ),
            WEB,
        );
        assert_eq!(
            approval.text,
            "Approval needed on db-1: `systemctl restart nginx` (high impact)"
        );
        assert_eq!(approval.url, "https://mf.example/remediation/approvals/");

        let server = Uuid::new_v4();
        let failed = render(
            &event(
                "update.failed",
                json!({"server": {"id": server, "name": "db-1"},
                       "update": {"kind": "apply_updates", "error": "dpkg was interrupted"}}),
            ),
            WEB,
        );
        assert_eq!(
            failed.text,
            "Package updates failed on db-1: dpkg was interrupted"
        );
        assert_eq!(
            failed.url,
            format!("https://mf.example/servers/view/?id={server}&tab=updates")
        );

        let relay = render(
            &event(
                "update.failed",
                json!({"relay": {"id": "r1", "name": "gw"},
                       "update": {"kind": "self_update", "version": "1.2.0", "error": null}}),
            ),
            WEB,
        );
        assert_eq!(relay.text, "Update of relay gw to 1.2.0 failed");
        assert_eq!(relay.url, "https://mf.example/admin/relays/view/?id=r1");

        let campaign = render(
            &event(
                "update.failed",
                json!({"update": {"kind": "campaign", "campaign_id": "c1", "name": "May", "error": "wave 1 failed"}}),
            ),
            WEB,
        );
        assert_eq!(
            campaign.text,
            "Update campaign \"May\" failed: wave 1 failed"
        );
        assert_eq!(campaign.url, "https://mf.example/campaigns/view/?id=c1");

        let rollout = render(
            &event(
                "update.failed",
                json!({"update": {"kind": "automatic_rollout", "peers": "relay", "version": "1.3.0", "error": "2 updates failed"}}),
            ),
            WEB,
        );
        assert_eq!(
            rollout.text,
            "Automatic relay updates to 1.3.0 halted: 2 updates failed"
        );
        assert_eq!(rollout.url, "https://mf.example/admin/updates/");
    }

    #[test]
    fn the_payload_carries_the_details_and_the_text() {
        let message = render(
            &event("incident.opened", incident("load", "warning", Some(2.5))),
            WEB,
        );
        let payload = &message.payload;
        assert_eq!(payload["version"], 1);
        assert_eq!(payload["id"], "42");
        assert_eq!(payload["event"], "incident.opened");
        assert_eq!(payload["text"], message.text);
        assert_eq!(payload["url"], message.url);
        assert_eq!(payload["server"]["name"], "web-1");
        assert_eq!(payload["incident"]["severity"], "warning");
        assert_eq!(payload["occurred_at"], "2027-01-15T08:00:00Z");
        assert!(message.body.contains("Server: web-1\n"));
        assert!(message
            .body
            .contains("https://mf.example/admin/notifications/"));
    }

    #[test]
    fn a_test_says_the_channel_works() {
        let message = render(&Event::test(), WEB);
        assert!(message.text.contains("this channel works"));
        assert_eq!(message.payload["event"], "test");
        assert_eq!(message.url, "https://mf.example/admin/notifications/");
    }
}
