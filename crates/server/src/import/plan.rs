//! From a reader's inventory to the items of a preview (ADR-0146).

use std::collections::{BTreeMap, HashMap, HashSet};

use chrono::{DateTime, SubsecRound, TimeDelta, Utc};
use chrono_tz::Tz;
use monitorflow_import::args;
use monitorflow_import::model::{
    Alert, AppKind, Check, Direction, Fidelity, Inventory, Judged, Measure, Os, Repeat, Rule,
    Severity, SilenceScope, Target, Watch, WindowFunction,
};
use monitorflow_import::Note;
use serde_json::{json, Map, Value};
use uuid::Uuid;

use super::{
    ChannelSecret, CheckRef, Create, Item, List, NoteView, OriginView, Richer, ServiceMember,
};
use crate::api::{sensors, thresholds};
use crate::enroll::install_key::peer_id_from_hostname;
use crate::notify::maintenance::{self, Schedule};
use crate::sensor_config::{MAX_CHECK_INTERVAL_MS, MIN_CHECK_INTERVAL_MS};

/// Longest name of an imported application check (ADR-0183).
const APP_MAX_NAME: usize = 32;
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
    /// A device watched from a probe (ADR-0148), not a server with an
    /// agent.
    pub device: bool,
}

/// What MonitorFlow has already, which the plan does not create again.
#[derive(Debug, Clone, Default)]
pub struct Existing {
    pub servers: Vec<ExistingServer>,
    /// Checks of servers by (lowercase peer ID, name), instance `""`.
    pub checks: HashSet<(String, String)>,
    /// Thresholds of servers by (lowercase peer ID, metric, instance,
    /// kind).
    pub thresholds: HashSet<(String, String, Option<String>, String)>,
    /// Silences not over by (lowercase peer ID, or `None` for the site),
    /// end and reason.
    pub silences: HashSet<(Option<String>, DateTime<Utc>, String)>,
    /// Channels by [`channel_key`].
    pub channels: HashSet<String>,
    /// Maintenance windows of the site not removed, by [`window_key`].
    pub windows: HashSet<String>,
    /// Network scans of the site, by lowercase name.
    pub scans: HashSet<String>,
    /// The site's default probe (ADR-0148): `central`, or the relay's
    /// name; `central` when empty.
    pub probe: String,
    /// Business services of the site's customer, by lowercase name.
    pub services: HashSet<String>,
}

/// What identifies a maintenance window of the import's site: its
/// servers (lowercase IDs), checks, rule and reason.
pub fn window_key(create: &Create) -> Option<String> {
    let Create::Window {
        servers,
        checks,
        repeat,
        every,
        weekdays,
        month_day,
        month_week,
        months,
        start_time,
        duration_minutes,
        time_zone,
        reason,
        ..
    } = create
    else {
        return None;
    };
    let mut servers: Vec<String> = servers.iter().map(|s| s.to_ascii_lowercase()).collect();
    servers.sort();
    let mut checks: Vec<String> = checks
        .iter()
        .map(|c| format!("{}/{}", c.name, c.instance))
        .collect();
    checks.sort();
    let mut months = months.clone();
    months.sort_unstable();
    if months.len() == 12 {
        months.clear();
    }
    Some(format!(
        "{}|{}|{repeat}|{every}|{}|{month_day:?}|{month_week:?}|{months:?}|{start_time}|{duration_minutes}|{time_zone}|{reason}",
        servers.join(","),
        checks.join(","),
        weekdays.join(","),
    ))
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
    plan_in(
        inventory,
        site_id,
        channels_everywhere,
        existing,
        now,
        Tz::UTC,
    )
}

/// [`plan`], with `zone` for maintenance windows whose source does not
/// say its time zone (ADR-0151).
pub fn plan_in(
    inventory: &Inventory,
    site_id: Uuid,
    channels_everywhere: bool,
    existing: &Existing,
    now: DateTime<Utc>,
    zone: Tz,
) -> Planned {
    let mut planner = Planner {
        inventory,
        site_id,
        zone,
        channels_everywhere,
        existing,
        now,
        out: Planned::default(),
        hosts: HashMap::new(),
        used_ids: HashSet::new(),
        check_names: HashSet::new(),
        checks: HashMap::new(),
        watch_names: HashMap::new(),
        alerts: HashMap::new(),
        thresholds: BTreeMap::new(),
        websites: HashMap::new(),
        web_rules: Vec::new(),
    };
    planner.hosts();
    for check in &inventory.checks {
        planner.check(check);
    }
    planner.thresholds();
    planner.silences();
    planner.windows();
    planner.scans();
    planner.channels();
    planner.services();
    planner.dependencies();
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

/// Thresholds merged by (server, metric, instance, kind).
type ThresholdKey = (String, String, Option<String>, String);

/// A metric every server has, with its instance.
type Fixed = Option<(String, Option<String>)>;

struct Merged {
    server: Server,
    /// What a richer rule judges (ADR-0153).
    richer: Richer,
    comparison: &'static str,
    warning: Option<f64>,
    critical: Option<f64>,
    duration_s: u32,
    hysteresis: f64,
    origins: Vec<OriginView>,
    note: Option<NoteView>,
    /// The item of the website check whose measure it compares.
    check: Option<String>,
}

/// An alert the web interface adds with a website check, planned once
/// the source's own thresholds are known: one of the source on the same
/// measure replaces it.
struct WebRule {
    server: Server,
    metric: String,
    rule: website::Rule,
    origin: OriginView,
    /// The check's item.
    check: String,
}

struct Planner<'a> {
    inventory: &'a Inventory,
    site_id: Uuid,
    /// For windows whose source does not say its time zone.
    zone: Tz,
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
    /// The name of the check each watch of a server became, by
    /// (lowercase peer ID, watch), for the silences that name it.
    watch_names: HashMap<(String, String), String>,
    /// Alert items planned, by metric of a server, to their index.
    alerts: HashMap<String, usize>,
    thresholds: BTreeMap<ThresholdKey, Merged>,
    /// Website checks by (lowercase peer ID, URL), to their name and item.
    websites: HashMap<(String, String), (String, String)>,
    web_rules: Vec<WebRule>,
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

const WEEKDAYS: [&str; 7] = [
    "monday",
    "tuesday",
    "wednesday",
    "thursday",
    "friday",
    "saturday",
    "sunday",
];

/// A reader's rule as MonitorFlow keeps it, in `zone`; starting today
/// when the source gives no first day.
fn schedule_of(rule: &Rule, zone: Tz, now: DateTime<Utc>) -> Schedule {
    Schedule {
        repeat: match rule.repeat {
            Repeat::Daily => maintenance::Repeat::Daily,
            Repeat::Weekly => maintenance::Repeat::Weekly,
            Repeat::Monthly => maintenance::Repeat::Monthly,
        },
        every: rule.every,
        weekdays: rule.weekdays,
        month_day: rule.month_day,
        month_week: rule.month_week,
        months: rule.months,
        start_time: rule.start_time,
        duration_minutes: rule.duration_minutes,
        time_zone: zone,
        // Today, or the last day when it is past, which the preview then
        // says is over.
        starts_on: rule.starts_on.unwrap_or_else(|| {
            let today = now.with_timezone(&zone).date_naive();
            rule.ends_on.map_or(today, |end| end.min(today))
        }),
        ends_on: rule.ends_on,
    }
}

/// What a watch becomes on a server of `os`.
enum Spec {
    /// A built-in check, by name.
    Builtin(&'static str),
    /// The offline alert.
    Agent,
    /// Nothing named here: a probe or SNMP check, which the plan names
    /// when it makes it.
    NoPort,
    /// A check of `kind` with `params`, named from `prefix`.
    Check {
        kind: &'static str,
        params: Map<String, Value>,
        prefix: (String, String),
    },
}

fn watch_spec(watch: &Watch, os: Os) -> Spec {
    let (kind, params, prefix) = match watch {
        Watch::Cpu => return Spec::Builtin("cpu"),
        Watch::Memory => return Spec::Builtin("memory"),
        Watch::Disk => return Spec::Builtin("disk"),
        Watch::DiskIo => return Spec::Builtin("diskio"),
        Watch::Agent => return Spec::Agent,
        // Probe and SNMP checks depend on the server; the plan names them
        // when it makes them.
        Watch::Ping { .. }
        | Watch::UdpPort { .. }
        | Watch::Snmp { .. }
        | Watch::SnmpCustom(_)
        | Watch::Traffic
        | Watch::Vmware
        | Watch::HyperV
        | Watch::App(_) => return Spec::NoPort,
        Watch::Service { name } => {
            let (kind, key) = if os == Os::Windows {
                ("windows_service", "services")
            } else {
                ("systemd", "units")
            };
            let mut params = Map::new();
            params.insert(key.into(), json!(name));
            (kind, params, ("service".into(), name.clone()))
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
        // A website check (ADR-0150), named as the plan names it.
        Watch::Web(site) => {
            let (params, _) = website::params(site);
            let label = website::label(site.url.trim());
            ("http", params, ("http".into(), label))
        }
    };
    Spec::Check {
        kind,
        params,
        prefix,
    }
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
            // What MonitorFlow has already decides: an imported host that
            // exists as a server with its agent stays one.
            let device = existing.map_or(host.agentless, |s| s.device);
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
                device: address.clone().filter(|_| device),
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
        // A custom SNMP check's lookups to make, and its channels' rules.
        let mut lookups = Vec::new();
        let mut snmp_custom = None;
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
            // Objects picked by OID (ADR-0157): the device's custom check.
            Watch::SnmpCustom(custom) => {
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
                if list == List::Exact {
                    list = List::Approximated;
                    why = note("snmp_credentials", args! {});
                }
                let (params, made) = snmp::params(custom);
                lookups = made;
                snmp_custom = Some(custom.as_ref());
                let text: String = check.origin.name.chars().take(snmp::MAX_NAME).collect();
                ("snmp_custom", params, ("snmp".into(), text))
            }
            Watch::Traffic => {
                self.builtin(origin, &server, &check.fidelity, "builtin_net", args! {});
                return;
            }
            // A vCenter or ESXi host is a device read from a probe; its
            // password comes after the import (ADR-0156).
            Watch::Vmware => {
                if server.device.is_none() {
                    self.not_movable(
                        "check",
                        origin,
                        vec![server.name.clone()],
                        note("vmware_of_agent", args! {}),
                    );
                    return;
                }
                probe = true;
                if list == List::Exact {
                    list = List::Approximated;
                    why = note("vmware_password", args! {});
                }
                ("vmware", Map::new(), ("vmware".into(), String::new()))
            }
            Watch::HyperV => {
                if server.device.is_some() || server.os != Os::Windows {
                    self.not_movable(
                        "check",
                        origin,
                        vec![server.name.clone()],
                        note("hyperv_windows", args! {}),
                    );
                    return;
                }
                if list == List::Exact {
                    list = List::Approximated;
                    why = note("hyperv_group", args! {});
                }
                ("hyperv", Map::new(), ("hyperv".into(), String::new()))
            }
            // A ready-made application check (ADR-0183): Docker by the
            // server's agent; a database or Redis by the agent when it is
            // on the server itself, else from a probe, as other network
            // checks; a Kubernetes cluster always from a probe.
            Watch::App(app) => {
                let kind = app.kind.as_str();
                let mut params = Map::new();
                if app.kind == AppKind::Docker {
                    if server.device.is_some() {
                        self.not_movable(
                            "check",
                            origin,
                            vec![server.name.clone()],
                            note("app_docker_agent", args! {}),
                        );
                        return;
                    }
                    if let Some(containers) = &app.containers {
                        params.insert("containers".into(), json!(containers));
                    }
                    let text = app.containers.as_deref().map_or(String::new(), |c| {
                        c.trim_start_matches('^').trim_end_matches('$').to_owned()
                    });
                    (kind, params, (kind.into(), text))
                } else {
                    let host = self.inventory.host(&check.host);
                    // The server itself, however the source wrote it.
                    let elsewhere = app.address.as_deref().map(str::trim).filter(|a| {
                        let own = matches!(
                            a.to_ascii_lowercase().as_str(),
                            "127.0.0.1" | "localhost" | "::1" | "[::1]" | ""
                        ) || host.is_some_and(|h| {
                            h.name.trim().eq_ignore_ascii_case(a)
                                || h.addresses.iter().any(|x| x.trim().eq_ignore_ascii_case(a))
                        }) || server.address.as_deref() == Some(*a);
                        !own
                    });
                    let kubernetes = app.kind == AppKind::Kubernetes;
                    let text = match (elsewhere, &server.device) {
                        // A cluster is read from a probe only, at the
                        // server's own address when it has an agent.
                        (None, None) if kubernetes => {
                            probe = true;
                            if let Some(address) = &server.address {
                                params.insert("host".into(), json!(address));
                            }
                            String::new()
                        }
                        // The agent, at 127.0.0.1 by default.
                        (None, None) => String::new(),
                        // The device's own address, which the check
                        // takes by default.
                        (None, Some(_)) => {
                            probe = true;
                            String::new()
                        }
                        (Some(address), _) => {
                            probe = true;
                            params.insert("host".into(), json!(address));
                            address.to_owned()
                        }
                    };
                    if let Some(port) = app.port {
                        params.insert("port".into(), json!(port.to_string()));
                    }
                    if let Some(database) = &app.database {
                        params.insert("database".into(), json!(database));
                    }
                    if let Some(user) = &app.user {
                        params.insert("user".into(), json!(user));
                    }
                    if let Some(path) = &app.path {
                        params.insert("path".into(), json!(path));
                    }
                    if app.https {
                        params.insert("tls".into(), json!("verify"));
                    }
                    // The password, or a cluster's token, is never
                    // exported: the person enters it.
                    if list == List::Exact {
                        list = List::Approximated;
                        why = note(
                            if kubernetes {
                                "app_kubernetes"
                            } else {
                                "app_password"
                            },
                            args! {},
                        );
                    }
                    (kind, params, (kind.into(), text))
                }
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
            Watch::Web(site) => {
                let (params, left) = website::params(site);
                // What the person must do (a password) comes first;
                // a header left out only when nothing else differs.
                let password = left
                    .as_ref()
                    .is_some_and(|n| n.code.starts_with("web_password") || n.code == "web_token");
                if left.is_some() && (password || list == List::Exact) {
                    list = List::Approximated;
                    why = left;
                }
                // A device has no agent: a probe runs it (ADR-0148).
                probe = server.device.is_some();
                let label = website::label(site.url.trim());
                ("http", params, ("http".into(), label))
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
        let website = match &check.watch {
            Watch::Web(site) => Some(site),
            _ => None,
        };
        // The time a run may take, within its interval.
        let timeout_ms = website
            .and_then(|site| site.timeout_s)
            .map(|s| (u64::from(s) * 1000).min(interval_ms));
        // A website check comes with its alert, as the web interface adds
        // it, with the source's severity and wait when it has some.
        // A no-data alert (ADR-0153) comes beside the check's own.
        let no_data = check.alert.clone().filter(|a| a.no_data);
        let alert = check.alert.clone().filter(|a| !a.no_data).or_else(|| {
            website.map(|_| Alert {
                severity: Severity::Critical,
                for_s: website::UP_FOR_S,
                no_data: false,
            })
        });
        let lower = server.peer_id.to_ascii_lowercase();
        // The same check twice, from two items or a template and the host,
        // is one check: the first one, with an alert if either has one.
        let same = (
            lower.clone(),
            kind.to_owned(),
            Value::Object(params.clone()).to_string(),
        );
        if let Some(name) = self.checks.get(&same).cloned() {
            self.watch_names
                .insert((lower.clone(), format!("{:?}", check.watch)), name.clone());
            if let Some(alert) = &no_data {
                self.alert(&server, kind, &name, alert, origin.clone());
            }
            if let Some(alert) = &alert {
                self.alert(&server, kind, &name, alert, origin);
            }
            return;
        }
        let mut name = check_name(&prefix_text.0, &prefix_text.1);
        // An application check's metrics end in suffixes of up to 31
        // characters, within the 64 a threshold's metric may have.
        let max_name = if monitorflow_apps::is_kind(kind) {
            Some(APP_MAX_NAME)
        } else {
            snmp_custom.is_some().then_some(snmp::MAX_NAME)
        };
        if let Some(max_name) = max_name {
            name = name
                .chars()
                .take(max_name)
                .collect::<String>()
                .trim_end_matches('_')
                .to_owned();
        }
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
                timeout_ms,
                params: params.clone(),
                enabled: true,
                probe: None,
                default_alerts: false,
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
                timeout_ms,
                params: params.clone(),
                probe,
                lookups,
            }),
        });
        if let Some(custom) = snmp_custom {
            self.snmp_rules(&server, &name, &params, custom, &id, origin.clone());
        }
        // The alerts of its template, as the API adds them with
        // `default_alerts` (ADR-0183).
        if let Some(template) = monitorflow_apps::templates::of(kind) {
            self.app_rules(&server, template, &name, &id, origin.clone());
        }
        self.checks.insert(same, name.clone());
        self.watch_names
            .insert((lower.clone(), format!("{:?}", check.watch)), name.clone());
        if let Some(site) = website {
            let url = site.url.trim().to_owned();
            for rule in website::rules(&url) {
                self.web_rules.push(WebRule {
                    server: server.clone(),
                    metric: format!("monitorflow_{name}_{}", rule.suffix),
                    rule,
                    origin: origin.clone(),
                    check: id.clone(),
                });
            }
            self.websites
                .entry((lower, url))
                .or_insert((name.clone(), id));
        }
        if let Some(alert) = &no_data {
            self.alert(&server, kind, &name, alert, origin.clone());
        }
        if let Some(alert) = &alert {
            self.alert(&server, kind, &name, alert, origin);
        }
    }

    /// The rules of a custom SNMP check's channels (ADR-0157): their
    /// limits, and their states' warning and critical.
    fn snmp_rules(
        &mut self,
        server: &Server,
        name: &str,
        params: &Map<String, Value>,
        custom: &monitorflow_import::model::SnmpCustom,
        check: &str,
        origin: OriginView,
    ) {
        let flat = crate::sensor_config::params(Value::Object(params.clone()));
        let Ok(parsed) = crate::snmp_custom::check_row(&flat, server.device.as_deref()) else {
            return;
        };
        let keys: Vec<String> = parsed.channels.iter().map(|c| c.key.clone()).collect();
        let lower = server.peer_id.to_ascii_lowercase();
        for (suffix, comparison, warning, critical) in snmp::rules(custom, &keys) {
            let metric = format!("monitorflow_{name}_{suffix}");
            let id = self.unique_id(format!("threshold:{lower}:{metric}"));
            let present = self.existing.thresholds.contains(&(
                lower.clone(),
                metric.clone(),
                None,
                "value".to_owned(),
            ));
            let mut needs = vec![check.to_owned()];
            needs.extend(Self::needs(server));
            self.push(Item {
                id,
                kind: "threshold".into(),
                list: List::Exact,
                note: None,
                origin: origin.clone(),
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
                    duration_s: 0,
                    hysteresis: 0.0,
                    richer: Richer::default(),
                }),
            });
        }
    }

    /// The default alerts of an application check `name` (ADR-0183), each
    /// one the source's own alert on the same metric replaces.
    fn app_rules(
        &mut self,
        server: &Server,
        template: &monitorflow_apps::templates::Template,
        name: &str,
        check: &str,
        origin: OriginView,
    ) {
        let lower = server.peer_id.to_ascii_lowercase();
        for rule in template.alerts {
            let metric = monitorflow_apps::templates::metric_name(name, rule.suffix);
            let key = format!("{lower}:{metric}:value");
            if self.alerts.contains_key(&key) {
                continue;
            }
            let id = self.unique_id(format!("threshold:{lower}:{metric}"));
            let present = self.existing.thresholds.contains(&(
                lower.clone(),
                metric.clone(),
                None,
                "value".to_owned(),
            ));
            let mut needs = vec![check.to_owned()];
            needs.extend(Self::needs(server));
            self.alerts.insert(key, self.out.items.len());
            self.push(Item {
                id,
                kind: "threshold".into(),
                list: List::Exact,
                note: None,
                origin: origin.clone(),
                hosts: vec![server.name.clone()],
                present,
                needs,
                create: Some(Create::Threshold {
                    server: server.peer_id.clone(),
                    metric,
                    instance: None,
                    comparison: rule.comparison.into(),
                    warning: rule.warning,
                    critical: rule.critical,
                    duration_s: u32::try_from(rule.minutes * 60)
                        .unwrap_or(0)
                        .min(thresholds::MAX_DURATION_S),
                    hysteresis: 0.0,
                    richer: Richer::default(),
                }),
            });
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
            "port" | "ping" | "snmp" | "http" => ("up", "<", 1.0),
            k if monitorflow_apps::is_kind(k) => ("up", "<", 1.0),
            "systemd" | "windows_service" => ("active", "<", 1.0),
            _ => ("matches", ">", 0.0),
        };
        let metric = format!("monitorflow_{name}_{suffix}");
        // No data from the check (ADR-0153): seconds of silence.
        let (comparison, value, richer) = if alert.no_data {
            let richer = Richer {
                kind: Some("nodata".into()),
                ..Richer::default()
            };
            (">", f64::from(alert.for_s.clamp(60, 604_800)), richer)
        } else {
            (comparison, value, Richer::default())
        };
        let (warning, critical) = match alert.severity {
            Severity::Warning => (Some(value), None),
            Severity::Critical => (None, Some(value)),
        };
        let key = format!("{lower}:{metric}:{}", richer.kind());
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
        let threshold_id = self.unique_id(if alert.no_data {
            format!("threshold:{lower}:{metric}:nodata")
        } else {
            format!("threshold:{lower}:{metric}")
        });
        self.alerts.insert(key, self.out.items.len());
        let mut needs = vec![id];
        needs.extend(Self::needs(server));
        let present = self.existing.thresholds.contains(&(
            lower.clone(),
            metric.clone(),
            None,
            richer.kind().to_owned(),
        ));
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
                duration_s: if alert.no_data {
                    0
                } else {
                    alert.for_s.min(thresholds::MAX_DURATION_S)
                },
                hysteresis: 0.0,
                richer,
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
            // A measure every server has, or one of a website check.
            let (fixed, site) = measure_metric(&threshold.measure);
            let (richer, comparison) = richer_of(&threshold.judged, threshold.direction);
            let mut conditions = Vec::new();
            let mut condition_failed = false;
            for c in &threshold.conditions {
                let (Some((metric, instance)), _) = measure_metric(&c.measure) else {
                    condition_failed = true;
                    continue;
                };
                conditions.push(thresholds::ThresholdCondition {
                    metric,
                    instance,
                    function: c.window.map_or("value", |(f, _)| function_name(f)).into(),
                    window_s: c.window.map(|(_, w)| w),
                    comparison: match c.direction {
                        Direction::Above => ">".into(),
                        Direction::Below => "<".into(),
                    },
                    value: c.value,
                });
            }
            if condition_failed {
                self.not_movable(
                    "threshold",
                    origin.clone(),
                    vec![],
                    note("expression", args! {}),
                );
                continue;
            }
            let richer = Richer {
                conditions,
                ..richer
            };
            let (_, why) = list_of(&threshold.fidelity);
            let level = threshold.warning.or(threshold.critical);
            let hysteresis = match (threshold.recovery, level, &threshold.judged) {
                (Some(recovery), Some(level), Judged::Value | Judged::Window { .. }) => {
                    (level - recovery).abs()
                }
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
                let lower = server.peer_id.to_ascii_lowercase();
                let (metric, instance, check) = match (&fixed, site) {
                    (Some((metric, instance)), _) => (metric.clone(), instance.clone(), None),
                    (None, Some((url, suffix))) => {
                        let found = self.websites.get(&(lower.clone(), url.trim().to_owned()));
                        let Some((name, id)) = found.cloned() else {
                            self.not_movable(
                                "threshold",
                                origin.clone(),
                                vec![server.name.clone()],
                                note("web_no_check", args! {}),
                            );
                            continue;
                        };
                        (format!("monitorflow_{name}_{suffix}"), None, Some(id))
                    }
                    (None, None) => continue,
                };
                let map_key = (lower, metric, instance, richer.kind().to_owned());
                match self.thresholds.get_mut(&map_key) {
                    // One rule of each kind per measure (ADR-0153): another
                    // window or forecast cannot join this one.
                    Some(merged) if merged.richer != richer => {
                        let kept = merged.origins[0].name.clone();
                        self.not_movable(
                            "threshold",
                            origin.clone(),
                            vec![server.name.clone()],
                            note("kind_taken", args! {"kept" => kept}),
                        );
                    }
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
                                richer: richer.clone(),
                                comparison,
                                warning: threshold.warning,
                                critical: threshold.critical,
                                duration_s: threshold.for_s,
                                hysteresis,
                                origins: vec![origin.clone()],
                                note: why.clone(),
                                check,
                            },
                        );
                    }
                }
            }
        }
        // The alerts a website check comes with, where the source has no
        // threshold of its own on the same measure.
        for web in std::mem::take(&mut self.web_rules) {
            let key = (
                web.server.peer_id.to_ascii_lowercase(),
                web.metric,
                None,
                "value".to_owned(),
            );
            self.thresholds.entry(key).or_insert(Merged {
                server: web.server,
                richer: Richer::default(),
                comparison: web.rule.comparison,
                warning: web.rule.warning,
                critical: web.rule.critical,
                duration_s: web.rule.duration_s,
                hysteresis: 0.0,
                origins: vec![web.origin],
                note: None,
                check: Some(web.check),
            });
        }
        for ((lower, metric, instance, kind), merged) in std::mem::take(&mut self.thresholds) {
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
            // Critical must be past warning on the alarm side: less time
            // left for a forecast, a longer silence for no data.
            if let (Some(w), Some(c)) = (warning, critical) {
                let above = match kind.as_str() {
                    "forecast" => false,
                    "nodata" => true,
                    _ => merged.comparison.starts_with('>'),
                };
                if (above && c < w) || (!above && c > w) {
                    warning = None;
                }
            }
            let mut note = merged.note;
            let duration_s = if kind == "nodata" {
                0
            } else {
                merged.duration_s.min(thresholds::MAX_DURATION_S)
            };
            if duration_s != merged.duration_s && note.is_none() {
                note = self::note("duration_rounded", args! {"seconds" => duration_s});
            }
            let request = thresholds::EntryRequest {
                metric: metric.clone(),
                instance: instance.clone(),
                comparison: Some(merged.comparison.into()),
                warning,
                critical,
                duration_s,
                hysteresis: merged.hysteresis,
                ..entry_fields(&merged.richer)
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
                kind.clone(),
            ));
            let suffix = if kind == "value" {
                String::new()
            } else {
                format!(":{kind}")
            };
            let id = self.unique_id(format!(
                "threshold:{lower}:{metric}:{}{suffix}",
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
                needs: merged
                    .check
                    .iter()
                    .cloned()
                    .chain(Self::needs(&merged.server))
                    .collect(),
                create: Some(Create::Threshold {
                    server: merged.server.peer_id.clone(),
                    metric,
                    instance,
                    comparison: merged.comparison.into(),
                    warning,
                    critical,
                    duration_s,
                    hysteresis: merged.hysteresis,
                    richer: merged.richer,
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
                let checks = self.check_refs(target.as_ref(), &silence.checks);
                if !silence.checks.is_empty() && checks.is_empty() {
                    self.not_movable(
                        "silence",
                        origin.clone(),
                        target.iter().map(|s| s.name.clone()).collect(),
                        note("no_equivalent", args! {}),
                    );
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
                        checks,
                    }),
                });
            }
        }
    }

    /// The checks of `server` (or of the site's servers) that `watches`
    /// are: built-in ones by name, others by the check planned or the
    /// name it would have. The offline alert is no check.
    fn check_refs(&self, server: Option<&Server>, watches: &[Watch]) -> Vec<CheckRef> {
        let mut refs: Vec<CheckRef> = Vec::new();
        for watch in watches {
            let planned = server.and_then(|s| {
                let key = (s.peer_id.to_ascii_lowercase(), format!("{watch:?}"));
                self.watch_names.get(&key).cloned()
            });
            let name = match planned {
                Some(name) => name,
                None => match watch_spec(watch, server.map_or(Os::Linux, |s| s.os)) {
                    Spec::Builtin(name) => name.to_owned(),
                    Spec::Agent | Spec::NoPort => continue,
                    Spec::Check {
                        kind,
                        params,
                        prefix,
                    } => server
                        .and_then(|s| {
                            let key = (
                                s.peer_id.to_ascii_lowercase(),
                                kind.to_owned(),
                                Value::Object(params).to_string(),
                            );
                            self.checks.get(&key).cloned()
                        })
                        .unwrap_or_else(|| check_name(&prefix.0, &prefix.1)),
                },
            };
            let check = CheckRef {
                name,
                instance: String::new(),
            };
            if !refs.contains(&check) && refs.len() < crate::notify::silence::MAX_CHECKS {
                refs.push(check);
            }
        }
        refs
    }

    fn windows(&mut self) {
        for window in &self.inventory.windows {
            let origin: OriginView = (&window.origin).into();
            let (list, why) = list_of(&window.fidelity);
            let servers: Vec<Server> = match &window.scope {
                SilenceScope::Everything => vec![],
                SilenceScope::Hosts(keys) => {
                    let found: Vec<Server> =
                        keys.iter().filter_map(|k| self.server_of(k)).collect();
                    if found.is_empty() {
                        self.not_movable("window", origin, vec![], note("no_hosts", args! {}));
                        continue;
                    }
                    found
                }
            };
            let servers: Vec<Server> = servers.into_iter().filter(|s| s.may_change).collect();
            if matches!(window.scope, SilenceScope::Hosts(_)) && servers.is_empty() {
                continue;
            }
            let checks = self.check_refs(servers.first(), &window.checks);
            if !window.checks.is_empty() && checks.is_empty() {
                self.not_movable("window", origin, vec![], note("no_equivalent", args! {}));
                continue;
            }
            let rule = &window.rule;
            let zone: Tz = match &rule.time_zone {
                None => self.zone,
                Some(name) => match name.parse() {
                    Ok(zone) => zone,
                    Err(_) => {
                        self.not_movable(
                            "window",
                            origin,
                            vec![],
                            note(
                                "refused",
                                args! {"reason" => format!("unknown time zone {name}")},
                            ),
                        );
                        continue;
                    }
                },
            };
            let schedule = schedule_of(rule, zone, self.now);
            if let Err(reason) = schedule.validate() {
                self.not_movable(
                    "window",
                    origin,
                    vec![],
                    note("refused", args! {"reason" => reason}),
                );
                continue;
            }
            if schedule.next(self.now, 1).is_empty() {
                self.not_movable("window", origin, vec![], note("silence_over", args! {}));
                continue;
            }
            let mut reason: String = window.reason.trim().chars().take(MAX_REASON).collect();
            if reason.is_empty() {
                reason = "Maintenance".into();
            }
            let bits = |value: u16, names: &[&str]| -> Vec<String> {
                names
                    .iter()
                    .enumerate()
                    .filter(|(i, _)| value & (1 << i) != 0)
                    .map(|(_, n)| (*n).to_owned())
                    .collect()
            };
            let create = Create::Window {
                servers: servers.iter().map(|s| s.peer_id.clone()).collect(),
                checks,
                repeat: schedule.repeat.as_str().to_owned(),
                every: schedule.every,
                weekdays: bits(u16::from(schedule.weekdays), &WEEKDAYS),
                month_day: schedule.month_day,
                month_week: schedule.month_week,
                months: if schedule.months == maintenance::EVERY_MONTH {
                    vec![]
                } else {
                    (1..=12u8)
                        .filter(|m| schedule.months & (1 << (m - 1)) != 0)
                        .collect()
                },
                start_time: schedule.start_time.format("%H:%M").to_string(),
                duration_minutes: schedule.duration_minutes,
                time_zone: zone.name().to_owned(),
                starts_on: rule.starts_on,
                ends_on: rule.ends_on,
                reason,
            };
            let present = window_key(&create).is_some_and(|k| self.existing.windows.contains(&k));
            let id = self.unique_id(format!("window:{}", origin.name));
            let mut needs: Vec<String> = servers.iter().flat_map(Self::needs).collect();
            needs.dedup();
            self.push(Item {
                id,
                kind: "window".into(),
                list,
                note: why,
                origin,
                hosts: servers.iter().map(|s| s.name.clone()).collect(),
                present,
                needs,
                create: Some(create),
            });
        }
    }

    fn scans(&mut self) {
        use crate::api::network_scans::MAX_NAME;
        use crate::netscan::targets;
        for scan in &self.inventory.scans {
            let origin: OriginView = (&scan.origin).into();
            let (mut list, mut why) = list_of(&scan.fidelity);
            let name: String = scan
                .name
                .chars()
                .filter(|c| !c.is_control())
                .take(MAX_NAME)
                .collect::<String>()
                .trim()
                .to_owned();
            let name = if name.is_empty() {
                origin.name.chars().take(MAX_NAME).collect()
            } else {
                name
            };
            // The same checks as the network scans API.
            let checked = targets::expand(&scan.targets)
                .and_then(|(t, ips)| Ok((t, ips, targets::ports(scan.ports.as_deref())?)));
            let (entries, addresses, ports) = match checked {
                Ok(checked) => checked,
                Err(reason) => {
                    let reason = reason.replace('`', "");
                    self.not_movable(
                        "network_scan",
                        origin,
                        vec![],
                        note("refused", args! {"reason" => reason}),
                    );
                    continue;
                }
            };
            let mut secret = None;
            if let Some(snmp) = &scan.snmp {
                let request = json!({
                    "version": snmp.version,
                    "port": snmp.port,
                    "community": snmp.community.as_ref().map(|s| s.expose()),
                    "user": snmp.user,
                    "auth_protocol": snmp.auth_protocol,
                    "auth_password": snmp.auth_password.as_ref().map(|s| s.expose()),
                    "priv_protocol": snmp.priv_protocol,
                    "priv_password": snmp.priv_password.as_ref().map(|s| s.expose()),
                });
                let checked =
                    serde_json::from_value::<crate::snmp::CredentialsRequest>(request.clone())
                        .map_err(|e| e.to_string())
                        .and_then(|r| crate::snmp::check(&r, None));
                match checked {
                    Ok(_) => secret = Some(request.to_string()),
                    Err(reason) if list == List::Exact => {
                        list = List::Approximated;
                        let reason = reason.replace('`', "");
                        why = note("scan_snmp_refused", args! {"reason" => reason});
                    }
                    Err(_) => {}
                }
            }
            let present = self.existing.scans.contains(&name.to_lowercase());
            let id = self.unique_id(format!("network_scan:{name}"));
            let has_snmp = secret.is_some();
            if let Some(secret) = secret {
                self.out.secrets.insert(
                    id.clone(),
                    ChannelSecret {
                        url: None,
                        secret: Some(secret),
                    },
                );
            }
            let probe = match self.existing.probe.as_str() {
                "" => monitorflow_probe::CENTRAL.to_owned(),
                p => p.to_owned(),
            };
            self.push(Item {
                id,
                kind: "network_scan".into(),
                list,
                note: why,
                origin,
                hosts: vec![],
                present,
                needs: vec![],
                create: Some(Create::NetworkScan {
                    name,
                    targets: entries,
                    addresses: u32::try_from(addresses.len()).unwrap_or(u32::MAX),
                    ports,
                    every: scan.every.map(|e| e.as_str().to_owned()),
                    probe,
                    has_snmp,
                }),
            });
        }
    }

    /// Business services (ADR-0155), of the servers the import has or
    /// makes; a member whose check has no equivalent counts as its server.
    fn services(&mut self) {
        for service in &self.inventory.services {
            let origin: OriginView = (&service.origin).into();
            let (list, why) = list_of(&service.fidelity);
            let mut members: Vec<ServiceMember> = Vec::new();
            let mut servers: Vec<Server> = Vec::new();
            for member in &service.members {
                // A server of a site the person does not administer would
                // tell its state through the service.
                let Some(server) = self
                    .server_of(&member.host)
                    .filter(|server| server.may_change)
                else {
                    continue;
                };
                let check = member.check.as_ref().and_then(|watch| {
                    self.check_refs(Some(&server), std::slice::from_ref(watch))
                        .pop()
                });
                let member = ServiceMember {
                    server: server.peer_id.clone(),
                    check,
                };
                if !members.contains(&member) {
                    members.push(member);
                }
                if !servers.iter().any(|s| s.peer_id == server.peer_id) {
                    servers.push(server);
                }
            }
            if members.is_empty() {
                self.not_movable("service", origin, vec![], note("no_hosts", args! {}));
                continue;
            }
            let count = u32::try_from(members.len()).unwrap_or(u32::MAX);
            let name: String = service.name.trim().chars().take(80).collect();
            let present = self.existing.services.contains(&name.to_lowercase());
            let id = self.unique_id(format!("service:{name}"));
            let mut needs: Vec<String> = servers.iter().flat_map(Self::needs).collect();
            needs.sort();
            needs.dedup();
            self.push(Item {
                id,
                kind: "service".into(),
                list,
                note: why,
                origin,
                hosts: servers.iter().map(|s| s.name.clone()).collect(),
                present,
                needs,
                create: Some(Create::Service {
                    name,
                    at_least: service
                        .at_least
                        .map(|n| n.clamp(1, count))
                        .filter(|&n| n < count),
                    target: service.target.filter(|t| (50.0..=100.0).contains(t)),
                    members,
                }),
            });
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

    /// Dependencies (ADR-0152), once every server and check is planned:
    /// each end is a server or the check its watch became.
    fn dependencies(&mut self) {
        for dependency in &self.inventory.dependencies {
            let origin: OriginView = (&dependency.origin).into();
            // A host that is not moved is listed already.
            let (Some(child), Some(parent)) = (
                self.server_of(&dependency.child.host),
                self.server_of(&dependency.parent.host),
            ) else {
                continue;
            };
            // Either end in a site the person does not administer would
            // hold that server's alerts, or tell its state.
            if !child.may_change || !parent.may_change {
                self.not_movable(
                    "dependency",
                    origin,
                    vec![child.name.clone(), parent.name.clone()],
                    note("server_elsewhere", args! {}),
                );
                continue;
            }
            let end = |server: &Server, watch: &Option<Watch>| match watch {
                None => Some(None),
                Some(watch) => self
                    .check_refs(Some(server), std::slice::from_ref(watch))
                    .into_iter()
                    .next()
                    .map(Some),
            };
            let (Some(check), Some(parent_check)) = (
                end(&child, &dependency.child.watch),
                end(&parent, &dependency.parent.watch),
            ) else {
                self.not_movable(
                    "dependency",
                    origin,
                    vec![child.name.clone(), parent.name.clone()],
                    note("no_equivalent", args! {}),
                );
                continue;
            };
            let (list, why) = list_of(&dependency.fidelity);
            let part = |server: &Server, check: &Option<CheckRef>| {
                let lower = server.peer_id.to_ascii_lowercase();
                match check {
                    Some(c) => format!("{lower}/{}", c.name),
                    None => lower,
                }
            };
            let id = self.unique_id(format!(
                "dependency:{}:{}",
                part(&child, &check),
                part(&parent, &parent_check)
            ));
            let mut needs: Vec<String> = Self::needs(&child)
                .into_iter()
                .chain(Self::needs(&parent))
                .collect();
            for (server, check) in [(&child, &check), (&parent, &parent_check)] {
                if let Some(check) = check {
                    let item = format!(
                        "check:{}:{}",
                        server.peer_id.to_ascii_lowercase(),
                        check.name
                    );
                    if self.out.items.iter().any(|i| i.id == item) {
                        needs.push(item);
                    }
                }
            }
            needs.dedup();
            self.push(Item {
                id,
                kind: "dependency".into(),
                list,
                note: why,
                origin,
                hosts: vec![child.name.clone(), parent.name.clone()],
                present: false,
                needs,
                create: Some(Create::Dependency {
                    server: child.peer_id.clone(),
                    check,
                    parent: parent.peer_id.clone(),
                    parent_check,
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

mod snmp;
mod website;

/// The metric and instance of a measure every server has, or the website
/// check URL and metric suffix of one of a website's.
fn measure_metric(measure: &Measure) -> (Fixed, Option<(&str, &'static str)>) {
    match measure {
        Measure::CpuUsage => (
            Some((
                "monitorflow_cpu_usage_ratio".to_owned(),
                Some("total".to_owned()),
            )),
            None,
        ),
        Measure::CpuLoad { minutes } => {
            (Some((format!("monitorflow_cpu_load{minutes}"), None)), None)
        }
        Measure::MemoryAvailable => (
            Some(("monitorflow_memory_available_ratio".to_owned(), None)),
            None,
        ),
        Measure::DiskUsed { mount } => (
            Some(("monitorflow_disk_used_ratio".to_owned(), mount.clone())),
            None,
        ),
        Measure::DiskWait { device } => (
            Some(("monitorflow_diskio_wait_seconds".to_owned(), device.clone())),
            None,
        ),
        Measure::WebResponseTime { url } => (None, Some((url, "response_seconds"))),
        Measure::CertificateDays { url } => (None, Some((url, "cert_days_left"))),
        Measure::DatastoreFree => (
            Some(("monitorflow_vmware_datastore_free_ratio".to_owned(), None)),
            None,
        ),
    }
}

fn function_name(function: WindowFunction) -> &'static str {
    match function {
        WindowFunction::Avg => "avg",
        WindowFunction::Min => "min",
        WindowFunction::Max => "max",
        WindowFunction::Change => "change",
        WindowFunction::Rate => "rate",
    }
}

/// The fields of a richer threshold (ADR-0153) and its comparison: for a
/// forecast the side of its target, for no data `>`.
fn richer_of(judged: &Judged, direction: Direction) -> (Richer, &'static str) {
    let side = match direction {
        Direction::Above => ">",
        Direction::Below => "<",
    };
    let kind = |k: &str| Some(k.to_owned());
    match *judged {
        Judged::Value => (Richer::default(), side),
        Judged::Window { function, window_s } => (
            Richer {
                kind: kind("window"),
                function: Some(function_name(function).into()),
                window_s: Some(window_s),
                ..Richer::default()
            },
            side,
        ),
        Judged::Compare {
            ratio,
            offset_s,
            window_s,
        } => (
            Richer {
                kind: kind("compare"),
                function: Some(if ratio { "ratio" } else { "difference" }.into()),
                window_s: Some(window_s),
                offset_s: Some(offset_s),
                ..Richer::default()
            },
            side,
        ),
        Judged::Forecast { target, window_s } => (
            Richer {
                kind: kind("forecast"),
                window_s: Some(window_s),
                target: Some(target),
                ..Richer::default()
            },
            if direction == Direction::Above {
                ">="
            } else {
                "<="
            },
        ),
        Judged::NoData => (
            Richer {
                kind: kind("nodata"),
                ..Richer::default()
            },
            ">",
        ),
    }
}

/// The threshold request fields of `richer`, everything else default.
pub(super) fn entry_fields(richer: &Richer) -> thresholds::EntryRequest {
    thresholds::EntryRequest {
        kind: richer.kind.clone(),
        function: richer.function.clone(),
        window_s: richer.window_s,
        offset_s: richer.offset_s,
        target: richer.target,
        conditions: richer.conditions.clone(),
        ..thresholds::EntryRequest::default()
    }
}

#[cfg(test)]
mod tests;
