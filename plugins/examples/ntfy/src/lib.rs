//! A notification channel posting to an ntfy topic (ADR-0103), on ntfy.sh
//! or a server of your own: https://docs.ntfy.sh/publish/

use monitorflow_plugin_sdk::{channel, ChannelInput, Request};

fn run(input: ChannelInput) -> Result<(), String> {
    let server = input.setting("server")?.trim_end_matches('/');
    let topic = input.setting("topic")?;
    if topic.is_empty()
        || !topic
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
    {
        return Err("the topic may hold only letters, digits, - and _".into());
    }
    let message = &input.message;
    let mut body = message.text.clone();
    for line in &message.lines {
        body.push('\n');
        body.push_str(line);
    }
    let mut request = Request::post(format!("{server}/{topic}"), body)
        .header("Title", "MonitorFlow")
        .header("Priority", priority(&message.event, &message.payload))
        .header("Tags", tag(&message.event));
    if !message.url.is_empty() {
        request = request.header("Click", message.url.as_str());
    }
    if let Some(token) = input.secrets.get("token") {
        request = request.header("Authorization", format!("Bearer {token}"));
    }
    let response = request.send()?;
    if response.is_success() {
        Ok(())
    } else {
        Err(format!(
            "ntfy answered {}: {}",
            response.status,
            first_line(&response.body)
        ))
    }
}

/// ntfy's 1 to 5: urgent for what opens critical, high for the rest that
/// needs someone.
pub fn priority(event: &str, payload: &monitorflow_plugin_sdk::serde_json::Value) -> &'static str {
    let critical = payload["incident"]["severity"].as_str() == Some("critical");
    match event {
        "incident.opened" | "incident.escalated" if critical => "5",
        "incident.opened" | "incident.escalated" | "approval.waiting" | "update.failed"
        | "backup.failed" => "4",
        _ => "3",
    }
}

fn tag(event: &str) -> &'static str {
    match event {
        "incident.resolved" => "white_check_mark",
        "incident.opened" | "incident.escalated" | "update.failed" | "backup.failed" => "warning",
        _ => "bell",
    }
}

fn first_line(text: &str) -> String {
    text.lines()
        .next()
        .unwrap_or_default()
        .chars()
        .take(200)
        .collect()
}

channel!(run);

#[cfg(test)]
mod tests {
    use monitorflow_plugin_sdk::serde_json::json;

    use super::*;

    #[test]
    fn critical_incidents_are_urgent() {
        let critical = json!({"incident": {"severity": "critical"}});
        assert_eq!(priority("incident.opened", &critical), "5");
        assert_eq!(priority("incident.opened", &json!({})), "4");
        assert_eq!(priority("incident.resolved", &critical), "3");
        assert_eq!(priority("test", &json!({})), "3");
    }
}
