//! What a Zabbix configuration holds, as read from exports or the API,
//! before it is mapped: one shape for both, so the mapping is written
//! once.

use std::collections::BTreeMap;

/// Everything read.
#[derive(Debug, Default, Clone)]
pub struct Config {
    pub hosts: Vec<Host>,
    /// Templates by technical name.
    pub templates: BTreeMap<String, Template>,
    pub global_macros: Vec<Macro>,
    /// Exports have none.
    pub maintenances: Vec<Maintenance>,
    pub media_types: Vec<MediaType>,
    /// Services and SLAs (Zabbix 6.0 and later), from the API only.
    pub services: Vec<Service>,
    pub slas: Vec<Sla>,
    /// Users' media, `None` when the source cannot say (exports).
    pub user_media: Option<Vec<UserMedia>>,
    /// Whether hosts' items, triggers and web scenarios include what their
    /// templates give them, as the API returns them. Exports list only a
    /// host's own and name its templates.
    pub inherited: bool,
}

/// A user macro: `{$NAME}` or `{$NAME:"context"}` and its value; `None`
/// for secret and vault macros, whose value is never given.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Macro {
    pub name: String,
    pub value: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InterfaceKind {
    Agent,
    Snmp,
    Ipmi,
    Jmx,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Interface {
    pub kind: InterfaceKind,
    pub ip: String,
    pub dns: String,
    /// Whether Zabbix connects by IP rather than DNS name.
    pub use_ip: bool,
}

#[derive(Debug, Clone, Default)]
pub struct Host {
    /// Technical name, unique in Zabbix.
    pub host: String,
    /// Visible name.
    pub name: String,
    pub enabled: bool,
    pub interfaces: Vec<Interface>,
    /// The inventory's OS field, when filled.
    pub os: Option<String>,
    pub groups: Vec<String>,
    /// Technical names of the templates linked to it.
    pub templates: Vec<String>,
    pub macros: Vec<Macro>,
    pub items: Vec<Item>,
    pub triggers: Vec<Trigger>,
    pub web: Vec<WebScenario>,
}

#[derive(Debug, Clone, Default)]
pub struct Template {
    /// Technical name.
    pub host: String,
    /// Templates linked to this one.
    pub templates: Vec<String>,
    pub macros: Vec<Macro>,
    pub items: Vec<Item>,
    pub triggers: Vec<Trigger>,
    pub web: Vec<WebScenario>,
}

/// How Zabbix collects an item.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ItemKind {
    /// Zabbix agent, passive or active.
    Agent,
    /// A check the Zabbix server makes itself (`net.tcp.service`, ICMP).
    Simple,
    Snmp,
    Ipmi,
    Jmx,
    /// Zabbix's own: `zabbix[host,agent,available]`.
    Internal,
    /// Computed from another item.
    Dependent,
    Calculated,
    HttpAgent,
    /// A command: external check, SSH, Telnet, script, browser.
    Command,
    /// Trapper, database monitor, SNMP trap and the like.
    Other,
}

#[derive(Debug, Clone)]
pub struct Item {
    pub name: String,
    pub key: String,
    pub kind: ItemKind,
    /// As written: `1m`, `30s;50s/1-5,09:00-18:00`, `{$DELAY}`.
    pub delay: Option<String>,
    pub enabled: bool,
    /// An item prototype of a discovery rule.
    pub prototype: bool,
    /// The request of an HTTP agent item.
    pub http: Option<Box<HttpAgent>>,
    /// The OID of an SNMP item, as written (`1.3.6.1.2.1.1.3.0`,
    /// `walk[1.3.6.1.2.1.2.2.1.8]`).
    pub snmp_oid: Option<String>,
    /// The template it comes from, when the API says.
    pub template: Option<String>,
}

/// Trigger severity, from Not classified (0) to Disaster (5).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Priority {
    NotClassified,
    Information,
    Warning,
    Average,
    High,
    Disaster,
}

impl Priority {
    /// From the API's number or the export's word.
    pub fn parse(text: &str) -> Self {
        match text.trim() {
            "1" | "INFO" => Self::Information,
            "2" | "WARNING" => Self::Warning,
            "3" | "AVERAGE" => Self::Average,
            "4" | "HIGH" => Self::High,
            "5" | "DISASTER" => Self::Disaster,
            _ => Self::NotClassified,
        }
    }

    /// As Zabbix shows it.
    pub fn label(self) -> &'static str {
        match self {
            Self::NotClassified => "Not classified",
            Self::Information => "Information",
            Self::Warning => "Warning",
            Self::Average => "Average",
            Self::High => "High",
            Self::Disaster => "Disaster",
        }
    }
}

#[derive(Debug, Clone)]
pub struct Trigger {
    pub name: String,
    pub expression: String,
    /// The recovery expression, when the trigger has one.
    pub recovery: Option<String>,
    pub priority: Priority,
    pub enabled: bool,
    pub prototype: bool,
    /// The template it comes from, when the API says.
    pub template: Option<String>,
    /// Its tags, name and value, which maintenances may select.
    pub tags: Vec<(String, String)>,
}

/// One HTTP request, as a web scenario's step or an HTTP agent item
/// makes it. Values are as written, macros unexpanded.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct WebRequest {
    /// The step's name; empty for an item.
    pub name: String,
    pub url: String,
    /// Query fields Zabbix adds to the URL.
    pub query: Vec<(String, String)>,
    /// `GET`, `POST`, `PUT` or `HEAD`.
    pub method: String,
    /// Whether it sends a body (post fields or raw data).
    pub body: bool,
    /// Codes that pass, `200,301-302`; empty passes any.
    pub status_codes: String,
    /// A regular expression the answer must match; empty for none.
    pub required: String,
    pub follow_redirects: bool,
    /// Whether only the headers are read (retrieve mode Headers).
    pub headers_only: bool,
    pub headers: Vec<(String, String)>,
    /// Whether it extracts variables, which later steps use.
    pub variables: bool,
    /// As written: `15s`, `{$TIMEOUT}`; `None` for the default.
    pub timeout: Option<String>,
}

/// How a request authenticates.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct WebAuth {
    /// `basic`, `ntlm`, `kerberos`, `digest`; empty for none.
    pub kind: String,
    pub user: String,
    /// Whether the source has a password. Its value is never kept.
    pub password: bool,
}

/// An HTTP agent item's request.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct HttpAgent {
    pub request: WebRequest,
    pub auth: WebAuth,
}

#[derive(Debug, Clone)]
pub struct WebScenario {
    pub name: String,
    pub delay: Option<String>,
    pub enabled: bool,
    /// Its steps, in order.
    pub steps: Vec<WebRequest>,
    /// Headers every step sends.
    pub headers: Vec<(String, String)>,
    /// Whether the scenario defines variables for its steps.
    pub variables: bool,
    pub auth: WebAuth,
}

/// How a maintenance period repeats.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PeriodKind {
    Once,
    Daily,
    Weekly,
    Monthly,
}

/// One time period of a maintenance, in Zabbix's terms.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Period {
    pub kind: PeriodKind,
    /// Every N days or weeks; for a monthly period by weekday, the week
    /// of the month (1 to 4, 5 for the last).
    pub every: u32,
    /// Months, January in bit 0.
    pub months: u32,
    /// Weekdays, Monday in bit 0.
    pub weekdays: u32,
    /// Day of the month, 0 when by weekday.
    pub day: u32,
    /// Seconds after midnight it starts.
    pub start_time: u32,
    /// Its length in seconds.
    pub length: u32,
    /// Unix time of a one-time period's start.
    pub start_date: i64,
}

#[derive(Debug, Clone)]
pub struct Maintenance {
    pub name: String,
    pub description: String,
    pub active_since: i64,
    pub active_till: i64,
    /// Technical names of its hosts.
    pub hosts: Vec<String>,
    /// Names of its host groups.
    pub groups: Vec<String>,
    pub periods: Vec<Period>,
    /// Problem tags it is limited to, as `tag=value`, for people.
    pub tags: Vec<String>,
    /// The same, to match: name, value, and whether the value is only
    /// contained (Zabbix's "contains") rather than equal.
    pub tag_filters: Vec<TagFilter>,
    /// Problems need every tag name (Zabbix's "And/Or"), not one of them.
    pub tags_all: bool,
    /// Zabbix keeps collecting data during it.
    pub collect_data: bool,
}

/// A Zabbix service (6.0 and later), as `service.get` gives it.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Service {
    pub id: String,
    pub name: String,
    /// 0: set the status to OK, 1: most critical if all children have
    /// problems, 2: most critical of child services.
    pub algorithm: u8,
    /// The problems it watches: all of these tags must match.
    pub problem_tags: Vec<TagFilter>,
    /// IDs of its child services.
    pub children: Vec<String>,
    pub status_rules: Vec<StatusRule>,
    /// Its own tags, which SLAs select it by.
    pub tags: Vec<(String, String)>,
    /// Whether its children have weights other than 0.
    pub weighted: bool,
}

/// An additional rule of a service: `kind` is Zabbix's type (0: at least
/// N children have a problem, 2: fewer than N children are OK, others by
/// percentage or weight), `limit` its N.
#[derive(Debug, Clone, PartialEq)]
pub struct StatusRule {
    pub kind: u8,
    pub limit: f64,
}

/// A Zabbix SLA: a target over the services its tags select.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Sla {
    pub name: String,
    pub enabled: bool,
    /// The target in percent.
    pub slo: f64,
    /// Whether it counts only some hours (a schedule other than 24x7).
    pub scheduled: bool,
    /// How many excluded downtimes it has.
    pub excluded: usize,
    pub service_tags: Vec<TagFilter>,
}

/// One tag condition of a maintenance.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TagFilter {
    pub tag: String,
    pub value: String,
    pub contains: bool,
}

impl TagFilter {
    /// Whether a problem tag passes it.
    pub fn matches(&self, tag: &str, value: &str) -> bool {
        tag == self.tag
            && if self.contains {
                value.contains(&self.value)
            } else {
                value == self.value
            }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MediaKind {
    Email,
    Script,
    Sms,
    Webhook,
}

#[derive(Debug, Clone)]
pub struct MediaType {
    pub name: String,
    pub kind: MediaKind,
    pub enabled: bool,
    pub smtp_server: String,
    pub smtp_port: u16,
    pub smtp_email: String,
    /// Empty without authentication.
    pub username: String,
    pub password: Option<String>,
    /// Webhook parameters, name and value.
    pub parameters: Vec<(String, String)>,
}

/// A user's media: where one user is sent notifications of a media type.
#[derive(Debug, Clone)]
pub struct UserMedia {
    pub user: String,
    pub media_type: String,
    pub sendto: Vec<String>,
    pub enabled: bool,
}
