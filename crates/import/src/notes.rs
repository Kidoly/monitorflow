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
    "discovery_rule",
    "action",
    "service",
    "sla",
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
        "snmp_no_oid",
        "No OID could be read from this custom SNMP sensor.",
    ),
    (
        "snmp_lookup_missing",
        "The lookup {name} was not among the files read: import its .ovl file with the configuration, or pick the channel's names of values after the import.",
    ),
    (
        "snmp_custom_more",
        "{count} more channels are left out: a custom SNMP check has at most 10.",
    ),
    (
        "snmp_string_one",
        "Only the first text condition is kept: a channel expects one text.",
    ),
    (
        "snmp_string_length",
        "A check of the text's length or of a number in it becomes a check of the text.",
    ),
    (
        "snmp_preprocessing",
        "The preprocessing step {step} is left out.",
    ),
    (
        "snmp_walk_table",
        "One table check, with a row per entry, stands for an item per row.",
    ),
    (
        "snmp_exporter_override",
        "Overrides and regular expressions of the module are left out.",
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
        "vmware_password",
        "The vCenter password is not moved: set the credentials on the device's page (a Read-only account is enough), and its check starts with the alerts that come with it.",
    ),
    (
        "vmware_discovered",
        "The source found it through its vCenter: it is listed on the vCenter's page, with the vCenter's alerts, and is not added again.",
    ),
    (
        "vmware_every_datastore",
        "MonitorFlow applies this to every datastore of the vCenter, not to one.",
    ),
    (
        "vmware_wbem",
        "MonitorFlow reads the host's hardware sensors through the vCenter API, not WBEM.",
    ),
    (
        "vmware_of_agent",
        "MonitorFlow reads a vCenter or an ESXi host as a device, and this host has an agent.",
    ),
    (
        "hyperv_group",
        "The agent's account must join Hyper-V Administrators on the host; then its Hyper-V check starts.",
    ),
    (
        "hyperv_windows",
        "Hyper-V is read by the agent of a Windows server.",
    ),
    (
        "app_password",
        "The password is not moved: enter it on the check, which then starts with the alerts that come with it.",
    ),
    (
        "app_query",
        "PRTG ran a query of its own, which MonitorFlow does not: its check measures the connection, the response time and the usual measures of the database. Enter the password on the check.",
    ),
    (
        "app_template",
        "The template {template} becomes MonitorFlow's ready-made check, with its own measures and alerts in place of the template's items and triggers. Enter the password on the check.",
    ),
    (
        "app_docker_template",
        "The template {template} becomes the agent's Docker check, with its own measures and alerts in place of the template's items and triggers.",
    ),
    (
        "app_exporter",
        "MonitorFlow connects to the database itself instead of reading {exporter}, on the exporter's machine and the default port: enter the password on the check. The exporter's other metrics are not moved.",
    ),
    (
        "app_cadvisor",
        "The agent's Docker check reads the containers instead of cAdvisor; cAdvisor's other metrics are not moved.",
    ),
    (
        "app_docker_all",
        "PRTG watched one container, whose name was not read: the Docker check watches every container; name it on the check to watch only that one.",
    ),
    (
        "app_docker_agent",
        "Docker is read by the agent of the server that runs it, and this host becomes a device.",
    ),
    (
        "app_other_database",
        "MonitorFlow has ready-made checks for PostgreSQL, MySQL, MariaDB and Redis, not for this database yet.",
    ),
    (
        "virtual_partial",
        "MonitorFlow reads the state, processor, memory, storage and snapshots of each virtual machine; traffic per network adapter and storage latency are not measured.",
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
    (
        "scan_disabled",
        "Turned off in the source: the scan is imported without its schedule, to run when asked.",
    ),
    (
        "scan_cut",
        "A scan tries at most 1,024 addresses: the first 1,024 of the {count} the source scanned are kept.",
    ),
    (
        "scan_skipped",
        "A scan takes IP addresses, blocks and ranges only: {entries} is left out.",
    ),
    (
        "scan_ports_cut",
        "A scan tries at most 32 ports: the first 32 of {count} are kept.",
    ),
    (
        "scan_every_hour",
        "The source scanned every {delay}; MonitorFlow scans every hour, the nearest it allows.",
    ),
    (
        "scan_every_day",
        "The source scanned every {delay}; MonitorFlow scans every day, the nearest it allows.",
    ),
    (
        "scan_every_week",
        "The source scanned every {delay}; MonitorFlow scans every week, the nearest it allows.",
    ),
    (
        "scan_proposes",
        "The source added what it found by itself ({action}); MonitorFlow proposes what a scan finds, and a person adds it.",
    ),
    (
        "scan_snmp_refused",
        "The SNMP credentials are not moved, as MonitorFlow refuses them: {reason}.",
    ),
    // Zabbix.
    (
        "zabbix_snmp_old",
        "MonitorFlow does not use SNMPv1: the scan tries the community with SNMPv2c.",
    ),
    (
        "zabbix_snmp_protocol",
        "MonitorFlow does not use the SNMPv3 protocol {protocol}: the SNMP credentials are not moved.",
    ),
    (
        "zabbix_snmp_first",
        "A scan tries one set of SNMP credentials besides those of the site's devices: only the first check's are kept.",
    ),
    (
        "zabbix_autoregistration",
        "MonitorFlow agents connect with the install command of their site: no rule is needed to add them.",
    ),
    (
        "zabbix_stock_template",
        "Read from the defaults of the stock template {template}, which the export does not include: export it too if it was changed.",
    ),
    (
        "zabbix_template_missing",
        "The template {template} is not among the uploaded files: export it too, or connect to the Zabbix API.",
    ),
    (
        "zabbix_change",
        "Zabbix compares the last two values; MonitorFlow compares the change over {seconds} seconds.",
    ),
    (
        "zabbix_fit",
        "Zabbix fits a {fit} trend; MonitorFlow fits a straight line.",
    ),
    (
        "zabbix_trend",
        "Zabbix averages its hourly trends; MonitorFlow averages the values themselves over the same {seconds} seconds.",
    ),
    (
        "zabbix_values_window",
        "Zabbix reads the last {count} values; MonitorFlow reads the last {seconds} seconds, the time they take.",
    ),
    (
        "window_rounded",
        "MonitorFlow reads windows of 1 minute to 1 day; this one is {seconds} seconds.",
    ),
    (
        "prometheus_deriv",
        "Prometheus fits a line to the samples (deriv); MonitorFlow divides the change by the time.",
    ),
    (
        "prometheus_compare_window",
        "Prometheus compares single values; MonitorFlow compares averages over 5 minutes.",
    ),
    (
        "prometheus_absent",
        "Prometheus alerts when no series at all is left (absent); MonitorFlow alerts on each series that stops.",
    ),
    (
        "kind_taken",
        "MonitorFlow keeps one rule of each kind per measure, and moves {kept} on it.",
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
        "prtg_port_only",
        "MonitorFlow checks that port {port} accepts connections; PRTG also spoke {protocol} to it.",
    ),
    (
        "prtg_sensor_type",
        "MonitorFlow has no check like this PRTG sensor ({type}).",
    ),
    (
        "flow_sensor",
        "Traffic flows are not a check: open the device's Flows tab, turn on receiving, and point its NetFlow, sFlow or IPFIX exporter at the central server or the relay it shows.",
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
        "prtg_scan_monthly",
        "PRTG scanned once a month; MonitorFlow scans every week, the least often it allows.",
    ),
    (
        "prtg_discovery_ad",
        "PRTG takes these devices from Active Directory; a MonitorFlow scan needs IP addresses, blocks or ranges.",
    ),
    (
        "prtg_triggers",
        "MonitorFlow sends it every incident; PRTG used it only where a notification trigger named it.",
    ),
    // Business services (ADR-0155).
    (
        "service_hosts",
        "MonitorFlow counts each host whose problems the service's tags select as a whole member: {hosts}.",
    ),
    (
        "service_children",
        "Child services are flattened: their hosts are members of this service, under its rule.",
    ),
    (
        "service_rule",
        "The rule {rule} has no equivalent: the service is down while any member is down.",
    ),
    (
        "service_weights",
        "Weights are not moved: every member counts the same.",
    ),
    (
        "service_no_members",
        "No host of the import has a problem these tags select.",
    ),
    (
        "sla_schedule",
        "The SLA counts only the hours of its schedule; MonitorFlow counts every hour, without planned maintenance.",
    ),
    (
        "sla_downtimes",
        "The SLA's excluded downtimes are not moved; maintenances moved as maintenance windows are left out instead.",
    ),
    (
        "sla_several",
        "{count} SLAs select this service: the strictest target, {target} %, is kept.",
    ),
    (
        "prtg_business_process",
        "PRTG does not give which objects a business process channel watches: make it a business service under Reports. Its channels: {channels}.",
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
