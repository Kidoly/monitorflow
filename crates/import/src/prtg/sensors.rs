//! One PRTG sensor to what MonitorFlow watches and its thresholds.
//!
//! PRTG measures from a probe, over the network; MonitorFlow measures on
//! the server, with its agent. A sensor that reads the machine (processor,
//! memory, disks, services, event log) moves as it is; one that looks at
//! it from outside (ping, ports, pages, certificates) becomes the nearest
//! thing the agent checks, with a note.

use crate::args;
use crate::model::{
    Alert, App, AppKind, Direction, Fidelity, Measure, Severity, Watch, WebMethod, Website,
};
use crate::notes::Note;

use super::tree::{Channel, Limits, Object};

/// The families of sensor types this reader knows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Family {
    Ping,
    Cpu,
    Memory,
    Disk,
    Service,
    Port,
    Web,
    Certificate,
    EventLog,
    Traffic,
    /// A DNS server answering (ADR-0148).
    Dns,
    /// An NTP server answering.
    Ntp,
    /// Another SNMP sensor: uptime, UPS, printer, temperature.
    Snmp,
    /// A custom SNMP sensor: of one OID, several, a text, a lookup or a
    /// table (ADR-0157).
    SnmpCustom,
    /// A VMware sensor over SOAP or WBEM: the device is a vCenter or an
    /// ESXi host (ADR-0156).
    Vmware,
    /// A Hyper-V sensor of a Windows host.
    HyperV,
    /// A database, Redis or Docker sensor: a ready-made application
    /// check (ADR-0183).
    App(AppKind),
    /// A database MonitorFlow has no ready-made check for: Microsoft SQL,
    /// Oracle, ADO.
    OtherDatabase,
    /// A REST Custom sensor: the Kubernetes check when its URL is a
    /// cluster's API, nothing otherwise.
    Rest,
    /// NetFlow, jFlow, sFlow or IPFIX received from the device.
    Flow,
    Other,
}

/// The family of a sensor type, like `wmiservice` or `snmptraffic`.
pub fn family(sensor_type: &str) -> Family {
    let t = sensor_type.to_ascii_lowercase();
    let has = |s: &str| t.contains(s);
    // By words, as raw types and the names PRTG shows both carry them.
    if has("vmware") || has("vcenter") || has("esx") {
        return Family::Vmware;
    }
    if has("hyperv") || has("hyper-v") {
        return Family::HyperV;
    }
    // `postgresql`, `mysqlv2`, `mysql`, `dockercontainerstatus`, and the
    // names PRTG shows: "PostgreSQL", "MySQL v2", "Docker Container
    // Status".
    if has("postgres") {
        return Family::App(AppKind::Postgresql);
    }
    if has("mysql") || has("mariadb") {
        return Family::App(AppKind::Mysql);
    }
    if has("redis") {
        return Family::App(AppKind::Redis);
    }
    if has("docker") {
        return Family::App(AppKind::Docker);
    }
    // PRTG has no Kubernetes sensor of its own: a custom one named so, or
    // one whose URL is a cluster's API (below).
    if has("kubernetes") || has("k8s") {
        return Family::App(AppKind::Kubernetes);
    }
    if t == "restcustom" || t.starts_with("paessler.rest.custom") {
        return Family::Rest;
    }
    // `mssqlv2`, `oraclev2`, `adosqlv2`, "Microsoft SQL v2", "Oracle
    // Tablespace".
    if has("sql") || has("oracle") {
        return Family::OtherDatabase;
    }
    if t.starts_with("snmpcustom") || t == "snmplibrary" || t.starts_with("paessler.snmp.custom") {
        return Family::SnmpCustom;
    }
    // Newer sensors are typed like `paessler.dns.dns_v2.40`: by the name
    // in third place.
    let t = match t.strip_prefix("paessler.") {
        Some(rest) => rest.split('.').nth(1).unwrap_or(rest).to_owned(),
        None => t,
    };
    let has = |s: &str| t.contains(s);
    if has("netflow") || has("jflow") || has("sflow") || has("ipfix") {
        // Before `traffic`: PRTG's custom flow sensors are named so too.
        Family::Flow
    } else if t.starts_with("ping") {
        Family::Ping
    } else if t.starts_with("dns") {
        Family::Dns
    } else if t == "sntp" || t == "ntp" {
        Family::Ntp
    } else if has("traffic") {
        Family::Traffic
    } else if t == "sslcertificate" {
        Family::Certificate
    } else if matches!(t.as_str(), "http" | "httpadvanced" | "httpfull") {
        Family::Web
    } else if t == "port" || protocol(&t).is_some() {
        Family::Port
    } else if has("eventlog") || has("logfile") {
        Family::EventLog
    } else if has("service") && (has("wmi") || has("win") || t == "service") {
        Family::Service
    } else if has("cpu") || has("processor") {
        Family::Cpu
    } else if has("memory") || has("meminfo") {
        Family::Memory
    } else if has("diskspace") || has("diskfree") || has("volume") {
        Family::Disk
    } else if t.starts_with("snmp") {
        Family::Snmp
    } else {
        Family::Other
    }
}

/// The settings this reader reads of a sensor type, and whether it reads
/// its channels, so neither the API nor the file is asked for more.
pub fn wanted(sensor_type: &str) -> (&'static [&'static str], bool) {
    match family(sensor_type) {
        Family::Port => (&["port"], true),
        Family::Web => (WEB_SETTINGS, true),
        Family::Certificate => (&["port", "sniname"], true),
        Family::Service => (&["service", "servicename"], false),
        Family::EventLog => (
            &[
                "logfile",
                "wmilogfile",
                "eventlog",
                "eventtype",
                "filtertype",
                "eventsource",
                "filtersource",
                "eventid",
                "filterid",
                "message",
                "filtermessage",
            ],
            true,
        ),
        Family::Cpu | Family::Memory | Family::Disk => (&[], true),
        Family::Dns | Family::Ntp | Family::Snmp | Family::HyperV => (&[], false),
        Family::SnmpCustom => (super::snmp::settings(), true),
        Family::Vmware => (&[], true),
        Family::App(AppKind::Docker) => (DOCKER_SETTINGS, true),
        Family::App(AppKind::Kubernetes) | Family::Rest => (REST_SETTINGS, false),
        Family::App(_) => (&["port", "database"], true),
        Family::Ping | Family::Traffic | Family::Flow | Family::OtherDatabase | Family::Other => {
            (&[], false)
        }
    }
}

/// A limit of one channel, read as a MonitorFlow threshold.
#[derive(Debug, Clone, PartialEq)]
pub struct Limit {
    pub measure: Measure,
    pub direction: Direction,
    pub warning: Option<f64>,
    pub critical: Option<f64>,
    pub fidelity: Fidelity,
    /// The channel, for the origin's detail.
    pub channel: String,
}

/// What one sensor becomes.
#[derive(Debug, Clone, PartialEq)]
pub enum Mapped {
    /// A watch, its limits, and the channels whose limits move nowhere.
    Watch {
        watch: Watch,
        alert: Option<Alert>,
        fidelity: Fidelity,
        limits: Vec<Limit>,
        lost: Vec<String>,
    },
    /// Nothing, with why.
    Not(Note),
}

/// The channels with limits on.
fn limited(sensor: &Object) -> impl Iterator<Item = (&Channel, &Limits)> {
    sensor
        .channels
        .iter()
        .filter_map(|c| c.limits.as_ref().filter(|l| !l.is_empty()).map(|l| (c, l)))
}

fn is_percent(channel: &Channel) -> bool {
    channel.unit.trim() == "%"
        || channel.name.contains('%')
        || channel.name.to_ascii_lowercase().contains("percent")
}

/// A Windows drive (`C:\`) or a Unix mount point (`/var`) named in a
/// channel or sensor name.
pub fn disk_mount(text: &str) -> Option<String> {
    let bytes = text.as_bytes();
    for i in 0..bytes.len().saturating_sub(1) {
        let boundary = i == 0 || !bytes[i - 1].is_ascii_alphanumeric();
        let after = bytes.get(i + 2).is_none_or(|b| !b.is_ascii_alphanumeric());
        if boundary && after && bytes[i].is_ascii_alphabetic() && bytes[i + 1] == b':' {
            return Some(format!("{}:\\", (bytes[i] as char).to_ascii_uppercase()));
        }
    }
    text.split_whitespace()
        .find(|w| w.starts_with('/'))
        .map(|w| w.trim_end_matches([',', ')', ']', ':']).to_owned())
}

fn exact() -> Fidelity {
    Fidelity::Exact
}

fn approximated(code: &str, args: crate::model::Args) -> Fidelity {
    Fidelity::Approximated(Note::new(code, args))
}

/// The alert of a check that passes or fails: critical at once, as PRTG
/// turns a sensor down at the first failed scan.
fn down() -> Option<Alert> {
    Some(Alert {
        severity: Severity::Critical,
        for_s: 0,
        no_data: false,
    })
}

/// Maps `sensor`, on a device whose address is `host`.
pub fn map(sensor: &Object, host: Option<&str>) -> Mapped {
    let sensor_type = sensor.sensor_type.as_deref().unwrap_or("");
    match family(sensor_type) {
        // Pinged from a probe of PRTG, from a probe of MonitorFlow (ADR-0148).
        Family::Ping => probe_watch(Watch::Ping { address: None }, exact(), sensor),
        Family::Dns => probe_watch(
            Watch::UdpPort {
                address: None,
                port: 53,
                query: Some("dns".into()),
            },
            approximated("dns_as_udp", args! {}),
            sensor,
        ),
        Family::Ntp => probe_watch(
            Watch::UdpPort {
                address: None,
                port: 123,
                query: Some("ntp".into()),
            },
            exact(),
            sensor,
        ),
        Family::Snmp => {
            let t = sensor_type.to_ascii_lowercase();
            let has = |words: &[&str]| words.iter().any(|w| t.contains(w));
            // A vendor's system health (Dell, HP, Cisco, NetApp: fans,
            // power supplies, disks) is read from its own MIB, not from
            // the system group, which only gives the uptime.
            let group = if has(&["health"]) {
                return Mapped::Not(Note::plain("snmp_object"));
            } else if has(&["uptime", "system"]) {
                "system"
            } else if has(&["ups"]) {
                "ups"
            } else if has(&["printer", "toner"]) {
                "printer"
            } else if has(&["temp"]) {
                "temperature"
            } else {
                return Mapped::Not(Note::plain("snmp_object"));
            };
            probe_watch(
                Watch::Snmp {
                    group: group.into(),
                },
                exact(),
                sensor,
            )
        }
        Family::SnmpCustom => super::snmp::map(sensor, sensor_type),
        Family::Cpu => cpu(sensor),
        Family::Memory => memory(sensor),
        Family::Disk => disk(sensor),
        Family::Service => service(sensor),
        Family::Port => port(sensor, host),
        Family::Web => match kubernetes_api(sensor, host) {
            Some(app) => kubernetes(sensor, app),
            None => web(sensor, host),
        },
        Family::Rest => match kubernetes_api(sensor, host) {
            Some(app) => kubernetes(sensor, app),
            None => Mapped::Not(Note::new("prtg_sensor_type", args! {"type" => sensor_type})),
        },
        Family::Certificate => certificate(sensor, host),
        Family::EventLog => event_log(sensor),
        // A server's own `net` check, a device's SNMP interfaces (ADR-0149).
        Family::Traffic => builtin(Watch::Traffic, sensor, Vec::new()),
        Family::Vmware => vmware(sensor, sensor_type),
        Family::HyperV => {
            let t = sensor_type.to_ascii_lowercase();
            let note = if t.contains("storage") || t.contains("network") || t.contains("adapter") {
                "virtual_partial"
            } else {
                "hyperv_group"
            };
            Mapped::Watch {
                watch: Watch::HyperV,
                alert: None,
                fidelity: approximated(note, args! {}),
                limits: Vec::new(),
                lost: lost(sensor, &[]),
            }
        }
        Family::App(AppKind::Kubernetes) => {
            let mut app =
                kubernetes_api(sensor, host).unwrap_or_else(|| App::new(AppKind::Kubernetes));
            if app.address.is_none() {
                app.address = host.map(str::to_owned);
            }
            kubernetes(sensor, app)
        }
        Family::App(kind) => app(sensor, kind, host),
        Family::OtherDatabase => Mapped::Not(Note::plain("app_other_database")),
        // Flows are not a check: the device's Flows tab turns their
        // reception on, once per device, from the probe it sends to
        // (ADR-0184).
        Family::Flow => Mapped::Not(Note::plain("flow_sensor")),
        Family::Other => Mapped::Not(Note::new("prtg_sensor_type", args! {"type" => sensor_type})),
    }
}

/// The settings holding the URL a REST or Kubernetes sensor reads.
const REST_SETTINGS: &[&str] = &["url", "httpurl", "requesturl"];

/// The API of a Kubernetes cluster a sensor reads, by its URL: a path of
/// the API (`/api/v1/...`, `/apis/...`), or a health or version path on
/// the API server's port 6443. A path alone is on the device.
fn kubernetes_api(sensor: &Object, host: Option<&str>) -> Option<App> {
    let url = REST_SETTINGS
        .iter()
        .find_map(|name| sensor.setting(name))
        .map(str::trim)
        .filter(|u| !u.is_empty())?;
    let (address, port, path) = match url.split_once("://") {
        Some((_, rest)) => {
            let path = rest.find('/').map_or("/", |i| &rest[i..]);
            (
                Some(crate::text::host_of(url)).filter(|h| !h.is_empty()),
                crate::text::port_of(url),
                path,
            )
        }
        None => (host.map(str::to_owned), None, url),
    };
    let api = path.starts_with("/api/v1/") || path.starts_with("/apis/");
    let health = ["/healthz", "/livez", "/readyz", "/version"]
        .iter()
        .any(|p| path.starts_with(p));
    if !(api || (health && port == Some(crate::model::KUBERNETES_PORT))) {
        return None;
    }
    let mut app = App::new(AppKind::Kubernetes);
    app.address = address;
    app.port = port.filter(|p| *p != crate::model::KUBERNETES_PORT);
    Some(app)
}

/// A sensor reading a Kubernetes cluster: the Kubernetes check of the
/// cluster (ADR-0183), whose token is never exported.
fn kubernetes(sensor: &Object, app: App) -> Mapped {
    Mapped::Watch {
        watch: Watch::app(app),
        alert: None,
        fidelity: approximated("app_kubernetes", args! {}),
        limits: Vec::new(),
        lost: lost(sensor, &[]),
    }
}

/// The settings naming the container of a Docker Container Status
/// sensor, as PRTG may call it.
const DOCKER_SETTINGS: &[&str] = &["container", "containername"];

/// A database, Redis or Docker sensor: the ready-made check of its kind
/// (ADR-0183). PRTG's database sensors run a query of their own, which
/// the check does not; a Docker sensor watches one container, which the
/// check is limited to when its name is read.
fn app(sensor: &Object, kind: AppKind, host: Option<&str>) -> Mapped {
    let setting = |name: &str| {
        sensor
            .setting(name)
            .map(str::trim)
            .filter(|v| !v.is_empty())
    };
    let mut app = App::new(kind);
    let fidelity = match kind {
        AppKind::Docker => match DOCKER_SETTINGS.iter().find_map(|name| setting(name)) {
            Some(name) => {
                app.containers = Some(format!("^{}$", regex::escape(name)));
                exact()
            }
            None => approximated("app_docker_all", args! {}),
        },
        _ => {
            app.address = host.map(str::to_owned);
            app.port = setting("port")
                .and_then(|p| p.parse::<u16>().ok())
                .filter(|p| *p > 0);
            if kind == AppKind::Postgresql {
                app.database = setting("database").map(str::to_owned);
            }
            if kind == AppKind::Redis {
                approximated("app_password", args! {})
            } else {
                approximated("app_query", args! {})
            }
        }
    };
    Mapped::Watch {
        watch: Watch::app(app),
        alert: None,
        fidelity,
        limits: Vec::new(),
        lost: lost(sensor, &[]),
    }
}

/// A VMware sensor: the device's `vmware` check, and a datastore's limit
/// on free space as the alert on every datastore (ADR-0156).
fn vmware(sensor: &Object, sensor_type: &str) -> Mapped {
    let t = sensor_type.to_ascii_lowercase();
    let mut limits = Vec::new();
    if t.contains("datastore") {
        for (channel, l) in limited(sensor) {
            let free = channel.name.to_ascii_lowercase().contains("free");
            if !free || !is_percent(channel) || (l.min_warning.is_none() && l.min_error.is_none()) {
                continue;
            }
            limits.push(Limit {
                measure: Measure::DatastoreFree,
                direction: Direction::Below,
                warning: l.min_warning.map(|v| v / 100.0),
                critical: l.min_error.map(|v| v / 100.0),
                fidelity: approximated("vmware_every_datastore", args! {}),
                channel: channel.name.clone(),
            });
        }
    }
    let note = if t.contains("wbem") {
        "vmware_wbem"
    } else if t.contains("datastore") || t.contains("hardware") || t.contains("health") {
        "vmware_password"
    } else {
        "virtual_partial"
    };
    let used: Vec<i64> = sensor
        .channels
        .iter()
        .filter(|c| limits.iter().any(|l| l.channel == c.name))
        .map(|c| c.id)
        .collect();
    Mapped::Watch {
        watch: Watch::Vmware,
        alert: None,
        fidelity: approximated(note, args! {}),
        lost: lost(sensor, &used),
        limits,
    }
}

/// A check a probe runs, which fails when the sensor goes down.
fn probe_watch(watch: Watch, fidelity: Fidelity, sensor: &Object) -> Mapped {
    Mapped::Watch {
        watch,
        alert: down(),
        fidelity,
        limits: Vec::new(),
        lost: lost(sensor, &[]),
    }
}

/// The channels with limits that `used` does not take.
pub(super) fn lost(sensor: &Object, used: &[i64]) -> Vec<String> {
    limited(sensor)
        .filter(|(c, _)| !used.contains(&c.id))
        .map(|(c, _)| c.name.clone())
        .collect()
}

fn builtin(watch: Watch, sensor: &Object, limits: Vec<Limit>) -> Mapped {
    let used: Vec<i64> = sensor
        .channels
        .iter()
        .filter(|c| limits.iter().any(|l| l.channel == c.name))
        .map(|c| c.id)
        .collect();
    Mapped::Watch {
        watch,
        alert: None,
        fidelity: exact(),
        lost: lost(sensor, &used),
        limits,
    }
}

fn cpu(sensor: &Object) -> Mapped {
    let mut limits = Vec::new();
    for (channel, l) in limited(sensor) {
        let total = channel.name.to_ascii_lowercase().starts_with("total");
        if !total || (l.max_warning.is_none() && l.max_error.is_none()) {
            continue;
        }
        limits.push(Limit {
            measure: Measure::CpuUsage,
            direction: Direction::Above,
            warning: l.max_warning.map(|v| v / 100.0),
            critical: l.max_error.map(|v| v / 100.0),
            fidelity: exact(),
            channel: channel.name.clone(),
        });
    }
    builtin(Watch::Cpu, sensor, limits)
}

fn memory(sensor: &Object) -> Mapped {
    let mut limits = Vec::new();
    for (channel, l) in limited(sensor) {
        let name = channel.name.to_ascii_lowercase();
        let available = name.contains("available") || name.contains("free");
        if !available || !is_percent(channel) || (l.min_warning.is_none() && l.min_error.is_none())
        {
            continue;
        }
        limits.push(Limit {
            measure: Measure::MemoryAvailable,
            direction: Direction::Below,
            warning: l.min_warning.map(|v| v / 100.0),
            critical: l.min_error.map(|v| v / 100.0),
            fidelity: exact(),
            channel: channel.name.clone(),
        });
    }
    builtin(Watch::Memory, sensor, limits)
}

fn disk(sensor: &Object) -> Mapped {
    let mut limits = Vec::new();
    for (channel, l) in limited(sensor) {
        let free = channel.name.to_ascii_lowercase().contains("free");
        if !free || !is_percent(channel) || (l.min_warning.is_none() && l.min_error.is_none()) {
            continue;
        }
        let mount = disk_mount(&channel.name).or_else(|| disk_mount(&sensor.name));
        let fidelity = if mount.is_some() {
            approximated("disk_free_as_used", args! {})
        } else {
            approximated("prtg_every_disk", args! {})
        };
        // PRTG limits free space from below; MonitorFlow, used space from
        // above: 10 % free is 90 % used.
        limits.push(Limit {
            measure: Measure::DiskUsed { mount },
            direction: Direction::Above,
            warning: l.min_warning.map(|v| 1.0 - v / 100.0),
            critical: l.min_error.map(|v| 1.0 - v / 100.0),
            fidelity,
            channel: channel.name.clone(),
        });
    }
    builtin(Watch::Disk, sensor, limits)
}

fn service(sensor: &Object) -> Mapped {
    match sensor
        .setting("service")
        .or_else(|| sensor.setting("servicename"))
    {
        Some(name) => Mapped::Watch {
            watch: Watch::Service {
                name: name.to_owned(),
            },
            alert: down(),
            fidelity: exact(),
            limits: Vec::new(),
            lost: lost(sensor, &[]),
        },
        None => Mapped::Not(Note::plain("prtg_no_setting")),
    }
}

/// The protocol and default port of a sensor type that speaks one to a
/// port: MonitorFlow checks the port only.
fn protocol(sensor_type: &str) -> Option<(&'static str, u16)> {
    Some(match sensor_type {
        "remotedesktop" => ("RDP", 3389),
        "smtp" => ("SMTP", 25),
        "imap" => ("IMAP", 143),
        "pop3" => ("POP3", 110),
        "ftp" => ("FTP", 21),
        _ => return None,
    })
}

fn port(sensor: &Object, host: Option<&str>) -> Mapped {
    let sensor_type = sensor
        .sensor_type
        .as_deref()
        .unwrap_or("")
        .to_ascii_lowercase();
    let protocol = protocol(&sensor_type);
    let Some(port) = sensor
        .setting("port")
        .and_then(|p| p.parse::<u16>().ok())
        .filter(|p| *p > 0)
        .or(protocol.map(|(_, p)| p))
    else {
        return Mapped::Not(Note::plain("prtg_no_setting"));
    };
    let fidelity = match protocol {
        Some((name, _)) => approximated(
            "prtg_port_only",
            args! {"port" => port.to_string(), "protocol" => name},
        ),
        None => approximated("prtg_from_server", args! {}),
    };
    Mapped::Watch {
        watch: Watch::Port {
            address: host.map(str::to_owned),
            port,
        },
        alert: down(),
        fidelity,
        limits: Vec::new(),
        lost: lost(sensor, &[]),
    }
}

/// The settings of HTTP sensors a website check takes: the URL, the
/// method and its body, the time it may take, the keyword the page must
/// or must not hold (plain text or a regular expression), custom headers
/// and the user name of the authentication, never its password. PRTG
/// names them as below; a setting PRTG does not have for a sensor type
/// reads as absent.
const WEB_SETTINGS: &[&str] = &[
    "httpurl",
    "url",
    "httpmethod",
    "postdata",
    "timeout",
    "includemode",
    "includestring",
    "includetype",
    "excludemode",
    "excludestring",
    "excludetype",
    "httpheaders",
    "authmethod",
    "httpusername",
];

/// A keyword condition: its text when the mode is on, plain or as the
/// literal a regular expression is.
fn keyword(
    sensor: &Object,
    mode: &str,
    text: &str,
    kind: &str,
    notes: &mut Vec<Note>,
) -> Option<String> {
    let on = sensor.setting(mode).is_none_or(|m| m.trim() != "0");
    let value = sensor.setting(text).filter(|t| !t.is_empty())?;
    if !on {
        return None;
    }
    let regex = sensor
        .setting(kind)
        .is_some_and(|k| k.trim() == "1" || k.to_ascii_lowercase().contains("reg"));
    match crate::web::condition(value, regex, text) {
        Ok(text) => Some(text),
        Err(note) => {
            notes.push(note);
            None
        }
    }
}

fn web(sensor: &Object, host: Option<&str>) -> Mapped {
    let raw = sensor
        .setting("httpurl")
        .or_else(|| sensor.setting("url"))
        .unwrap_or("/");
    let url = if raw.contains("://") {
        raw.to_owned()
    } else {
        // A path alone is relative to the device, over plain HTTP.
        match host {
            Some(host) if host.contains(':') => {
                format!("http://[{host}]/{}", raw.trim_start_matches('/'))
            }
            Some(host) => format!("http://{host}/{}", raw.trim_start_matches('/')),
            None => return Mapped::Not(Note::plain("prtg_no_setting")),
        }
    };
    let mut site = Website::new(url.clone());
    let mut notes = Vec::new();
    match sensor
        .setting("httpmethod")
        .map(|m| m.trim().to_ascii_uppercase())
        .as_deref()
    {
        None | Some("" | "GET") => {}
        Some("HEAD") => site.method = WebMethod::Head,
        Some(other) => notes.push(Note::new("web_method", args! {"method" => other})),
    }
    if sensor
        .setting("postdata")
        .is_some_and(|d| !d.trim().is_empty())
        && notes.is_empty()
    {
        notes.push(Note::new("web_setting", args! {"setting" => "postdata"}));
    }
    if site.method == WebMethod::Get {
        site.contains = keyword(
            sensor,
            "includemode",
            "includestring",
            "includetype",
            &mut notes,
        );
        site.not_contains = keyword(
            sensor,
            "excludemode",
            "excludestring",
            "excludetype",
            &mut notes,
        );
    }
    if let Some(headers) = sensor.setting("httpheaders") {
        for line in headers.lines().map(str::trim).filter(|l| !l.is_empty()) {
            match line.split_once(':') {
                Some((name, value)) => site
                    .headers
                    .push((name.trim().to_owned(), value.trim().to_owned())),
                None => notes.push(Note::new("web_setting", args! {"setting" => "httpheaders"})),
            }
        }
    }
    let user = sensor
        .setting("httpusername")
        .map(str::trim)
        .filter(|u| !u.is_empty());
    let method = sensor
        .setting("authmethod")
        .map(|m| m.trim().to_ascii_lowercase())
        .unwrap_or_default();
    if method.contains("ntlm") || method.contains("windows") {
        notes.push(Note::new("web_auth", args! {"kind" => "NTLM"}));
    } else if let Some(user) = user {
        site.username = Some(user.to_owned());
        // PRTG never shows a password back: assume there is one.
        site.credentials = true;
    }
    site.timeout_s = sensor
        .setting("timeout")
        .and_then(|t| t.trim().parse::<u32>().ok())
        .filter(|t| *t > 0);
    // Response time limits are a threshold on the check's response time.
    let mut limits = Vec::new();
    for (channel, l) in limited(sensor) {
        let name = channel.name.to_ascii_lowercase();
        let timed = name.contains("response") || name.contains("loading");
        if !timed || (l.max_warning.is_none() && l.max_error.is_none()) {
            continue;
        }
        let seconds = |v: f64| match channel.unit.trim().to_ascii_lowercase().as_str() {
            "s" | "sec" | "second" | "seconds" => v,
            _ => v / 1000.0,
        };
        limits.push(Limit {
            measure: Measure::WebResponseTime { url: url.clone() },
            direction: Direction::Above,
            warning: l.max_warning.map(seconds),
            critical: l.max_error.map(seconds),
            fidelity: exact(),
            channel: channel.name.clone(),
        });
    }
    let used: Vec<i64> = sensor
        .channels
        .iter()
        .filter(|c| limits.iter().any(|l| l.channel == c.name))
        .map(|c| c.id)
        .collect();
    Mapped::Watch {
        watch: Watch::web(site),
        alert: down(),
        fidelity: notes
            .into_iter()
            .next()
            .map_or(Fidelity::Exact, Fidelity::Approximated),
        lost: lost(sensor, &used),
        limits,
    }
}

/// An SSL certificate sensor: a website check of the address, whatever
/// the page answers, and its days limits on the days left.
fn certificate(sensor: &Object, host: Option<&str>) -> Mapped {
    let port = sensor
        .setting("port")
        .and_then(|p| p.parse::<u16>().ok())
        .filter(|p| *p > 0)
        .unwrap_or(443);
    let Some(address) = sensor
        .setting("sniname")
        .filter(|s| !s.trim().is_empty())
        .or(host)
    else {
        return Mapped::Not(Note::plain("prtg_no_setting"));
    };
    let url = crate::web::https_url(address, port);
    let mut limits = Vec::new();
    for (channel, l) in limited(sensor) {
        let name = channel.name.to_ascii_lowercase();
        if !name.contains("days") || (l.min_warning.is_none() && l.min_error.is_none()) {
            continue;
        }
        limits.push(Limit {
            measure: Measure::CertificateDays { url: url.clone() },
            direction: Direction::Below,
            warning: l.min_warning,
            critical: l.min_error,
            fidelity: exact(),
            channel: channel.name.clone(),
        });
    }
    let used: Vec<i64> = sensor
        .channels
        .iter()
        .filter(|c| limits.iter().any(|l| l.channel == c.name))
        .map(|c| c.id)
        .collect();
    Mapped::Watch {
        watch: Watch::web(Website::new(url).any_status()),
        alert: down(),
        fidelity: approximated("web_certificate_only", args! {}),
        lost: lost(sensor, &used),
        limits,
    }
}

/// The event levels of PRTG's event type setting.
fn levels(sensor: &Object) -> Option<Vec<String>> {
    let raw = sensor
        .setting("eventtype")
        .or_else(|| sensor.setting("filtertype"))?
        .to_ascii_lowercase();
    let levels: Vec<String> = ["critical", "error", "warning", "information"]
        .into_iter()
        .filter(|l| raw.contains(l))
        .map(str::to_owned)
        .collect();
    (!levels.is_empty()).then_some(levels)
}

fn event_log(sensor: &Object) -> Mapped {
    let channel = sensor
        .setting("logfile")
        .or_else(|| sensor.setting("wmilogfile"))
        .or_else(|| sensor.setting("eventlog"))
        .unwrap_or("Application")
        .to_owned();
    let mut notes = Vec::new();
    let levels = levels(sensor).unwrap_or_else(|| {
        notes.push(Note::plain("prtg_event_levels"));
        vec!["critical".into(), "error".into()]
    });
    let providers: Vec<String> = sensor
        .setting("eventsource")
        .or_else(|| sensor.setting("filtersource"))
        .map(|s| {
            s.split(',')
                .map(|p| p.trim().to_owned())
                .filter(|p| !p.is_empty())
                .collect()
        })
        .unwrap_or_default();
    let mut event_ids = Vec::new();
    if let Some(ids) = sensor
        .setting("eventid")
        .or_else(|| sensor.setting("filterid"))
    {
        match ids
            .split(',')
            .map(|id| id.trim().parse::<u32>())
            .collect::<Result<Vec<_>, _>>()
        {
            Ok(ids) => event_ids = ids,
            Err(_) => notes.push(Note::plain("prtg_event_filter")),
        }
    }
    if sensor.setting("message").is_some() || sensor.setting("filtermessage").is_some() {
        notes.push(Note::plain("prtg_event_filter"));
    }
    // MonitorFlow alerts on the first matching event; PRTG on its limit.
    let limit = limited(sensor).find_map(|(_, l)| l.max_error.or(l.max_warning));
    if let Some(limit) = limit.filter(|l| *l > 0.0) {
        notes.push(Note::new("prtg_event_count", args! {"count" => limit}));
    }
    let severity = match limited(sensor).next() {
        Some((_, l)) if l.max_error.is_none() && l.max_warning.is_some() => Severity::Warning,
        Some(_) => Severity::Critical,
        None => Severity::Warning,
    };
    let fidelity = notes
        .into_iter()
        .next()
        .map_or(Fidelity::Exact, Fidelity::Approximated);
    Mapped::Watch {
        watch: Watch::EventLog {
            channel,
            levels,
            providers,
            event_ids,
        },
        alert: Some(Alert {
            severity,
            for_s: 0,
            no_data: false,
        }),
        fidelity,
        limits: Vec::new(),
        lost: Vec::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::prtg::tree::Kind;

    #[test]
    fn families_by_type() {
        assert_eq!(family("ping"), Family::Ping);
        assert_eq!(family("snmptraffic"), Family::Traffic);
        for flow in [
            "netflow5",
            "netflow9custom",
            "jflowv5",
            "sflow",
            "sflowcustom",
            "ipfix",
            "paessler.flow.ipfix_v2.1",
        ] {
            assert_eq!(family(flow), Family::Flow, "{flow}");
        }
        let mut flow = Object::new(1, Kind::Sensor, "NetFlow", Some(0));
        flow.sensor_type = Some("netflow9".into());
        assert_eq!(map(&flow, None), Mapped::Not(Note::plain("flow_sensor")));
        assert_eq!(family("wmiservice"), Family::Service);
        assert_eq!(family("snmpcpu"), Family::Cpu);
        assert_eq!(family("wmivolume"), Family::Disk);
        assert_eq!(family("httpadvanced"), Family::Web);
        assert_eq!(family("httptransaction"), Family::Other);
        assert_eq!(family("wineventlog"), Family::EventLog);
    }

    /// Sensors speaking a protocol to a port check that port, and the
    /// newer `paessler.*` types are known by their name, as seen on a
    /// real PRTG 24.3.
    #[test]
    fn protocols_and_newer_types() {
        assert_eq!(family("paessler.DNS.dns_v2.40"), Family::Dns);
        assert_eq!(family("paessler.snmp.custom_table"), Family::SnmpCustom);
        assert_eq!(family("paessler.paecloud.http_v2_sensor.41"), Family::Other);
        let sensor = |t: &str, port: Option<&str>| {
            let mut s = Object::new(1, Kind::Sensor, "s", Some(0));
            s.sensor_type = Some(t.into());
            if let Some(port) = port {
                s.settings.insert("port".into(), port.into());
            }
            s
        };
        for (t, setting, port, protocol) in [
            ("remotedesktop", Some("3389"), 3389, "RDP"),
            ("smtp", Some("587"), 587, "SMTP"),
            ("imap", None, 143, "IMAP"),
            ("pop3", None, 110, "POP3"),
        ] {
            let Mapped::Watch {
                watch, fidelity, ..
            } = map(&sensor(t, setting), Some("192.0.2.1"))
            else {
                panic!("{t} moves");
            };
            assert_eq!(
                watch,
                Watch::Port {
                    address: Some("192.0.2.1".into()),
                    port
                },
                "{t}"
            );
            let Fidelity::Approximated(note) = fidelity else {
                panic!("{t} only checks the port");
            };
            assert_eq!(note.code, "prtg_port_only");
            assert_eq!(note.args["protocol"], protocol);
        }
        // A plain port sensor still needs its port.
        assert!(matches!(map(&sensor("port", None), None), Mapped::Not(_)));
    }

    /// A vendor's hardware health is not the system group: once read as
    /// the uptime, exact, because its type holds the word "system".
    #[test]
    fn hardware_health_is_not_the_uptime() {
        let sensor = |t: &str| {
            let mut s = Object::new(1, Kind::Sensor, "Health", Some(0));
            s.sensor_type = Some(t.into());
            s
        };
        for t in [
            "snmpdellsystemhealth",
            "snmphpsystemhealth",
            "snmpciscosystemhealth",
            "snmpnetappsystemhealth",
        ] {
            assert!(
                matches!(map(&sensor(t), None), Mapped::Not(ref n) if n.code == "snmp_object"),
                "{t}"
            );
        }
        let Mapped::Watch {
            watch, fidelity, ..
        } = map(&sensor("snmpuptime"), None)
        else {
            panic!("the uptime moves");
        };
        assert_eq!(
            watch,
            Watch::Snmp {
                group: "system".into()
            }
        );
        assert_eq!(fidelity, Fidelity::Exact);
    }

    /// VMware and Hyper-V sensors (ADR-0156), by their raw types and the
    /// names PRTG shows alike.
    #[test]
    fn virtualization_sensors() {
        for t in [
            "vmwaredatastoreextern",
            "VMware Virtual Machine (SOAP)",
            "esxserversensorextern",
            "vcenter",
        ] {
            assert_eq!(family(t), Family::Vmware, "{t}");
        }
        for t in ["hypervvm", "Hyper-V Virtual Storage Device"] {
            assert_eq!(family(t), Family::HyperV, "{t}");
        }
        let mut datastore = Object::new(1, Kind::Sensor, "Datastore ds1", Some(0));
        datastore.sensor_type = Some("vmware datastore (soap)".into());
        datastore.channels = vec![
            crate::prtg::tree::Channel {
                id: 1,
                name: "Free Space".into(),
                unit: "%".into(),
                limits: Some(crate::prtg::tree::Limits {
                    max_error: None,
                    max_warning: None,
                    min_error: Some(5.0),
                    min_warning: Some(15.0),
                }),
                ..Default::default()
            },
            crate::prtg::tree::Channel {
                id: 2,
                name: "Read Latency".into(),
                unit: "msec".into(),
                limits: Some(crate::prtg::tree::Limits {
                    max_error: Some(50.0),
                    max_warning: None,
                    min_error: None,
                    min_warning: None,
                }),
                ..Default::default()
            },
        ];
        let Mapped::Watch {
            watch,
            fidelity,
            limits,
            lost,
            ..
        } = map(&datastore, None)
        else {
            panic!("not mapped");
        };
        assert_eq!(watch, Watch::Vmware);
        assert_eq!(fidelity, approximated("vmware_password", args! {}));
        assert_eq!(limits.len(), 1);
        assert_eq!(limits[0].measure, Measure::DatastoreFree);
        assert_eq!(
            (limits[0].warning, limits[0].critical),
            (Some(0.15), Some(0.05))
        );
        assert_eq!(lost, ["Read Latency"]);

        let mut wbem = Object::new(2, Kind::Sensor, "Hardware", Some(0));
        wbem.sensor_type = Some("vmware host hardware (wbem)".into());
        let Mapped::Watch { fidelity, .. } = map(&wbem, None) else {
            panic!()
        };
        assert_eq!(fidelity, approximated("vmware_wbem", args! {}));

        let mut vm = Object::new(3, Kind::Sensor, "VM web-01", Some(0));
        vm.sensor_type = Some("hyper-v virtual machine".into());
        let Mapped::Watch {
            watch, fidelity, ..
        } = map(&vm, None)
        else {
            panic!()
        };
        assert_eq!(watch, Watch::HyperV);
        assert_eq!(fidelity, approximated("hyperv_group", args! {}));
        vm.sensor_type = Some("hyper-v virtual network adapter".into());
        let Mapped::Watch { fidelity, .. } = map(&vm, None) else {
            panic!()
        };
        assert_eq!(fidelity, approximated("virtual_partial", args! {}));
    }

    #[test]
    fn mounts_from_names() {
        assert_eq!(disk_mount("C:\\ Free Space").as_deref(), Some("C:\\"));
        assert_eq!(disk_mount("Disk Free e: data").as_deref(), Some("E:\\"));
        assert_eq!(
            disk_mount("Free Space /var/lib").as_deref(),
            Some("/var/lib")
        );
        assert_eq!(disk_mount("Free Space"), None);
        assert_eq!(disk_mount("Total: 5"), None);
    }

    #[test]
    fn a_service_without_its_name_does_not_move() {
        let mut sensor = Object::new(1, Kind::Sensor, "Service", Some(0));
        sensor.sensor_type = Some("wmiservice".into());
        assert_eq!(
            map(&sensor, None),
            Mapped::Not(Note::plain("prtg_no_setting"))
        );
    }

    #[test]
    fn a_relative_page_is_on_the_device() {
        let mut sensor = Object::new(1, Kind::Sensor, "Page", Some(0));
        sensor.sensor_type = Some("http".into());
        sensor.settings.insert("httpurl".into(), "/status".into());
        let Mapped::Watch { watch, .. } = map(&sensor, Some("fd00::1")) else {
            panic!()
        };
        assert_eq!(watch, Watch::web(Website::new("http://[fd00::1]/status")));
    }

    fn advanced(settings: &[(&str, &str)]) -> (Watch, Fidelity) {
        let mut sensor = Object::new(1, Kind::Sensor, "Shop", Some(0));
        sensor.sensor_type = Some("httpadvanced".into());
        sensor
            .settings
            .insert("httpurl".into(), "https://shop.example.com/".into());
        for (k, v) in settings {
            sensor.settings.insert((*k).into(), (*v).into());
        }
        let Mapped::Watch {
            watch, fidelity, ..
        } = map(&sensor, None)
        else {
            panic!()
        };
        (watch, fidelity)
    }

    #[test]
    fn http_advanced_settings() {
        let (watch, fidelity) = advanced(&[
            ("httpmethod", "GET"),
            ("timeout", "30"),
            ("includemode", "1"),
            ("includestring", "Welcome"),
            ("excludemode", "1"),
            ("excludestring", "Maintenance\\ mode"),
            ("excludetype", "1"),
            ("httpheaders", "Accept-Language: en\nX-Monitor: prtg"),
            ("httpusername", "monitor"),
        ]);
        let mut expected = Website::new("https://shop.example.com/");
        expected.contains = Some("Welcome".into());
        expected.not_contains = Some("Maintenance mode".into());
        expected.headers = vec![
            ("Accept-Language".into(), "en".into()),
            ("X-Monitor".into(), "prtg".into()),
        ];
        expected.username = Some("monitor".into());
        expected.credentials = true;
        expected.timeout_s = Some(30);
        assert_eq!(watch, Watch::web(expected));
        assert_eq!(fidelity, Fidelity::Exact);
        // A keyword check turned off is no condition.
        let (watch, _) = advanced(&[("includemode", "0"), ("includestring", "Welcome")]);
        assert_eq!(watch, Watch::web(Website::new("https://shop.example.com/")));
        // What a website check cannot do.
        let (_, fidelity) = advanced(&[("httpmethod", "POST"), ("postdata", "a=1")]);
        assert_eq!(
            fidelity,
            approximated("web_method", args! {"method" => "POST"})
        );
        let (_, fidelity) = advanced(&[("includestring", "Total: \\d+"), ("includetype", "1")]);
        assert_eq!(
            fidelity,
            approximated("web_regex", args! {"pattern" => "Total: \\d+"})
        );
        let (watch, fidelity) = advanced(&[("authmethod", "Windows NTLM"), ("httpusername", "x")]);
        assert_eq!(fidelity, approximated("web_auth", args! {"kind" => "NTLM"}));
        assert!(matches!(watch, Watch::Web(site) if !site.credentials));
        let (watch, _) = advanced(&[("httpmethod", "HEAD")]);
        assert!(matches!(watch, Watch::Web(site) if site.method == WebMethod::Head));
    }

    #[test]
    fn a_certificate_sensor_checks_its_address() {
        let mut sensor = Object::new(1, Kind::Sensor, "Certificate", Some(0));
        sensor.sensor_type = Some("sslcertificate".into());
        sensor.settings.insert("port".into(), "8443".into());
        sensor
            .settings
            .insert("sniname".into(), "shop.example.com".into());
        let Mapped::Watch {
            watch, fidelity, ..
        } = map(&sensor, Some("192.0.2.30"))
        else {
            panic!()
        };
        assert_eq!(
            watch,
            Watch::web(Website::new("https://shop.example.com:8443/").any_status())
        );
        assert_eq!(fidelity, approximated("web_certificate_only", args! {}));
        sensor.settings.clear();
        assert_eq!(
            map(&sensor, None),
            Mapped::Not(Note::plain("prtg_no_setting"))
        );
    }

    /// Database, Redis and Docker sensors become ready-made application
    /// checks (ADR-0183); other databases cannot be moved.
    #[test]
    fn application_sensors() {
        for (t, kind) in [
            ("postgresql", AppKind::Postgresql),
            ("PostgreSQL", AppKind::Postgresql),
            ("mysqlv2", AppKind::Mysql),
            ("MySQL v2", AppKind::Mysql),
            ("mysql", AppKind::Mysql),
            ("redis", AppKind::Redis),
            ("dockercontainerstatus", AppKind::Docker),
            ("Docker Container Status", AppKind::Docker),
        ] {
            assert_eq!(family(t), Family::App(kind), "{t}");
        }
        for t in ["mssqlv2", "oraclev2", "Microsoft SQL v2", "adosqlv2"] {
            assert_eq!(family(t), Family::OtherDatabase, "{t}");
        }
        let sensor = |t: &str| {
            let mut s = Object::new(1, Kind::Sensor, "Database", Some(0));
            s.sensor_type = Some(t.into());
            s
        };
        let mut pg = sensor("postgresql");
        pg.settings.insert("port".into(), "5433".into());
        pg.settings.insert("database".into(), "shop".into());
        let Mapped::Watch {
            watch, fidelity, ..
        } = map(&pg, Some("192.0.2.40"))
        else {
            panic!()
        };
        let mut expected = App::new(AppKind::Postgresql);
        expected.address = Some("192.0.2.40".into());
        expected.port = Some(5433);
        expected.database = Some("shop".into());
        assert_eq!(watch, Watch::app(expected));
        assert_eq!(fidelity, approximated("app_query", args! {}));

        let Mapped::Watch {
            watch, fidelity, ..
        } = map(&sensor("redis"), Some("192.0.2.41"))
        else {
            panic!()
        };
        let mut expected = App::new(AppKind::Redis);
        expected.address = Some("192.0.2.41".into());
        assert_eq!(watch, Watch::app(expected));
        assert_eq!(fidelity, approximated("app_password", args! {}));

        // One container by name: exactly that one.
        let mut docker = sensor("dockercontainerstatus");
        docker
            .settings
            .insert("container".into(), "shop.web".into());
        let Mapped::Watch {
            watch, fidelity, ..
        } = map(&docker, Some("192.0.2.42"))
        else {
            panic!()
        };
        let mut expected = App::new(AppKind::Docker);
        expected.containers = Some("^shop\\.web$".into());
        assert_eq!(watch, Watch::app(expected));
        assert_eq!(fidelity, Fidelity::Exact);
        docker.settings.clear();
        let Mapped::Watch { fidelity, .. } = map(&docker, None) else {
            panic!()
        };
        assert_eq!(fidelity, approximated("app_docker_all", args! {}));

        assert_eq!(
            map(&sensor("mssqlv2"), Some("192.0.2.43")),
            Mapped::Not(Note::plain("app_other_database"))
        );
    }

    /// PRTG has no Kubernetes sensor: custom ones named so, and HTTP or
    /// REST Custom sensors reading a cluster's API, become the Kubernetes
    /// check (ADR-0183); other REST sensors are not moved.
    #[test]
    fn kubernetes_sensors() {
        assert_eq!(
            family("Kubernetes Nodes (EXE/XML)"),
            Family::App(AppKind::Kubernetes)
        );
        assert_eq!(family("restcustom"), Family::Rest);
        assert_eq!(wanted("restcustom"), (REST_SETTINGS, false));
        let sensor = |t: &str, url: Option<&str>| {
            let mut s = Object::new(1, Kind::Sensor, "API", Some(0));
            s.sensor_type = Some(t.into());
            if let Some(url) = url {
                s.settings.insert("url".into(), url.into());
            }
            s
        };
        let watch = |mapped: Mapped| match mapped {
            Mapped::Watch {
                watch, fidelity, ..
            } => {
                assert_eq!(fidelity, approximated("app_kubernetes", args! {}));
                watch
            }
            other => panic!("{other:?}"),
        };
        let mut on_device = App::new(AppKind::Kubernetes);
        on_device.address = Some("192.0.2.70".into());
        assert_eq!(
            watch(map(
                &sensor("restcustom", Some("https://192.0.2.70:6443/api/v1/nodes")),
                None
            )),
            Watch::app(on_device.clone())
        );
        assert_eq!(
            watch(map(
                &sensor("httpadvanced", Some("https://192.0.2.70:6443/readyz")),
                None
            )),
            Watch::app(on_device.clone())
        );
        assert_eq!(
            watch(map(
                &sensor("restcustom", Some("/apis/apps/v1/deployments")),
                Some("192.0.2.70")
            )),
            Watch::app(on_device.clone())
        );
        let mut other_port = App::new(AppKind::Kubernetes);
        other_port.address = Some("k8s.example.com".into());
        other_port.port = Some(443);
        assert_eq!(
            watch(map(
                &sensor("restcustom", Some("https://k8s.example.com/api/v1/pods")),
                None
            )),
            Watch::app(other_port)
        );
        assert_eq!(
            watch(map(
                &sensor("Kubernetes Pods (Python)", None),
                Some("192.0.2.70")
            )),
            Watch::app(on_device)
        );
        // A health path elsewhere than 6443 is any website's.
        assert!(matches!(
            map(
                &sensor("httpadvanced", Some("https://shop.example.com/healthz")),
                None
            ),
            Mapped::Watch {
                watch: Watch::Web(_),
                ..
            }
        ));
        assert_eq!(
            map(
                &sensor("restcustom", Some("https://api.example.com/v2/orders")),
                None
            ),
            Mapped::Not(Note::new(
                "prtg_sensor_type",
                args! {"type" => "restcustom"}
            ))
        );
    }
}
