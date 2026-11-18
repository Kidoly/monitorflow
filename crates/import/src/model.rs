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
    /// A web page or API answering. MonitorFlow checks that its port
    /// answers, which the preview says.
    Web { url: String },
}

/// The alert that comes with a check: when it fails, an incident of this
/// severity after `for_s` seconds.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Alert {
    pub severity: Severity,
    pub for_s: u32,
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
}

/// Above or below.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Direction {
    Above,
    Below,
}

/// When a measure is a problem, on some hosts.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Threshold {
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

/// Something the source does that MonitorFlow cannot.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Unmovable {
    /// The hosts it concerned; empty when not about hosts.
    pub hosts: Vec<String>,
    pub origin: Origin,
    pub why: Note,
}

/// Arguments of a [`Note`], built with [`args!`](crate::args).
pub type Args = BTreeMap<String, String>;
