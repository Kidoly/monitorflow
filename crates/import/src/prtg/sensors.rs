//! One PRTG sensor to what MonitorFlow watches and its thresholds.
//!
//! PRTG measures from a probe, over the network; MonitorFlow measures on
//! the server, with its agent. A sensor that reads the machine (processor,
//! memory, disks, services, event log) moves as it is; one that looks at
//! it from outside (ping, ports, pages, certificates) becomes the nearest
//! thing the agent checks, with a note.

use crate::args;
use crate::model::{Alert, Direction, Fidelity, Measure, Severity, Watch};
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
    Other,
}

/// The family of a sensor type, like `wmiservice` or `snmptraffic`.
pub fn family(sensor_type: &str) -> Family {
    let t = sensor_type.to_ascii_lowercase();
    let has = |s: &str| t.contains(s);
    if t.starts_with("ping") {
        Family::Ping
    } else if has("traffic") {
        Family::Traffic
    } else if t == "sslcertificate" {
        Family::Certificate
    } else if matches!(t.as_str(), "http" | "httpadvanced" | "httpfull") {
        Family::Web
    } else if t == "port" {
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
    } else {
        Family::Other
    }
}

/// The settings this reader reads of a sensor type, and whether it reads
/// its channels, so neither the API nor the file is asked for more.
pub fn wanted(sensor_type: &str) -> (&'static [&'static str], bool) {
    match family(sensor_type) {
        Family::Port => (&["port"], true),
        Family::Web => (&["httpurl", "url"], true),
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
        Family::Ping | Family::Traffic | Family::Other => (&[], false),
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
    })
}

/// Maps `sensor`, on a device whose address is `host`.
pub fn map(sensor: &Object, host: Option<&str>) -> Mapped {
    let sensor_type = sensor.sensor_type.as_deref().unwrap_or("");
    match family(sensor_type) {
        Family::Ping => Mapped::Watch {
            watch: Watch::Agent,
            alert: None,
            fidelity: approximated("prtg_ping", args! {}),
            limits: Vec::new(),
            lost: Vec::new(),
        },
        Family::Cpu => cpu(sensor),
        Family::Memory => memory(sensor),
        Family::Disk => disk(sensor),
        Family::Service => service(sensor),
        Family::Port => port(sensor, host),
        Family::Web => web(sensor, host),
        Family::Certificate => certificate(sensor, host),
        Family::EventLog => event_log(sensor),
        Family::Traffic => Mapped::Not(Note::plain("network_traffic")),
        Family::Other => Mapped::Not(Note::new("prtg_sensor_type", args! {"type" => sensor_type})),
    }
}

/// The channels with limits that `used` does not take.
fn lost(sensor: &Object, used: &[i64]) -> Vec<String> {
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

fn port(sensor: &Object, host: Option<&str>) -> Mapped {
    let Some(port) = sensor
        .setting("port")
        .and_then(|p| p.parse::<u16>().ok())
        .filter(|p| *p > 0)
    else {
        return Mapped::Not(Note::plain("prtg_no_setting"));
    };
    Mapped::Watch {
        watch: Watch::Port {
            address: host.map(str::to_owned),
            port,
        },
        alert: down(),
        fidelity: approximated("prtg_from_server", args! {}),
        limits: Vec::new(),
        lost: lost(sensor, &[]),
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
    // The planner adds its own note: the port only, not the page.
    Mapped::Watch {
        watch: Watch::Web { url },
        alert: down(),
        fidelity: exact(),
        limits: Vec::new(),
        lost: lost(sensor, &[]),
    }
}

fn certificate(sensor: &Object, host: Option<&str>) -> Mapped {
    let port = sensor
        .setting("port")
        .and_then(|p| p.parse::<u16>().ok())
        .filter(|p| *p > 0)
        .unwrap_or(443);
    let address = sensor.setting("sniname").or(host).map(str::to_owned);
    Mapped::Watch {
        watch: Watch::Port { address, port },
        alert: down(),
        fidelity: approximated("prtg_certificate", args! {"port" => port}),
        limits: Vec::new(),
        lost: lost(sensor, &[]),
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
        alert: Some(Alert { severity, for_s: 0 }),
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
        assert_eq!(family("wmiservice"), Family::Service);
        assert_eq!(family("snmpcpu"), Family::Cpu);
        assert_eq!(family("wmivolume"), Family::Disk);
        assert_eq!(family("httpadvanced"), Family::Web);
        assert_eq!(family("httptransaction"), Family::Other);
        assert_eq!(family("wineventlog"), Family::EventLog);
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
        assert_eq!(
            watch,
            Watch::Web {
                url: "http://[fd00::1]/status".into()
            }
        );
    }
}
