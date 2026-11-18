//! Moving from another monitoring tool (ADR-0146): the plan made from a
//! reader's [`Inventory`], stored as a preview, then confirmed or undone.
//!
//! [`plan`] is pure: it takes what MonitorFlow has already
//! ([`Existing`]) and returns the items of the preview, each with what it
//! would create. [`store`] reads and writes runs.

pub mod plan;
pub mod store;

use std::collections::BTreeMap;

use chrono::{DateTime, NaiveDate, Utc};
use monitorflow_import::model;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use uuid::Uuid;

pub use crate::notify::silence::CheckRef;
pub use plan::{plan, plan_in, Existing, Planned};

/// A preview older than this cannot be confirmed: the source may have
/// changed, and so may MonitorFlow.
pub const PREVIEW_HOURS: i64 = 24;
/// Most items in one preview.
pub const MAX_ITEMS: usize = 100_000;

/// Which of the three lists of the preview an item is in.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "snake_case")]
#[schema(as = ImportList)]
pub enum List {
    /// MonitorFlow does the same.
    Exact,
    /// MonitorFlow does something close; the note says how it differs.
    Approximated,
    /// MonitorFlow cannot do it; the note says why.
    NotMovable,
}

/// A note: a code the web interface translates, its arguments, and the
/// English text.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
#[schema(as = ImportNote)]
pub struct NoteView {
    /// One of the codes of `crates/import/src/notes.rs`.
    pub code: String,
    #[serde(default)]
    pub args: BTreeMap<String, String>,
    /// In English.
    pub text: String,
}

impl From<&monitorflow_import::Note> for NoteView {
    fn from(note: &monitorflow_import::Note) -> Self {
        Self {
            code: note.code.clone(),
            args: note.args.clone(),
            text: note.text(),
        }
    }
}

/// Where an item came from in the source.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
#[schema(as = ImportOrigin)]
pub struct OriginView {
    /// What it is there: `host`, `item`, `trigger`, `template`,
    /// `web_scenario`, `maintenance`, `media_type`, `user_media`,
    /// `scrape_target`, `alerting_rule`, `receiver`, `silence`, `sensor`,
    /// `notification`, `device`, `group`, `probe`, `time_interval`,
    /// `schedule`, `discovery_rule` or `action`.
    pub kind: String,
    /// Its name there.
    pub name: String,
    /// The key, expression or setting it was read from.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
}

impl From<&model::Origin> for OriginView {
    fn from(origin: &model::Origin) -> Self {
        Self {
            kind: origin.kind.clone(),
            name: origin.name.clone(),
            detail: origin.detail.clone(),
        }
    }
}

/// What an item creates.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(tag = "object", rename_all = "snake_case")]
#[schema(as = ImportCreate)]
pub enum Create {
    /// A server waiting for its agent.
    Server {
        /// Its ID, which an agent takes over by enrolling with it.
        peer_id: String,
        hostname: String,
        /// `debian`, `rhel`, `windows` or `linux`.
        os_family: String,
        /// Addresses the source had, for the person who installs the agent.
        addresses: Vec<String>,
    },
    /// A device: an address without an agent, watched from a probe
    /// (ADR-0148).
    Device {
        peer_id: String,
        hostname: String,
        /// What its checks reach.
        address: String,
    },
    /// A check (a sensor row) of a server.
    Check {
        /// The server's ID.
        server: String,
        kind: String,
        name: String,
        interval_ms: u64,
        /// How long one run may take, when the source says; at most
        /// `interval_ms`.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        timeout_ms: Option<u64>,
        #[schema(value_type = Object)]
        params: Map<String, Value>,
        /// Run from the site's default probe, the relay of the site or the
        /// central server, rather than the agent (ADR-0148).
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        probe: bool,
    },
    /// A threshold of a server.
    Threshold {
        server: String,
        metric: String,
        instance: Option<String>,
        /// `>`, `>=`, `<` or `<=`.
        comparison: String,
        warning: Option<f64>,
        critical: Option<f64>,
        duration_s: u32,
        hysteresis: f64,
    },
    /// A silence of a server, or of the import's site when `server` is
    /// `null`.
    Silence {
        server: Option<String>,
        starts_at: DateTime<Utc>,
        ends_at: DateTime<Utc>,
        reason: String,
        /// Only these checks (ADR-0151); empty for everything.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        checks: Vec<CheckRef>,
    },
    /// A maintenance window of the import's site (ADR-0151), as
    /// `POST /api/maintenance-windows` takes it.
    Window {
        /// The servers' IDs; empty for the whole site.
        servers: Vec<String>,
        /// Only these checks; empty for everything.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        checks: Vec<CheckRef>,
        /// `daily`, `weekly` or `monthly`.
        repeat: String,
        every: u16,
        /// `monday` to `sunday`.
        weekdays: Vec<String>,
        month_day: Option<u8>,
        month_week: Option<u8>,
        /// 1 to 12; empty for every month.
        months: Vec<u8>,
        /// `HH:MM`, local.
        start_time: String,
        duration_minutes: u32,
        time_zone: String,
        starts_on: Option<NaiveDate>,
        ends_on: Option<NaiveDate>,
        reason: String,
    },
    /// A network scan of the import's site (ADR-0154), notifications
    /// off, run from the site's default probe.
    NetworkScan {
        name: String,
        /// Addresses, blocks and ranges, as `POST /api/network-scans`
        /// takes them.
        targets: Vec<String>,
        /// How many addresses they stand for.
        addresses: u32,
        ports: Vec<u16>,
        /// `hour`, `day` or `week`; `null`: only when asked.
        every: Option<String>,
        /// Where it runs from when the preview was made: `central`, or the
        /// name of the relay of the site added first.
        probe: String,
        /// Whether its own SNMP credentials come with it, kept sealed.
        has_snmp: bool,
    },
    /// A notification channel.
    Channel {
        /// `email`, `webhook`, `slack`, `teams` or `discord`.
        kind: String,
        /// As `POST /api/notifications/channels` takes them; a webhook's
        /// URL is shown by its origin only, and chat URLs not at all.
        #[schema(value_type = Object)]
        settings: Map<String, Value>,
        /// Whether a password or chat URL comes with it, kept sealed.
        has_secret: bool,
        /// The site it covers; `null` for everything.
        site_id: Option<Uuid>,
    },
}

impl Create {
    /// `server`, `check`, `threshold`, `silence`, `window`,
    /// `network_scan` or `channel`.
    pub fn object(&self) -> &'static str {
        match self {
            Self::Server { .. } | Self::Device { .. } => "server",
            Self::Check { .. } => "check",
            Self::Threshold { .. } => "threshold",
            Self::Silence { .. } => "silence",
            Self::Window { .. } => "window",
            Self::NetworkScan { .. } => "network_scan",
            Self::Channel { .. } => "channel",
        }
    }
}

/// One line of the preview.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, utoipa::ToSchema)]
#[schema(as = ImportItem)]
pub struct Item {
    /// Unique in the run; what `skip` names.
    pub id: String,
    /// `server`, `check`, `threshold`, `silence`, `window`,
    /// `network_scan`, `channel`, or what the source item was when
    /// nothing is created (`builtin` for what every server measures
    /// already, `other`).
    pub kind: String,
    pub list: List,
    /// How it is approximated, or why it cannot be moved.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<NoteView>,
    pub origin: OriginView,
    /// Names of the hosts it concerns, as the source names them.
    #[serde(default)]
    pub hosts: Vec<String>,
    /// MonitorFlow has it already: nothing is created.
    #[serde(default)]
    pub present: bool,
    /// Items it needs: unticking one of them leaves this one out.
    #[serde(default)]
    pub needs: Vec<String>,
    /// What it creates; `null` when nothing.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub create: Option<Create>,
}

impl Item {
    /// Whether confirming may create something for it.
    pub fn creates(&self) -> bool {
        self.create.is_some() && !self.present && self.list != List::NotMovable
    }
}

/// The secret of a channel or network scan item, kept sealed until the
/// confirmation.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct ChannelSecret {
    /// The full webhook URL, whose path may hold a token.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    /// The SMTP password or the chat webhook URL; a scan's SNMP
    /// credentials, as JSON the network scans API takes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub secret: Option<String>,
}
