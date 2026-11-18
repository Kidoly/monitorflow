//! From a reader's inventory to the items of a preview (ADR-0146).

use std::collections::{BTreeMap, HashMap, HashSet};

use chrono::{DateTime, SubsecRound, TimeDelta, Utc};
use monitorflow_import::args;
use monitorflow_import::model::{
    Alert, Check, Direction, Fidelity, Inventory, Measure, Os, Severity, SilenceScope, Target,
    Watch,
};
use monitorflow_import::Note;
use serde_json::{json, Map, Value};
use uuid::Uuid;

use super::{ChannelSecret, Create, Item, List, NoteView, OriginView};
use crate::api::{sensors, thresholds};
use crate::enroll::install_key::peer_id_from_hostname;
use crate::sensor_config::{MAX_CHECK_INTERVAL_MS, MIN_CHECK_INTERVAL_MS};

/// Interval of a check whose source says none.
const DEFAULT_INTERVAL_MS: u64 = 60_000;
/// Longest silence (ADR-0079).
const MAX_SILENCE_DAYS: i64 = 30;
/// Longest silence reason, in characters.
const MAX_REASON: usize = 200;
/// Minutes before an offline alert, as the offline rule waits.
const OFFLINE_MINUTES: u32 = 5;

/// A server MonitorFlow has.
#[derive(Debug, Clone)]
pub struct ExistingServer {
    pub peer_id: String,
    pub hostname: String,
    pub site_id: Uuid,
    /// Whether the person importing administers its site.
    pub may_change: bool,
}

/// What MonitorFlow has already, which the plan does not create again.
#[derive(Debug, Clone, Default)]
pub struct Existing {
    pub servers: Vec<ExistingServer>,
    /// Checks of servers by (lowercase peer ID, name), instance `""`.
    pub checks: HashSet<(String, String)>,
    /// Thresholds of servers by (lowercase peer ID, metric, instance).
    pub thresholds: HashSet<(String, String, Option<String>)>,
    /// Silences not over by (lowercase peer ID, or `None` for the site),
    /// end and reason.
    pub silences: HashSet<(Option<String>, DateTime<Utc>, String)>,
    /// Channels by [`channel_key`].
    pub channels: HashSet<String>,
}

impl Existing {
    /// The server a host of `name` (or `peer_id`) is, if MonitorFlow has
    /// it.
    fn server(&self, peer_id: &str, name: &str) -> Option<&ExistingServer> {
        self.servers
            .iter()
            .find(|s| s.peer_id.eq_ignore_ascii_case(peer_id))
            .or_else(|| {
                self.servers
                    .iter()
                    .find(|s| s.hostname.eq_ignore_ascii_case(name.trim()))
            })
    }
}

/// A preview's items, and the secrets of its channel items.
#[derive(Debug, Default)]
pub struct Planned {
    pub items: Vec<Item>,
    pub secrets: BTreeMap<String, ChannelSecret>,
}

/// What identifies a channel: its kind and where it sends.
pub fn channel_key(kind: &str, target: &str) -> String {
    format!("{kind}:{}", target.trim().to_ascii_lowercase())
}

/// The plan of `inventory` for the site `site_id`. Channels cover
/// everything when `channels_everywhere`, the site otherwise.
pub fn plan(
    inventory: &Inventory,
    site_id: Uuid,
    channels_everywhere: bool,
    existing: &Existing,
    now: DateTime<Utc>,
) -> Planned {
    let mut planner = Planner {
        inventory,
        site_id,
        channels_everywhere,
        existing,
        now,
        out: Planned::default(),
        hosts: HashMap::new(),
        used_ids: HashSet::new(),
        check_names: HashSet::new(),
        checks: HashMap::new(),
        alerts: HashMap::new(),
        thresholds: BTreeMap::new(),
    };
    planner.hosts();
    for check in &inventory.checks {
        planner.check(check);
    }
    planner.thresholds();
    planner.silences();
    planner.channels();
    planner.unmovable();
    planner.out
}

/// What a host became.
#[derive(Debug, Clone)]
struct Server {
    /// The server's ID in MonitorFlow.
    peer_id: String,
    /// The host's name in the source.
    name: String,
    /// The item that creates it, when one does.
    item: Option<String>,
    os: Os,
    /// Whether checks may be added to it.
    may_change: bool,
    /// A device's address: its checks run from a probe (ADR-0148).
    device: Option<String>,
    /// An address a probe reaches it at, for checks of a server.
    address: Option<String>,
}

/// Thresholds merged by (server, metric, instance).
type ThresholdKey = (String, String, Option<String>);

struct Merged {
    server: Server,
    comparison: &'static str,
    warning: Option<f64>,
    critical: Option<f64>,
    duration_s: u32,
    hysteresis: f64,
    origins: Vec<OriginView>,
    note: Option<NoteView>,
}

struct Planner<'a> {
    inventory: &'a Inventory,
    site_id: Uuid,
    channels_everywhere: bool,
    existing: &'a Existing,
    now: DateTime<Utc>,
    out: Planned,
    /// By host key.
    hosts: HashMap<String, Server>,
    used_ids: HashSet<String>,
    /// (lowercase peer ID, check name) planned already.
    check_names: HashSet<(String, String)>,
    /// Checks planned by (lowercase peer ID, kind, parameters), to their
    /// name.
    checks: HashMap<(String, String, String), String>,
    /// Alert items planned, by metric of a server, to their index.
    alerts: HashMap<String, usize>,
    thresholds: BTreeMap<ThresholdKey, Merged>,
}

fn list_of(fidelity: &Fidelity) -> (List, Option<NoteView>) {
    match fidelity {
        Fidelity::Exact => (List::Exact, None),
        Fidelity::Approximated(note) => (List::Approximated, Some(note.into())),
    }
}

fn note(code: &str, args: monitorflow_import::model::Args) -> Option<NoteView> {
    Some((&Note::new(code, args)).into())
}

fn os_family(os: Os) -> &'static str {
    match os {
        Os::Debian => "debian",
        Os::Rhel => "rhel",
        Os::Windows => "windows",
        Os::Linux | Os::Unknown => "linux",
    }
}

/// `service_nginx`: a check name from a prefix and free text, as the web
/// interface names checks.
fn check_name(prefix: &str, text: &str) -> String {
    let base = text.rsplit(['/', '\\']).next().unwrap_or_default();
    let base = base.to_ascii_lowercase();
    let base = [".service", ".log", ".sh", ".ps1"]
        .iter()
        .fold(base, |b, s| {
            b.strip_suffix(s).map(str::to_owned).unwrap_or(b)
        });
    let mut slug = String::new();
    for c in base.chars() {
        if c.is_ascii_lowercase() || c.is_ascii_digit() {
            slug.push(c);
        } else if !slug.ends_with('_') {
            slug.push('_');
        }
    }
    let slug = slug.trim_matches('_');
    let name = if slug.is_empty() {
        prefix.to_owned()
    } else {
        format!("{prefix}_{slug}")
    };
    name.chars()
        .take(56)
        .collect::<String>()
        .trim_end_matches('_')
        .to_owned()
}

impl Planner<'_> {
    fn push(&mut self, item: Item) {
        if self.out.items.len() < super::MAX_ITEMS {
            self.out.items.push(item);
        }
    }

    fn unique_id(&mut self, base: String) -> String {
        let mut id = base.clone();
        let mut n = 2;
        while !self.used_ids.insert(id.clone()) {
            id = format!("{base}#{n}");
            n += 1;
        }
        id
    }

    fn hosts(&mut self) {
        let mut by_peer: HashMap<String, Server> = HashMap::new();
        for host in &self.inventory.hosts {
            let peer_id = peer_id_from_hostname(&host.name);
            let lower = peer_id.to_ascii_lowercase();
            if let Some(server) = by_peer.get(&lower) {
                // Two hosts of one name are one server.
                self.hosts.insert(host.key.clone(), server.clone());
                continue;
            }
            let existing = self.existing.server(&peer_id, &host.name);
            let id = self.unique_id(format!("server:{lower}"));
            let addresses = host.addresses.clone();
            // The first address a probe can reach, else the name.
            let address = host
                .addresses
                .iter()
                .chain(std::iter::once(&host.name))
                .map(|a| a.trim())
                .find(|a| crate::api::devices::check_address(a).is_ok())
                .map(str::to_owned);
            if host.agentless && existing.is_none() && address.is_none() {
                self.not_movable(
                    "server",
                    (&host.origin).into(),
                    vec![host.name.clone()],
                    note("no_address", args! {}),
                );
                continue;
            }
            let (peer_id, present, may_change) = match existing {
                Some(s) => (s.peer_id.clone(), true, s.may_change),
                None => (peer_id, false, true),
            };
            let item = Item {
                id: id.clone(),
                kind: "server".into(),
                list: List::Exact,
                note: None,
                origin: (&host.origin).into(),
                hosts: vec![host.name.clone()],
                present,
                needs: vec![],
                create: Some(match (&address, host.agentless) {
                    (Some(address), true) => Create::Device {
                        peer_id: peer_id.clone(),
                        hostname: host.name.trim().to_owned(),
                        address: address.clone(),
                    },
                    _ => Create::Server {
                        peer_id: peer_id.clone(),
                        hostname: host.name.trim().to_owned(),
                        os_family: os_family(host.os).into(),
                        addresses,
                    },
                }),
            };
            self.push(item);
            let server = Server {
                peer_id,
                name: host.name.clone(),
                item: (!present).then_some(id),
                os: host.os,
                may_change,
                device: address.clone().filter(|_| host.agentless),
                address,
            };
            by_peer.insert(lower, server.clone());
            self.hosts.insert(host.key.clone(), server);
        }
    }

    /// The server of host `key`, or an item saying the host is unknown.
    fn server_of(&self, key: &str) -> Option<Server> {
        self.hosts.get(key).cloned()
    }

    fn needs(server: &Server) -> Vec<String> {
        server.item.iter().cloned().collect()
    }

    fn not_movable(
        &mut self,
        kind: &str,
        origin: OriginView,
        hosts: Vec<String>,
        why: Option<NoteView>,
    ) {
        let id = self.unique_id(format!("other:{}:{}", origin.kind, origin.name));
        self.push(Item {
            id,
            kind: kind.into(),
            list: List::NotMovable,
            note: why,
            origin,
            hosts,
            present: false,
            needs: vec![],
            create: None,
        });
    }

    fn check(&mut self, check: &Check) {
        let origin: OriginView = (&check.origin).into();
        let Some(server) = self.server_of(&check.host) else {
            self.not_movable("check", origin, vec![], note("unknown_host", args! {}));
            return;
        };
        let (mut list, mut why) = list_of(&check.fidelity);
        let mut probe = false;
        // What a device's SNMP says stands for what an agent measures
        // (ADR-0149).
        let snmp_group = |group: &str| Watch::Snmp {
            group: group.into(),
        };
        let watch = match (&check.watch, server.device.is_some()) {
            (Watch::Cpu | Watch::Memory | Watch::Disk, true) => snmp_group("host"),
            (Watch::Traffic, true) => snmp_group("interfaces"),
            (watch, _) => watch.clone(),
        };
        // Where a probe check of this server connects: a device's own
        // address by default, else the address named or the server's.
        let target = |address: &Option<String>| -> Option<Option<String>> {
            match (address, &server.device) {
                (Some(a), _) => Some(Some(a.clone())),
                (None, Some(_)) => Some(None),
                (None, None) => server.address.clone().map(Some),
            }
        };
        let (kind, params, prefix_text): (&str, Map<String, Value>, (String, String)) = match &watch
        {
            Watch::DiskIo | Watch::Agent if server.device.is_some() => {
                self.not_movable(
                    "check",
                    origin,
                    vec![server.name.clone()],
                    note("device_no_agent", args! {}),
                );
                return;
            }
            Watch::Ping { address } => {
                let Some(host) = target(address) else {
                    self.not_movable(
                        "check",
                        origin,
                        vec![server.name.clone()],
                        note("no_address", args! {}),
                    );
                    return;
                };
                probe = true;
                let mut params = Map::new();
                if let Some(host) = &host {
                    params.insert("host".into(), json!(host));
                }
                let text = if server.device.is_some() && address.is_none() {
                    String::new()
                } else {
                    host.unwrap_or_default()
                };
                ("ping", params, ("ping".into(), text))
            }
            Watch::UdpPort {
                address,
                port,
                query,
            } => {
                let Some(host) = target(address) else {
                    self.not_movable(
                        "check",
                        origin,
                        vec![server.name.clone()],
                        note("no_address", args! {}),
                    );
                    return;
                };
                probe = true;
                let mut params = Map::new();
                params.insert("port".into(), json!(port.to_string()));
                params.insert("protocol".into(), json!("udp"));
                if let Some(query) = query {
                    params.insert("query".into(), json!(query));
                }
                if let Some(host) = &host {
                    params.insert("host".into(), json!(host));
                }
                ("port", params, ("udp".into(), port.to_string()))
            }
            Watch::Snmp { group } => {
                if server.device.is_none() {
                    self.not_movable(
                        "check",
                        origin,
                        vec![server.name.clone()],
                        note("snmp_of_agent", args! {}),
                    );
                    return;
                }
                probe = true;
                let mut params = Map::new();
                params.insert("what".into(), json!(group));
                if list == List::Exact {
                    list = List::Approximated;
                    why = note("snmp_credentials", args! {});
                }
                let name = match group.as_str() {
                    "system" => "snmp",
                    "interfaces" => "snmp_if",
                    "host" => "snmp_host",
                    "ups" => "snmp_ups",
                    "temperature" => "snmp_temp",
                    "printer" => "snmp_printer",
                    _ => "snmp",
                };
                ("snmp", params, (name.into(), String::new()))
            }
            Watch::Traffic => {
                self.builtin(origin, &server, &check.fidelity, "builtin_net", args! {});
                return;
            }
            Watch::Cpu | Watch::Memory | Watch::Disk | Watch::DiskIo => {
                self.builtin(origin, &server, &check.fidelity, "builtin", args! {});
                return;
            }
            Watch::Agent => {
                self.builtin(
                    origin,
                    &server,
                    &check.fidelity,
                    "offline_alert",
                    args! {"minutes" => OFFLINE_MINUTES},
                );
                return;
            }
            Watch::Service { name } => {
                let windows = server.os == Os::Windows;
                let (kind, key) = if windows {
                    ("windows_service", "services")
                } else {
                    ("systemd", "units")
                };
                let mut params = Map::new();
                params.insert(key.into(), json!(name));
                (kind, params, ("service".into(), name.clone()))
            }
            Watch::Port { address, port } if server.device.is_some() => {
                probe = true;
                let mut params = Map::new();
                params.insert("port".into(), json!(port.to_string()));
                if let Some(address) = address {
                    params.insert("host".into(), json!(address));
                }
                let text = match address {
                    Some(a) => format!("{a}_{port}"),
                    None => port.to_string(),
                };
                ("port", params, ("port".into(), text))
            }
            Watch::Port { address, port } => {
                // The server itself, however the source wrote it.
                let address = address.as_deref().filter(|a| {
                    !matches!(
                        a.trim().to_ascii_lowercase().as_str(),
                        "127.0.0.1" | "localhost" | "::1" | "[::1]" | ""
                    )
                });
                let mut params = Map::new();
                params.insert("port".into(), json!(port.to_string()));
                let host = address.unwrap_or("127.0.0.1").to_owned();
                let text = match address {
                    Some(a) => format!("{a}_{port}"),
                    None => port.to_string(),
                };
                params.insert("host".into(), json!(host));
                ("port", params, ("port".into(), text))
            }
            Watch::Log { path, pattern } => {
                let mut params = Map::new();
                params.insert("path".into(), json!(path));
                params.insert("pattern".into(), json!(pattern));
                if list == List::Exact {
                    list = List::Approximated;
                    why = note("log_dirs", args! {});
                }
                ("log", params, ("log".into(), path.clone()))
            }
            Watch::EventLog {
                channel,
                levels,
                providers,
                event_ids,
            } => {
                let mut params = Map::new();
                params.insert("channel".into(), json!(channel));
                let lists = [
                    ("levels", levels.join(",")),
                    ("providers", providers.join(",")),
                    (
                        "event_ids",
                        event_ids
                            .iter()
                            .map(u32::to_string)
                            .collect::<Vec<_>>()
                            .join(","),
                    ),
                ];
                for (key, list) in lists {
                    if !list.is_empty() {
                        params.insert(key.into(), json!(list));
                    }
                }
                ("eventlog", params, ("eventlog".into(), channel.clone()))
            }
            Watch::Web { url } => {
                let host = monitorflow_import::text::host_of(url);
                let Some(port) = monitorflow_import::text::port_of(url) else {
                    self.not_movable(
                        "check",
                        origin,
                        vec![server.name.clone()],
                        note("no_equivalent", args! {}),
                    );
                    return;
                };
                let mut params = Map::new();
                params.insert("port".into(), json!(port.to_string()));
                params.insert("host".into(), json!(host));
                list = List::Approximated;
                why = note("web_as_port", args! {"port" => port, "host" => host});
                ("port", params, ("port".into(), format!("{host}_{port}")))
            }
        };
        if !server.may_change {
            self.not_movable(
                "check",
                origin,
                vec![server.name.clone()],
                note("server_elsewhere", args! {}),
            );
            return;
        }
        let interval_ms = match check.interval_s.map(|s| s.saturating_mul(1000)) {
            None => DEFAULT_INTERVAL_MS,
            Some(ms) => {
                let clamped = u64::from(ms).clamp(MIN_CHECK_INTERVAL_MS, MAX_CHECK_INTERVAL_MS);
                if clamped != u64::from(ms) && list == List::Exact {
                    list = List::Approximated;
                    why = note("interval_clamped", args! {"seconds" => clamped / 1000});
                }
                clamped
            }
        };
        let lower = server.peer_id.to_ascii_lowercase();
        // The same check twice, from two items or a template and the host,
        // is one check: the first one, with an alert if either has one.
        let same = (
            lower.clone(),
            kind.to_owned(),
            Value::Object(params.clone()).to_string(),
        );
        if let Some(name) = self.checks.get(&same).cloned() {
            if let Some(alert) = &check.alert {
                self.alert(&server, kind, &name, alert, origin);
            }
            return;
        }
        let mut name = check_name(&prefix_text.0, &prefix_text.1);
        let base = name.clone();
        let mut n = 2;
        while !self.check_names.insert((lower.clone(), name.clone())) {
            name = format!("{base}_{n}");
            n += 1;
        }
        let present = self
            .existing
            .checks
            .contains(&(lower.clone(), name.clone()));
        let valid = sensors::validate_with_host(
            sensors::EntryRequest {
                kind: kind.into(),
                name: Some(name.clone()),
                instance: String::new(),
                interval_ms,
                timeout_ms: None,
                params: params.clone(),
                enabled: true,
                probe: None,
            },
            server.device.as_deref(),
        );
        if let Err(reason) = valid {
            self.not_movable(
                "check",
                origin,
                vec![server.name.clone()],
                note("refused", args! {"reason" => reason}),
            );
            return;
        }
        let id = self.unique_id(format!("check:{lower}:{name}"));
        self.push(Item {
            id: id.clone(),
            kind: "check".into(),
            list,
            note: why,
            origin: origin.clone(),
            hosts: vec![server.name.clone()],
            present,
            needs: Self::needs(&server),
            create: Some(Create::Check {
                server: server.peer_id.clone(),
                kind: kind.into(),
                name: name.clone(),
                interval_ms,
                params,
                probe,
            }),
        });
        self.checks.insert(same, name.clone());
        if let Some(alert) = &check.alert {
            self.alert(&server, kind, &name, alert, origin);
        }
    }

    /// The alert that comes with the check `name`, as the web interface
    /// adds it; once per check.
    fn alert(
        &mut self,
        server: &Server,
        kind: &str,
        name: &str,
        alert: &Alert,
        origin: OriginView,
    ) {
        // Of an SNMP device's groups, only `system` says whether it answers.
        if kind == "snmp" && name != "snmp" {
            return;
        }
        let lower = server.peer_id.to_ascii_lowercase();
        let id = format!("check:{lower}:{name}");
        let (suffix, comparison, value) = match kind {
            "port" | "ping" | "snmp" => ("up", "<", 1.0),
            "systemd" | "windows_service" => ("active", "<", 1.0),
            _ => ("matches", ">", 0.0),
        };
        let metric = format!("monitorflow_{name}_{suffix}");
        let (warning, critical) = match alert.severity {
            Severity::Warning => (Some(value), None),
            Severity::Critical => (None, Some(value)),
        };
        let key = format!("{lower}:{metric}");
        if let Some(&index) = self.alerts.get(&key) {
            // A second alert on the same check: the more severe one stays.
            if let Some(Item {
                create:
                    Some(Create::Threshold {
                        warning: w,
                        critical: c,
                        ..
                    }),
                ..
            }) = self.out.items.get_mut(index)
            {
                if critical.is_some() {
                    (*w, *c) = (None, critical);
                }
            }
            return;
        }
        let threshold_id = self.unique_id(format!("threshold:{lower}:{metric}"));
        self.alerts.insert(key, self.out.items.len());
        let mut needs = vec![id];
        needs.extend(Self::needs(server));
        let present = self
            .existing
            .thresholds
            .contains(&(lower.clone(), metric.clone(), None));
        self.push(Item {
            id: threshold_id,
            kind: "threshold".into(),
            list: List::Exact,
            note: None,
            origin,
            hosts: vec![server.name.clone()],
            present,
            needs,
            create: Some(Create::Threshold {
                server: server.peer_id.clone(),
                metric,
                instance: None,
                comparison: comparison.into(),
                warning,
                critical,
                duration_s: alert.for_s.min(thresholds::MAX_DURATION_S),
                hysteresis: 0.0,
            }),
        });
    }

    /// What every server does already: one line per source item, however
    /// many hosts it concerns.
    fn builtin(
        &mut self,
        origin: OriginView,
        server: &Server,
        fidelity: &Fidelity,
        code: &str,
        args: monitorflow_import::model::Args,
    ) {
        if let Some(item) = self
            .out
            .items
            .iter_mut()
            .find(|i| i.kind == "builtin" && i.origin == origin)
        {
            if !item.hosts.contains(&server.name) {
                item.hosts.push(server.name.clone());
            }
            return;
        }
        let (list, why) = match fidelity {
            Fidelity::Exact => (List::Exact, note(code, args)),
            Fidelity::Approximated(n) => (List::Approximated, Some(n.into())),
        };
        let id = self.unique_id(format!("builtin:{}:{}", origin.kind, origin.name));
        self.push(Item {
            id,
            kind: "builtin".into(),
            list,
            note: why,
            origin,
            hosts: vec![server.name.clone()],
            present: true,
            needs: vec![],
            create: None,
        });
    }

    fn thresholds(&mut self) {
        for threshold in &self.inventory.thresholds {
            let origin: OriginView = (&threshold.origin).into();
            let (metric, instance) = match &threshold.measure {
                Measure::CpuUsage => (
                    "monitorflow_cpu_usage_ratio".to_owned(),
                    Some("total".to_owned()),
                ),
                Measure::CpuLoad { minutes } => (format!("monitorflow_cpu_load{minutes}"), None),
                Measure::MemoryAvailable => ("monitorflow_memory_available_ratio".to_owned(), None),
                Measure::DiskUsed { mount } => {
                    ("monitorflow_disk_used_ratio".to_owned(), mount.clone())
                }
                Measure::DiskWait { device } => {
                    ("monitorflow_diskio_wait_seconds".to_owned(), device.clone())
                }
            };
            let comparison = match threshold.direction {
                Direction::Above => ">",
                Direction::Below => "<",
            };
            let (_, why) = list_of(&threshold.fidelity);
            let level = threshold.warning.or(threshold.critical);
            let hysteresis = match (threshold.recovery, level) {
                (Some(recovery), Some(level)) => (level - recovery).abs(),
                _ => 0.0,
            };
            for key in &threshold.hosts {
                let Some(server) = self.server_of(key) else {
                    self.not_movable(
                        "threshold",
                        origin.clone(),
                        vec![],
                        note("unknown_host", args! {}),
                    );
                    continue;
                };
                if !server.may_change {
                    self.not_movable(
                        "threshold",
                        origin.clone(),
                        vec![server.name.clone()],
                        note("server_elsewhere", args! {}),
                    );
                    continue;
                }
                let map_key = (
                    server.peer_id.to_ascii_lowercase(),
                    metric.clone(),
                    instance.clone(),
                );
                match self.thresholds.get_mut(&map_key) {
                    Some(merged) => {
                        merged.warning = merged.warning.or(threshold.warning);
                        merged.critical = merged.critical.or(threshold.critical);
                        merged.duration_s = merged.duration_s.min(threshold.for_s);
                        merged.hysteresis = merged.hysteresis.max(hysteresis);
                        if !merged.origins.contains(&origin) {
                            merged.origins.push(origin.clone());
                        }
                        if merged.note.is_none() {
                            merged.note.clone_from(&why);
                        }
                    }
                    None => {
                        self.thresholds.insert(
                            map_key,
                            Merged {
                                server,
                                comparison,
                                warning: threshold.warning,
                                critical: threshold.critical,
                                duration_s: threshold.for_s,
                                hysteresis,
                                origins: vec![origin.clone()],
                                note: why.clone(),
                            },
                        );
                    }
                }
            }
        }
        for ((lower, metric, instance), merged) in std::mem::take(&mut self.thresholds) {
            let mut origin = merged.origins[0].clone();
            if merged.origins.len() > 1 {
                origin.name = merged
                    .origins
                    .iter()
                    .map(|o| o.name.as_str())
                    .collect::<Vec<_>>()
                    .join(" + ");
                origin.detail = None;
            }
            let (mut warning, critical) = (merged.warning, merged.critical);
            // Critical must be past warning on the alarm side.
            if let (Some(w), Some(c)) = (warning, critical) {
                let above = merged.comparison == ">";
                if (above && c < w) || (!above && c > w) {
                    warning = None;
                }
            }
            let mut note = merged.note;
            let duration_s = merged.duration_s.min(thresholds::MAX_DURATION_S);
            if duration_s != merged.duration_s && note.is_none() {
                note = self::note("duration_rounded", args! {"seconds" => duration_s});
            }
            let request = thresholds::EntryRequest {
                metric: metric.clone(),
                instance: instance.clone(),
                comparison: merged.comparison.into(),
                warning,
                critical,
                duration_s,
                hysteresis: merged.hysteresis,
                enabled: true,
            };
            if let Err(reason) = thresholds::validate_entry(request) {
                self.not_movable(
                    "threshold",
                    origin,
                    vec![merged.server.name.clone()],
                    self::note("refused", args! {"reason" => reason}),
                );
                continue;
            }
            let present = self.existing.thresholds.contains(&(
                lower.clone(),
                metric.clone(),
                instance.clone(),
            ));
            let id = self.unique_id(format!(
                "threshold:{lower}:{metric}:{}",
                instance.as_deref().unwrap_or("*")
            ));
            self.push(Item {
                id,
                kind: "threshold".into(),
                list: if note.is_some() {
                    List::Approximated
                } else {
                    List::Exact
                },
                note,
                origin,
                hosts: vec![merged.server.name.clone()],
                present,
                needs: Self::needs(&merged.server),
                create: Some(Create::Threshold {
                    server: merged.server.peer_id.clone(),
                    metric,
                    instance,
                    comparison: merged.comparison.into(),
                    warning,
                    critical,
                    duration_s,
                    hysteresis: merged.hysteresis,
                }),
            });
        }
    }

    fn silences(&mut self) {
        for silence in &self.inventory.silences {
            let origin: OriginView = (&silence.origin).into();
            let (mut list, mut why) = list_of(&silence.fidelity);
            if silence.ends_at <= self.now {
                self.not_movable("silence", origin, vec![], note("silence_over", args! {}));
                continue;
            }
            // Whole seconds: the database keeps microseconds, and a second
            // import must find the same window.
            let starts_at = silence.starts_at.max(self.now).trunc_subsecs(0);
            let mut ends_at = silence.ends_at.trunc_subsecs(0);
            if ends_at - starts_at > TimeDelta::days(MAX_SILENCE_DAYS) {
                ends_at = starts_at + TimeDelta::days(MAX_SILENCE_DAYS);
                list = List::Approximated;
                why = note(
                    "silence_cut",
                    args! {"ends" => ends_at.format("%Y-%m-%d %H:%M UTC")},
                );
            }
            let mut reason: String = silence.reason.trim().chars().take(MAX_REASON).collect();
            if reason.is_empty() {
                reason = silence.origin.name.chars().take(MAX_REASON).collect();
            }
            if reason.trim().is_empty() {
                reason = "Maintenance".into();
            }
            let targets: Vec<Option<Server>> = match &silence.scope {
                SilenceScope::Everything => vec![None],
                SilenceScope::Hosts(keys) => keys
                    .iter()
                    .filter_map(|k| self.server_of(k))
                    .map(Some)
                    .collect(),
            };
            if targets.is_empty() {
                self.not_movable("silence", origin, vec![], note("no_hosts", args! {}));
                continue;
            }
            for target in targets {
                if target.as_ref().is_some_and(|s| !s.may_change) {
                    continue;
                }
                let lower = target.as_ref().map(|s| s.peer_id.to_ascii_lowercase());
                let present =
                    self.existing
                        .silences
                        .contains(&(lower.clone(), ends_at, reason.clone()));
                let id = self.unique_id(format!(
                    "silence:{}:{}",
                    lower.as_deref().unwrap_or("site"),
                    origin.name
                ));
                self.push(Item {
                    id,
                    kind: "silence".into(),
                    list,
                    note: why.clone(),
                    origin: origin.clone(),
                    hosts: target.iter().map(|s| s.name.clone()).collect(),
                    present,
                    needs: target.as_ref().map(Self::needs).unwrap_or_default(),
                    create: Some(Create::Silence {
                        server: target.map(|s| s.peer_id),
                        starts_at,
                        ends_at,
                        reason: reason.clone(),
                    }),
                });
            }
        }
    }

    fn channels(&mut self) {
        let site_id = (!self.channels_everywhere).then_some(self.site_id);
        for channel in &self.inventory.channels {
            let origin: OriginView = (&channel.origin).into();
            let (mut list, mut why) = list_of(&channel.fidelity);
            let mut settings = Map::new();
            let mut secret = ChannelSecret::default();
            let key = match &channel.target {
                Target::Email { to, smtp } => {
                    let Some(smtp) = smtp else {
                        self.not_movable("channel", origin, vec![], note("no_smtp", args! {}));
                        continue;
                    };
                    let mut to: Vec<String> = to
                        .iter()
                        .map(|t| t.trim().to_owned())
                        .filter(|t| !t.is_empty())
                        .collect();
                    to.sort();
                    to.dedup();
                    settings.insert("host".into(), json!(smtp.host));
                    settings.insert("port".into(), json!(smtp.port));
                    settings.insert("username".into(), json!(smtp.username));
                    settings.insert("from".into(), json!(smtp.from));
                    settings.insert("to".into(), json!(to));
                    secret.secret = smtp.password.as_ref().map(|p| p.expose().to_owned());
                    if !smtp.username.is_empty() && secret.secret.is_none() && list == List::Exact {
                        list = List::Approximated;
                        why = note("smtp_password", args! {});
                    }
                    channel_key("email", &format!("{}|{}", smtp.host, to.join(",")))
                }
                Target::Webhook { url } => {
                    let url = url.expose().trim().to_owned();
                    let origin_only = url
                        .split_once("://")
                        .map(|(s, rest)| {
                            format!(
                                "{s}://{}",
                                rest.split(['/', '?', '#']).next().unwrap_or_default()
                            )
                        })
                        .unwrap_or_default();
                    settings.insert("url".into(), json!(origin_only));
                    secret.url = Some(url.clone());
                    if list == List::Exact {
                        list = List::Approximated;
                        why = note("webhook_payload", args! {});
                    }
                    channel_key("webhook", &url)
                }
                Target::Slack { url } | Target::Teams { url } | Target::Discord { url } => {
                    let name: String = channel.name.trim().chars().take(80).collect();
                    settings.insert("name".into(), json!(name));
                    secret.secret = Some(url.expose().trim().to_owned());
                    channel_key(channel.target.kind(), url.expose())
                }
            };
            // What the channels page would refuse cannot be moved, such as
            // a retired chat URL: the preview says so, not the confirmation.
            let mut full = Value::Object(settings.clone());
            if let Some(url) = &secret.url {
                full["url"] = Value::String(url.clone());
            }
            if let Err(error) = crate::api::notifications::checked(
                channel.target.kind(),
                full,
                secret.secret.clone(),
                None,
            ) {
                let reason = error.to_string();
                self.not_movable(
                    "channel",
                    origin,
                    vec![],
                    note("refused", args! {"reason" => reason.trim_end_matches('.')}),
                );
                continue;
            }
            let present = self.existing.channels.contains(&key);
            let id = self.unique_id(format!(
                "channel:{}:{}",
                channel.target.kind(),
                channel.name
            ));
            let has_secret = secret.secret.is_some();
            if secret != ChannelSecret::default() {
                self.out.secrets.insert(id.clone(), secret);
            }
            self.push(Item {
                id,
                kind: "channel".into(),
                list,
                note: why,
                origin,
                hosts: vec![],
                present,
                needs: vec![],
                create: Some(Create::Channel {
                    kind: channel.target.kind().into(),
                    settings,
                    has_secret,
                    site_id,
                }),
            });
        }
    }

    fn unmovable(&mut self) {
        for item in &self.inventory.unmovable {
            let hosts = item
                .hosts
                .iter()
                .map(|k| {
                    self.inventory
                        .host(k)
                        .map_or_else(|| k.clone(), |h| h.name.clone())
                })
                .collect();
            let origin: OriginView = (&item.origin).into();
            // The same source item on many hosts is one line.
            let why: NoteView = (&item.why).into();
            if let Some(existing) = self.out.items.iter_mut().find(|i| {
                i.list == List::NotMovable && i.origin == origin && i.note.as_ref() == Some(&why)
            }) {
                for host in hosts {
                    if !existing.hosts.contains(&host) {
                        existing.hosts.push(host);
                    }
                }
                continue;
            }
            self.not_movable("other", origin, hosts, Some(why));
        }
    }
}

#[cfg(test)]
mod tests;
