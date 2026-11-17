//! Why an item is approximated or cannot be moved, as codes with
//! arguments: the web interface shows them in the reader's language from
//! its catalog (`web/src/i18n/messages/imports.ts`), and API clients get the English
//! text too. A reader adds a code here and in that catalog, in English and
//! French; a test checks both lists agree.

use std::collections::BTreeMap;

use serde::Serialize;

/// Arguments for [`Note::new`]: `args!{"port" => 22}`.
#[macro_export]
macro_rules! args {
    ($($key:literal => $value:expr),* $(,)?) => {{
        #[allow(unused_mut)]
        let mut args = $crate::model::Args::new();
        $(args.insert($key.to_owned(), $value.to_string());)*
        args
    }};
}

/// A sentence by code, with its arguments.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Note {
    pub code: String,
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    pub args: BTreeMap<String, String>,
}

impl Note {
    /// A note of `code`, which must be in [`NOTES`].
    pub fn new(code: &str, args: BTreeMap<String, String>) -> Self {
        debug_assert!(english(code).is_some(), "unknown note code {code}");
        Self {
            code: code.to_owned(),
            args,
        }
    }

    pub fn plain(code: &str) -> Self {
        Self::new(code, BTreeMap::new())
    }

    /// The English sentence, its `{name}` placeholders filled in.
    pub fn text(&self) -> String {
        let template = english(&self.code).unwrap_or(&self.code);
        let mut out = template.to_owned();
        for (key, value) in &self.args {
            out = out.replace(&format!("{{{key}}}"), value);
        }
        out
    }
}

fn english(code: &str) -> Option<&'static str> {
    NOTES.iter().find(|(c, _)| *c == code).map(|(_, t)| *t)
}

/// What [`crate::model::Origin::kind`] may be.
pub const ORIGIN_KINDS: &[&str] = &[
    "host",
    "item",
    "trigger",
    "template",
    "web_scenario",
    "maintenance",
    "media_type",
    "user_media",
    "scrape_target",
    "alerting_rule",
    "receiver",
    "silence",
    "sensor",
    "notification",
    "device",
    "group",
];

/// Every note, in English. Shared notes first, then one block per reader.
pub const NOTES: &[(&str, &str)] = &[
    // Shared: what the server's planner says.
    (
        "builtin",
        "Every MonitorFlow server measures this already; nothing to add.",
    ),
    (
        "offline_alert",
        "MonitorFlow's offline alert tells when the agent stops answering, after {minutes} minutes.",
    ),
    (
        "web_as_port",
        "MonitorFlow checks that port {port} of {host} answers; it does not read the page or its status code.",
    ),
    (
        "silence_next_window",
        "Recurring in the source: only the next window is added, from {starts} to {ends}.",
    ),
    (
        "silence_cut",
        "MonitorFlow silences last at most 30 days: this one ends on {ends}.",
    ),
    (
        "silence_over",
        "The window is over already.",
    ),
    (
        "silence_whole_server",
        "MonitorFlow silences every alert of a server, not only the ones the source matched ({matchers}).",
    ),
    (
        "smtp_password",
        "The mail server password is not in the source: type it in the channel after the import.",
    ),
    (
        "no_smtp",
        "No mail server is set for this address in the source; add an email channel with yours.",
    ),
    (
        "webhook_payload",
        "MonitorFlow sends its own signed JSON, not the source's message format.",
    ),
    (
        "duration_rounded",
        "Waits {seconds} seconds before alerting, the nearest MonitorFlow allows.",
    ),
    (
        "interval_clamped",
        "Runs every {seconds} seconds, the nearest interval MonitorFlow allows.",
    ),
    (
        "severity_mapped",
        "The source's severity {severity} becomes {mapped}.",
    ),
    (
        "process_as_service",
        "MonitorFlow watches the systemd service {name} instead of counting processes.",
    ),
    (
        "disk_free_as_used",
        "Read as space used, which counts the space kept for the system as used.",
    ),
    (
        "log_dirs",
        "The agent reads only logs under the directories its configuration allows.",
    ),
    (
        "server_elsewhere",
        "A server of this name is in a site you do not administer.",
    ),
    (
        "refused",
        "MonitorFlow refuses it: {reason}.",
    ),
    (
        "unknown_host",
        "It refers to a host that is not part of this import.",
    ),
    (
        "udp",
        "MonitorFlow checks TCP ports only.",
    ),
    (
        "network_traffic",
        "MonitorFlow does not measure network interface traffic yet.",
    ),
    (
        "no_equivalent",
        "MonitorFlow has no check of this kind.",
    ),
    (
        "expression",
        "MonitorFlow cannot read this expression; add a check and an alert by hand if you need it.",
    ),
    (
        "per_cpu_load",
        "The source compares the load per processor; MonitorFlow compares the load itself.",
    ),
    (
        "script",
        "Commands run by the source are not moved: put the script in the agent's scripts directory and add a custom script check.",
    ),
    (
        "snmp",
        "MonitorFlow watches servers through its agent; SNMP devices cannot be moved.",
    ),
    (
        "no_target",
        "Notifications of this kind cannot be sent by MonitorFlow.",
    ),
    (
        "chat_token",
        "The source posts with an app token; MonitorFlow needs an incoming webhook URL. Add a channel with one.",
    ),
    (
        "no_hosts",
        "It covers no host of this import.",
    ),
    (
        "disabled",
        "Turned off in the source.",
    ),
    (
        "unresolved_macro",
        "It uses a value that is not in the source: {macro}.",
    ),
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn notes_fill_their_arguments() {
        let note = Note::new(
            "web_as_port",
            args! {"port" => 443, "host" => "shop.example"},
        );
        assert_eq!(
            note.text(),
            "MonitorFlow checks that port 443 of shop.example answers; it does not read the page or its status code."
        );
        assert_eq!(
            Note::plain("udp").text(),
            "MonitorFlow checks TCP ports only."
        );
    }

    #[test]
    fn codes_are_unique_and_plain() {
        let mut codes: Vec<_> = NOTES.iter().map(|(c, _)| *c).collect();
        codes.sort_unstable();
        let before = codes.len();
        codes.dedup();
        assert_eq!(before, codes.len(), "a note code is listed twice");
        for code in codes {
            assert!(
                code.chars().all(|c| c.is_ascii_lowercase() || c == '_'),
                "{code}"
            );
        }
    }

    /// The web interface's catalog has every note and origin kind, in
    /// English and French.
    #[test]
    fn the_web_catalog_has_every_code() {
        let catalog = include_str!("../../../web/src/i18n/messages/imports.ts");
        for (code, _) in NOTES {
            let key = format!("\"note.{code}\"");
            assert_eq!(
                catalog.matches(&key).count(),
                2,
                "{key} should be in the English and the French catalog"
            );
        }
        for kind in ORIGIN_KINDS {
            let key = format!("\"origin.{kind}\"");
            assert_eq!(catalog.matches(&key).count(), 2, "{key}");
        }
    }
}
