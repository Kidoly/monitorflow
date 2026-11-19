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
    "probe",
    "time_interval",
    "schedule",
];

/// Every note, in English. Shared notes first, then one block per reader.
pub const NOTES: &[(&str, &str)] = &[
    // Shared: what the server's planner says.
    (
        "dependency_whole_check",
        "The whole check depends on it: in the source only {trigger} did.",
    ),
    (
        "dependency_unmapped",
        "It depends on {parent}, which is not moved.",
    ),
    (
        "dependency_group",
        "Group {group} depends on it in PRTG: each of its devices does.",
    ),
    (
        "builtin",
        "Every MonitorFlow server measures this already; nothing to add.",
    ),
    (
        "offline_alert",
        "MonitorFlow's offline alert tells when the agent stops answering, after {minutes} minutes.",
    ),
    (
        "web_password",
        "Passwords are not imported: set the password of {username} in the check after the import.",
    ),
    (
        "web_token",
        "Tokens are not imported: set the token as the check's password after the import.",
    ),
    (
        "web_password_http",
        "The source sends a password over plain http; MonitorFlow sends one only over https, so the check runs without it.",
    ),
    (
        "web_auth",
        "MonitorFlow uses basic authentication, not {kind}: the check runs without it.",
    ),
    (
        "web_method",
        "MonitorFlow sends GET or HEAD requests only: the check sends GET instead of {method}, without a body.",
    ),
    (
        "web_regex",
        "MonitorFlow looks for plain text, not a pattern: the condition {pattern} is not moved.",
    ),
    (
        "web_steps",
        "Only the first of {count} steps is moved: a MonitorFlow website check reads one page.",
    ),
    (
        "web_setting",
        "The setting {setting} has no equivalent in a MonitorFlow website check; the rest is moved.",
    ),
    (
        "web_header",
        "The header {name} is not moved: MonitorFlow sets it itself or does not keep it in clear.",
    ),
    (
        "web_certificate_only",
        "The source checks only the certificate; MonitorFlow's website check also requests the page, whatever its status.",
    ),
    (
        "web_response_phase",
        "MonitorFlow measures the whole response time, redirects included, not one phase of it.",
    ),
    (
        "web_no_check",
        "It is about a website this import does not check.",
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
        "window_cut",
        "MonitorFlow maintenance windows last at most 7 days: this one ends 7 days after it starts.",
    ),
    (
        "window_offline_kept",
        "In MonitorFlow a server going offline still notifies: only its checks are silenced.",
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
        "network_traffic",
        "MonitorFlow measures interface traffic but does not move thresholds on it: add one on the server's page.",
    ),
    (
        "snmp_object",
        "An SNMP object outside the common MIBs MonitorFlow reads: interfaces, host resources, UPS, temperature sensors and printer supplies.",
    ),
    (
        "dns_as_udp",
        "MonitorFlow checks that the DNS server answers a query; it does not compare the record.",
    ),
    (
        "no_address",
        "There is no address MonitorFlow can check it at.",
    ),
    (
        "device_no_agent",
        "It becomes a device, which has no agent to measure this.",
    ),
    (
        "snmp_of_agent",
        "MonitorFlow polls SNMP on devices; on a server, its agent measures this itself.",
    ),
    (
        "snmp_credentials",
        "SNMP credentials are not moved: set them on the device's page, and its SNMP checks start.",
    ),
    (
        "builtin_net",
        "Every server measures the traffic of its network interfaces already.",
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
    // Zabbix.
    (
        "zabbix_stock_template",
        "Read from the defaults of the stock template {template}, which the export does not include: export it too if it was changed.",
    ),
    (
        "zabbix_template_missing",
        "The template {template} is not among the uploaded files: export it too, or connect to the Zabbix API.",
    ),
    (
        "zabbix_partial",
        "Only the condition {kept} is moved; MonitorFlow does not check the rest of the expression.",
    ),
    (
        "zabbix_no_measure",
        "MonitorFlow has no threshold on this measure ({key}).",
    ),
    (
        "zabbix_bytes",
        "MonitorFlow compares shares of memory and disk space, not amounts such as {value}.",
    ),
    (
        "zabbix_no_recipients",
        "An export has no users' media, so no recipients: connect to the Zabbix API to read them, or add the channel by hand.",
    ),
    (
        "zabbix_web_step",
        "Only the first step of a web scenario is moved; this is about the step {step}.",
    ),
    (
        "zabbix_not_set",
        "Its setting {setting} has no usable address in the source.",
    ),
    // Prometheus.
    (
        "scrape_as_port",
        "MonitorFlow checks that port {port} answers; it does not read the exporter's metrics.",
    ),
    (
        "disk_wait",
        "MonitorFlow compares the average wait of the disk's operations, reads and writes together, which differs from what the source measures.",
    ),
    (
        "cpu_busy_modes",
        "MonitorFlow counts every busy mode of the processors, also the ones the source leaves out.",
    ),
    (
        "extra_condition",
        "The source adds a condition MonitorFlow does not check; only the threshold is moved.",
    ),
    (
        "service_failed",
        "MonitorFlow alerts when the service is not active, not only when it failed.",
    ),
    (
        "service_discovery",
        "The targets come from {kind}, which only a running Prometheus resolves: connect to it to read them.",
    ),
    (
        "file_missing",
        "The configuration refers to {file}, which is not among the uploaded files.",
    ),
    (
        "secret_hidden",
        "The source's API does not show this secret: upload its configuration file to move it, or add the channel by hand.",
    ),
    (
        "secret_in_file",
        "The source reads this secret from {file}, which is not part of the import: add the channel by hand.",
    ),
    (
        "routes",
        "MonitorFlow sends every alert to every channel of its site; the source's routing by labels is not moved.",
    ),
    // PRTG.
    (
        "zabbix_no_data",
        "Zabbix stopped collecting data during this maintenance; MonitorFlow keeps watching and only holds notifications.",
    ),
    (
        "zabbix_tags_unmatched",
        "No imported check comes from a trigger with these tags ({tags}): the whole server is silenced.",
    ),
    (
        "am_active_intervals",
        "Alertmanager sends this route only within its active time intervals, which MonitorFlow does not have.",
    ),
    (
        "am_interval",
        "This time interval cannot be said as a MonitorFlow maintenance window: {detail}.",
    ),
    (
        "am_months_weekly",
        "Weekdays of some months only: MonitorFlow's weekly windows apply every month.",
    ),
    (
        "am_years",
        "Limited to some years: the window applies from the first of them to the last.",
    ),
    (
        "am_route_labels",
        "The route also matched other labels ({matchers}): MonitorFlow holds every notification of these servers.",
    ),
    (
        "prtg_from_server",
        "Checked by the server's own agent, not from a probe: a network problem between them is not seen.",
    ),
    (
        "prtg_sensor_type",
        "MonitorFlow has no check like this PRTG sensor ({type}).",
    ),
    (
        "prtg_no_setting",
        "The sensor's settings do not say what it checks.",
    ),
    (
        "prtg_limit",
        "MonitorFlow has no threshold for the limits of the channel {channel}.",
    ),
    (
        "prtg_every_disk",
        "No drive or mount point in its name: the threshold applies to every disk of the server.",
    ),
    (
        "prtg_event_levels",
        "No event type could be read: MonitorFlow counts critical events and errors.",
    ),
    (
        "prtg_event_filter",
        "A filter on the message or on event IDs could not be moved: more events may match.",
    ),
    (
        "prtg_event_count",
        "MonitorFlow alerts on the first matching event; PRTG alerted above {count}.",
    ),
    (
        "prtg_same_port",
        "Another sensor of this device checks port {port} already.",
    ),
    (
        "prtg_not_on_device",
        "It belongs to a probe or a group, not to a device.",
    ),
    (
        "prtg_paused",
        "Paused in PRTG with no end: the silence lasts 30 days, the longest MonitorFlow allows.",
    ),
    (
        "prtg_paused_by_rule",
        "Paused by a schedule or a dependency in PRTG, which MonitorFlow does not have.",
    ),
    (
        "prtg_paused_sensor",
        "A paused sensor MonitorFlow has no check for: silencing its whole server would hide more than PRTG did.",
    ),
    (
        "prtg_local_time",
        "The file does not say PRTG's time zone: the window is read as UTC.",
    ),
    (
        "prtg_schedule_unknown",
        "Paused by the schedule {schedule}, whose periods are not in the source or are a time table: it cannot be moved.",
    ),
    (
        "prtg_triggers",
        "MonitorFlow sends it every incident; PRTG used it only where a notification trigger named it.",
    ),
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn notes_fill_their_arguments() {
        let note = Note::new("web_password", args! {"username" => "monitor"});
        assert_eq!(
            note.text(),
            "Passwords are not imported: set the password of monitor in the check after the import."
        );
        assert_eq!(
            Note::plain("builtin_net").text(),
            "Every server measures the traffic of its network interfaces already."
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
