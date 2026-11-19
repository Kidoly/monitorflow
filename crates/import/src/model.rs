//! The neutral description of what another monitoring tool watches
//! (ADR-0146): hosts, checks, thresholds, silences and notification
//! channels, each with where it came from and how faithfully MonitorFlow
//! can do the same. Readers fill it; the server turns it into a preview
//! and, once confirmed, into servers, checks, thresholds, silences and
//! channels. Nothing here knows about the database.
//!
//! Every reference between items is by a host's [`Host::key`], unique in
//! one [`Inventory`].

use std::collections::BTreeMap;
use std::fmt;

use chrono::{DateTime, NaiveDate, NaiveTime, Utc};
use monitorflow_common::redact::Secret;
use serde::Serialize;

use crate::notes::Note;

/// Everything read from one source.
#[derive(Debug, Default, Clone, Serialize)]
pub struct Inventory {
    pub hosts: Vec<Host>,
    pub checks: Vec<Check>,
    pub thresholds: Vec<Threshold>,
    pub silences: Vec<Silence>,
    /// Maintenance windows that come back (ADR-0151).
    pub windows: Vec<Window>,
    pub channels: Vec<Channel>,
    /// Business services, with their rule and SLA target (ADR-0155).
    pub services: Vec<Service>,
    /// What is behind what (ADR-0152).
    pub dependencies: Vec<Dependency>,
    /// What the source has and MonitorFlow cannot do, with why.
    pub unmovable: Vec<Unmovable>,
}

impl Inventory {
    /// The host of `key`, if the inventory has it.
    pub fn host(&self, key: &str) -> Option<&Host> {
        self.hosts.iter().find(|h| h.key == key)
    }

    /// Whether there is nothing at all to show.
    pub fn is_empty(&self) -> bool {
        self.hosts.is_empty()
            && self.checks.is_empty()
            && self.thresholds.is_empty()
            && self.silences.is_empty()
            && self.windows.is_empty()
            && self.channels.is_empty()
            && self.services.is_empty()
            && self.dependencies.is_empty()
            && self.unmovable.is_empty()
    }
}

/// Where an item came from in the source, in the source's own terms.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Origin {
    /// What it is there, a code the web interface translates (see
    /// [`crate::notes::ORIGIN_KINDS`]): `host`, `item`, `trigger`,
    /// `template`, `web_scenario`, `maintenance`, `media_type`,
    /// `user_media`, `scrape_target`, `alerting_rule`, `receiver`,
    /// `silence`, `sensor`, `notification`, `device`, `group`, `probe`.
    pub kind: String,
    /// Its name there.
    pub name: String,
    /// The key, expression or setting it was read from, shown as is.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
}

impl Origin {
    pub fn new(kind: &str, name: impl Into<String>) -> Self {
        Self {
            kind: kind.to_owned(),
            name: name.into(),
            detail: None,
        }
    }

    pub fn with_detail(mut self, detail: impl Into<String>) -> Self {
        let detail = detail.into();
        self.detail = (!detail.is_empty()).then_some(detail);
        self
    }
}

/// How faithfully MonitorFlow does what the source item did.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(tag = "fidelity", content = "note", rename_all = "snake_case")]
pub enum Fidelity {
    /// The same thing, the same way.
    Exact,
    /// Close, and the note says how it differs.
    Approximated(Note),
}

/// What the source says of a host's system.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Os {
    #[default]
    Unknown,
    Linux,
    Debian,
    Rhel,
    Windows,
}

impl Os {
    /// A guess from free text: an OS name, a template name, a platform.
    pub fn guess(text: &str) -> Self {
        let text = text.to_ascii_lowercase();
        let has = |words: &[&str]| words.iter().any(|w| text.contains(w));
        if has(&["windows"]) {
            Self::Windows
        } else if has(&["debian", "ubuntu", "mint", "raspbian", "proxmox"]) {
            Self::Debian
        } else if has(&[
            "red hat",
            "redhat",
            "rhel",
            "centos",
            "rocky",
            "alma",
            "fedora",
            "oracle linux",
        ]) {
            Self::Rhel
        } else if has(&["linux", "unix"]) {
            Self::Linux
        } else {
            Self::Unknown
        }
    }

    /// The more precise of two hints.
    pub fn or(self, other: Self) -> Self {
        match (self, other) {
            (Self::Unknown, o) => o,
            (Self::Linux, o @ (Self::Debian | Self::Rhel)) => o,
            (s, _) => s,
        }
    }
}

/// A machine the source watches. In MonitorFlow it becomes a server
/// waiting for its agent, unless one of that name exists already.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Host {
    /// Unique in the inventory; other items refer to the host by it.
    pub key: String,
    /// The technical name, which the agent's host name is matched with.
    pub name: String,
    /// A friendlier name the source shows, when it has one.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub visible_name: Option<String>,
    /// IP addresses and DNS names, as the source has them.
    pub addresses: Vec<String>,
    pub os: Os,
    /// Groups, folders or jobs it belongs to there.
    pub groups: Vec<String>,
    pub origin: Origin,
    /// No agent can run on it, like a switch or a printer the source
    /// watches by ping or SNMP: it becomes a device, watched from a probe
    /// (ADR-0148).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub agentless: bool,
}

/// Alert severity.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Severity {
    Warning,
    Critical,
}

/// What a check watches.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Watch {
    /// Processor usage and load. Every server measures it already.
    Cpu,
    /// Memory and swap. Every server measures it already.
    Memory,
    /// Space on every mounted file system. Every server measures it.
    Disk,
    /// Disk activity. Every server measures it already.
    DiskIo,
    /// The agent answering: MonitorFlow's offline alert.
    Agent,
    /// A service: a systemd unit, or a Windows service on Windows.
    Service { name: String },
    /// A TCP port answering, on the host itself or another address.
    Port {
        #[serde(skip_serializing_if = "Option::is_none")]
        address: Option<String>,
        port: u16,
    },
    /// Lines of a log file matching a pattern (a regular expression).
    Log { path: String, pattern: String },
    /// Events of a Windows Event Log channel, filtered by level, provider
    /// and event ID (empty lists: any).
    EventLog {
        channel: String,
        levels: Vec<String>,
        providers: Vec<String>,
        event_ids: Vec<u32>,
    },
    /// A web page or API answering as expected: a website check
    /// (ADR-0150).
    Web(Box<Website>),
    /// The host, or another address, answering ping, from the central
    /// server or a relay (ADR-0148).
    Ping {
        #[serde(skip_serializing_if = "Option::is_none")]
        address: Option<String>,
    },
    /// A UDP port of the host or another address, from a probe: `query`
    /// is `dns` or `ntp` for an answer to expect, `none` for no refusal.
    UdpPort {
        #[serde(skip_serializing_if = "Option::is_none")]
        address: Option<String>,
        port: u16,
        #[serde(skip_serializing_if = "Option::is_none")]
        query: Option<String>,
    },
    /// What a group of the common SNMP objects says (ADR-0149): `system`,
    /// `interfaces`, `host`, `ups`, `temperature` or `printer`.
    Snmp { group: String },
    /// The traffic of the host's network interfaces: the built-in `net`
    /// check of a server, the SNMP interfaces of a device (ADR-0149).
    Traffic,
    /// A vCenter or ESXi host read through its API from a probe: its hosts,
    /// virtual machines and datastores (ADR-0156). The host is a device.
    Vmware,
    /// The virtual machines of a Hyper-V host, read by its Windows agent
    /// (ADR-0156).
    HyperV,
}

impl Watch {
    /// The website check of `site`.
    pub fn web(site: Website) -> Self {
        Self::Web(Box::new(site))
    }
}

/// GET reads the page; HEAD only its status and headers.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "UPPERCASE")]
pub enum WebMethod {
    #[default]
    Get,
    Head,
}

impl WebMethod {
    fn is_get(&self) -> bool {
        *self == Self::Get
    }
}

/// Which addresses of a host name a website check tries.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum IpFamily {
    #[default]
    Any,
    V4,
    V6,
}

impl IpFamily {
    fn is_any(&self) -> bool {
        *self == Self::Any
    }
}

fn is_true(value: &bool) -> bool {
    *value
}

fn is_false(value: &bool) -> bool {
    !*value
}

/// Every status code: a check that only needs an answer.
pub const ANY_STATUS: &str = "100-599";

/// What a website check asks for and expects, as the source has it.
/// Readers note what they could not keep; the planner turns it into the
/// parameters of a check of kind `http`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Website {
    pub url: String,
    #[serde(skip_serializing_if = "WebMethod::is_get")]
    pub method: WebMethod,
    /// Status codes and ranges that pass, `200-299,301`; the check's
    /// default (`200-299`, or `200-399` without following redirects)
    /// when absent.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub status: Option<String>,
    /// Plain text the page must hold.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub contains: Option<String>,
    /// Plain text the page must not hold.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub not_contains: Option<String>,
    #[serde(skip_serializing_if = "is_true")]
    pub follow_redirects: bool,
    /// Extra request headers, name and value, in order.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub headers: Vec<(String, String)>,
    /// The user name of basic authentication, when the source has one.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub username: Option<String>,
    /// Whether the source sends a password or a token. Its value is never
    /// read into the inventory: the person sets it after the import.
    #[serde(skip_serializing_if = "is_false")]
    pub credentials: bool,
    #[serde(skip_serializing_if = "IpFamily::is_any")]
    pub ip: IpFamily,
    /// How long one run may take, when the source says.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub timeout_s: Option<u32>,
}

impl Website {
    /// A check of `url` with MonitorFlow's defaults: GET, a 2xx answer
    /// after following redirects.
    pub fn new(url: impl Into<String>) -> Self {
        Self {
            url: url.into(),
            method: WebMethod::Get,
            status: None,
            contains: None,
            not_contains: None,
            follow_redirects: true,
            headers: Vec::new(),
            username: None,
            credentials: false,
            ip: IpFamily::Any,
            timeout_s: None,
        }
    }

    /// Any answer passes: what a check of the certificate alone needs.
    pub fn any_status(mut self) -> Self {
        self.status = Some(ANY_STATUS.to_owned());
        self
    }
}

/// The alert that comes with a check: when it fails, an incident of this
/// severity after `for_s` seconds.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Alert {
    pub severity: Severity,
    pub for_s: u32,
    /// The alert is about the check sending nothing for `for_s` seconds,
    /// not about what it measures (ADR-0153).
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub no_data: bool,
}

/// Something a host is checked for.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Check {
    pub host: String,
    pub watch: Watch,
    /// How often, when the source says.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub interval_s: Option<u32>,
    /// The alert when it fails, for checks that pass or fail (services,
    /// ports, logs, web).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub alert: Option<Alert>,
    pub origin: Origin,
    #[serde(flatten)]
    pub fidelity: Fidelity,
}

/// A measure a threshold compares.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Measure {
    /// Share of processor time in use, all cores, 0 to 1.
    CpuUsage,
    /// Load average over 1, 5 or 15 minutes.
    CpuLoad { minutes: u8 },
    /// Share of memory available, 0 to 1.
    MemoryAvailable,
    /// Share of a file system's space in use, 0 to 1; every mount when
    /// `mount` is absent.
    DiskUsed {
        #[serde(skip_serializing_if = "Option::is_none")]
        mount: Option<String>,
    },
    /// Average wait of a disk's operations, in seconds; every disk when
    /// `device` is absent.
    DiskWait {
        #[serde(skip_serializing_if = "Option::is_none")]
        device: Option<String>,
    },
    /// How long the website check of `url` takes, in seconds.
    WebResponseTime { url: String },
    /// Days until the certificate of the website check of `url` expires.
    CertificateDays { url: String },
    /// Share of a vCenter's datastores' space that is free, 0 to 1, every
    /// datastore (ADR-0156).
    DatastoreFree,
}

impl Measure {
    /// The check that measures it: a built-in one, or the website check of
    /// its URL among `checks` (ADR-0150), as a silence names it.
    pub fn watch<'a>(&self, checks: impl IntoIterator<Item = &'a Check>) -> Watch {
        match self {
            Self::CpuUsage | Self::CpuLoad { .. } => Watch::Cpu,
            Self::MemoryAvailable => Watch::Memory,
            Self::DiskUsed { .. } => Watch::Disk,
            Self::DiskWait { .. } => Watch::DiskIo,
            Self::DatastoreFree => Watch::Vmware,
            Self::WebResponseTime { url } | Self::CertificateDays { url } => checks
                .into_iter()
                .find_map(|c| match &c.watch {
                    Watch::Web(site) if site.url == *url => Some(c.watch.clone()),
                    _ => None,
                })
                .unwrap_or_else(|| Watch::web(Website::new(url.clone()))),
        }
    }
}

/// Above or below.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Direction {
    Above,
    Below,
}

/// A function over a window (ADR-0153).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum WindowFunction {
    Avg,
    Min,
    Max,
    /// The last value minus the first one.
    Change,
    /// The change per second.
    Rate,
}

/// What a threshold judges (ADR-0153): the value by default.
#[derive(Debug, Clone, PartialEq, Default, Serialize)]
#[serde(tag = "judged", rename_all = "snake_case")]
pub enum Judged {
    /// The latest value.
    #[default]
    Value,
    /// A function of the values of the last `window_s` seconds.
    Window {
        function: WindowFunction,
        window_s: u32,
    },
    /// The average over `window_s` against the same `offset_s` earlier:
    /// divided (`ratio`) or subtracted.
    Compare {
        ratio: bool,
        offset_s: u32,
        window_s: u32,
    },
    /// Seconds left until the measure reaches `target` on the side of
    /// `direction`, from its trend over `window_s`; warning and critical
    /// are seconds left.
    Forecast { target: f64, window_s: u32 },
    /// Seconds without a value; warning and critical are seconds.
    NoData,
}

impl Judged {
    pub fn is_value(&self) -> bool {
        *self == Self::Value
    }
}

/// Another measure of the same host that must also be past a value, for
/// a threshold joined with "and" (ADR-0153).
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Condition {
    pub measure: Measure,
    pub direction: Direction,
    pub value: f64,
    /// A function over a window of seconds, else the latest value.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub window: Option<(WindowFunction, u32)>,
}

/// When a measure is a problem, on some hosts.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Threshold {
    /// What is judged: the value, a window, a forecast...
    #[serde(flatten, skip_serializing_if = "Judged::is_value")]
    pub judged: Judged,
    /// More conditions joined with "and", at most two.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub conditions: Vec<Condition>,
    /// The hosts it applies to.
    pub hosts: Vec<String>,
    pub measure: Measure,
    pub direction: Direction,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub warning: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub critical: Option<f64>,
    /// How long it must last first.
    pub for_s: u32,
    /// The value the measure must come back past for the problem to end,
    /// when the source has one.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub recovery: Option<f64>,
    pub origin: Origin,
    #[serde(flatten)]
    pub fidelity: Fidelity,
}

/// What a silence covers.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "kind", content = "hosts", rename_all = "snake_case")]
pub enum SilenceScope {
    Everything,
    Hosts(Vec<String>),
}

/// No notifications for a while: a one-time maintenance or a silence.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Silence {
    pub scope: SilenceScope,
    /// Only what these watch on the hosts (ADR-0151); empty: everything.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub checks: Vec<Watch>,
    pub starts_at: DateTime<Utc>,
    pub ends_at: DateTime<Utc>,
    pub reason: String,
    pub origin: Origin,
    #[serde(flatten)]
    pub fidelity: Fidelity,
}

/// How a maintenance window comes back.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Repeat {
    Daily,
    Weekly,
    Monthly,
}

/// When a maintenance window comes back, as MonitorFlow's rules say it
/// (ADR-0151).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Rule {
    pub repeat: Repeat,
    /// Every N days or weeks; 1 for monthly.
    pub every: u16,
    /// A bit per weekday, Monday first: the days of a weekly rule, the one
    /// weekday of a monthly one by week.
    pub weekdays: u8,
    /// Monthly, on this day.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub month_day: Option<u8>,
    /// Monthly, in this week (1 to 4, 5 the last).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub month_week: Option<u8>,
    /// A bit per month, January first.
    pub months: u16,
    /// Local start.
    pub start_time: NaiveTime,
    /// At most a week.
    pub duration_minutes: u32,
    /// The IANA zone when the source says it; `None`: the zone chosen
    /// for the import, as the source keeps its server's local time.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub time_zone: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub starts_on: Option<NaiveDate>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ends_on: Option<NaiveDate>,
}

/// Longest window of a rule, in minutes.
pub const MAX_WINDOW_MINUTES: u32 = 7 * 24 * 60;
/// Every month of a rule.
pub const EVERY_MONTH: u16 = 0xfff;

/// A maintenance that comes back: a recurring Zabbix maintenance,
/// an Alertmanager time interval, a PRTG schedule.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Window {
    pub scope: SilenceScope,
    /// Only what these watch on the hosts; empty: everything.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub checks: Vec<Watch>,
    pub rule: Rule,
    pub reason: String,
    pub origin: Origin,
    #[serde(flatten)]
    pub fidelity: Fidelity,
}

/// A mail server.
#[derive(Clone, PartialEq, Serialize)]
pub struct Smtp {
    pub host: String,
    pub port: u16,
    /// Empty without authentication.
    pub username: String,
    /// The sender address; the username when empty.
    pub from: String,
    /// Never shown, never logged.
    #[serde(skip)]
    pub password: Option<Secret>,
}

impl fmt::Debug for Smtp {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Smtp")
            .field("host", &self.host)
            .field("port", &self.port)
            .field("username", &self.username)
            .field("from", &self.from)
            .field("password", &self.password.as_ref().map(|_| "[redacted]"))
            .finish()
    }
}

/// Where a channel sends.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Target {
    /// Mail to `to`, through `smtp`; without a mail server it cannot be
    /// moved.
    Email {
        to: Vec<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        smtp: Option<Smtp>,
    },
    /// A JSON POST to a URL. Its path may hold a token: shown by origin
    /// only.
    Webhook {
        #[serde(skip)]
        url: Secret,
    },
    /// Chat incoming webhooks, whose URL is the secret.
    Slack {
        #[serde(skip)]
        url: Secret,
    },
    Teams {
        #[serde(skip)]
        url: Secret,
    },
    Discord {
        #[serde(skip)]
        url: Secret,
    },
}

impl Target {
    /// `email`, `webhook`, `slack`, `teams` or `discord`, as MonitorFlow
    /// names channel kinds.
    pub fn kind(&self) -> &'static str {
        match self {
            Self::Email { .. } => "email",
            Self::Webhook { .. } => "webhook",
            Self::Slack { .. } => "slack",
            Self::Teams { .. } => "teams",
            Self::Discord { .. } => "discord",
        }
    }
}

/// Where notifications go.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Channel {
    /// Its name in the source, for the person who reads the preview.
    pub name: String,
    pub target: Target,
    pub origin: Origin,
    #[serde(flatten)]
    pub fidelity: Fidelity,
}

/// A member of a business service: a whole host, or one of its checks.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Member {
    /// The host's [`Host::key`].
    pub host: String,
    /// One check of it; `None` for the whole host.
    pub check: Option<Watch>,
}

/// A business service (ADR-0155): a named group with one availability.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Service {
    pub name: String,
    pub members: Vec<Member>,
    /// Down while fewer than this many members are up; `None` for all of
    /// them.
    pub at_least: Option<u32>,
    /// The SLA target in percent, like 99.9.
    pub target: Option<f64>,
    pub origin: Origin,
    #[serde(flatten)]
    pub fidelity: Fidelity,
}

/// Something the source does that MonitorFlow cannot.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Unmovable {
    /// The hosts it concerned; empty when not about hosts.
    pub hosts: Vec<String>,
    pub origin: Origin,
    pub why: Note,
}

/// One end of a dependency (ADR-0152): a host, or one of its checks.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct DependencyEnd {
    pub host: String,
    /// The check it is, by what it watches; `None` for all of the host.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub watch: Option<Watch>,
}

impl DependencyEnd {
    pub fn host(host: impl Into<String>) -> Self {
        Self {
            host: host.into(),
            watch: None,
        }
    }

    /// One check of `host`; the agent answering is the host itself.
    pub fn check(host: impl Into<String>, watch: Watch) -> Self {
        Self {
            host: host.into(),
            watch: (watch != Watch::Agent).then_some(watch),
        }
    }
}

impl DependencyEnd {
    /// The parent `parent` for `child`, where `watch` is what it is: a
    /// host as a whole is behind its own checks only one check at a time,
    /// so a check of a host that depends on that host's ping depends on
    /// the ping check. `None` when a host would depend on itself.
    pub fn parent_for(parent: Self, watch: &Watch, child: &Self) -> Option<Self> {
        if parent.host != child.host || parent.watch.is_some() {
            return (parent != *child).then_some(parent);
        }
        let own = Self::check(parent.host, watch.clone());
        (child.watch.is_some() && own.watch.is_some() && own != *child).then_some(own)
    }
}

/// `child` is behind `parent`: while the parent is down, the child opens
/// no incident of its own (ADR-0152).
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Dependency {
    pub child: DependencyEnd,
    pub parent: DependencyEnd,
    pub origin: Origin,
    #[serde(flatten)]
    pub fidelity: Fidelity,
}

/// Arguments of a [`Note`], built with [`args!`](crate::args).
pub type Args = BTreeMap<String, String>;
