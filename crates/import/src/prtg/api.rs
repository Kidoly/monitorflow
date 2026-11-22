//! Reads a PRTG installation through its HTTP API into a [`Tree`]. Only
//! GET requests of reading endpoints are sent: tables, object
//! properties, the status and notification settings. Nothing is changed.
//!
//! The requests go through [`Fetch`], so tests replay recorded answers
//! and the real [`super::client::Client`] stays small.

use std::collections::BTreeMap;
use std::sync::Arc;

use chrono::{DateTime, Duration, NaiveDateTime, Utc};
use serde_json::Value;
use tokio::task::JoinSet;

use crate::source::ReadError;

use super::sensors::wanted;
use super::tree::{
    Channel, Delivery, Depends, Kind, Limits, Maintenance, Notification, Object, Paused, Tree,
};

/// Requests in flight at once, to keep the load on PRTG low.
const PARALLEL: usize = 4;

/// One GET request of the API, authenticated by the implementation.
#[async_trait::async_trait]
pub trait Fetch: Send + Sync {
    /// The body of `path` (like `/api/table.json`) with `query`.
    async fn get(&self, path: &str, query: &[(&str, String)]) -> Result<String, ReadError>;
}

fn unreadable(what: &str, why: impl std::fmt::Display) -> ReadError {
    ReadError::Connect(format!(
        "PRTG's answer to the {what} could not be read: {why}."
    ))
}

/// The rows of a `table.json` answer: its one array, whatever its key.
pub fn table_rows(body: &str, what: &str) -> Result<Vec<Value>, ReadError> {
    let value: Value = serde_json::from_str(body).map_err(|e| unreadable(what, e))?;
    value
        .as_object()
        .and_then(|o| o.values().find_map(|v| v.as_array().cloned()))
        .ok_or_else(|| unreadable(what, "no table in the answer"))
}

/// `field_raw` when present, else `field`.
fn raw<'a>(row: &'a Value, field: &str) -> Option<&'a Value> {
    row.get(format!("{field}_raw")).or_else(|| row.get(field))
}

fn int(row: &Value, field: &str) -> Option<i64> {
    match raw(row, field)? {
        Value::Number(n) => n.as_i64(),
        Value::String(s) => s.trim().parse().ok(),
        _ => None,
    }
}

fn text(row: &Value, field: &str) -> Option<String> {
    match raw(row, field)? {
        Value::String(s) => Some(s.trim().to_owned()),
        Value::Number(n) => Some(n.to_string()),
        _ => None,
    }
}

/// Paused or not, from PRTG's status code: 7 paused by a person, 9 by a
/// schedule, 12 paused until a time. 8, paused by a dependency, is the
/// state of the moment: the dependency itself is read (ADR-0152).
fn paused(status: Option<i64>) -> Option<Paused> {
    match status? {
        7 | 12 => Some(Paused::Indefinitely),
        9 => Some(Paused::ByRule),
        _ => None,
    }
}

/// An object's dependency setting, when it is not "use parent".
async fn depends(fetch: &dyn Fetch, id: i64) -> Result<Option<Depends>, ReadError> {
    let kind = get_property(fetch, id, "dependencytype").await?;
    let object = match kind.as_deref() {
        Some("1") => get_property(fetch, id, "dependency").await?,
        _ => None,
    };
    Ok(Depends::parse(kind.as_deref(), object.as_deref()))
}

/// The objects of one table.
pub fn objects(body: &str, kind: Kind) -> Result<Vec<Object>, ReadError> {
    let what = format!("{kind:?} list").to_lowercase();
    let mut out = Vec::new();
    for row in table_rows(body, &what)? {
        let Some(id) = int(&row, "objid") else {
            continue;
        };
        let name = text(&row, "name").unwrap_or_else(|| format!("#{id}"));
        let parent = match kind {
            // A probe is a child of the root. Without a filter, PRTG's
            // `probenodes` table lists every node of the tree too, and
            // some versions ignore the filter: those rows are left out.
            Kind::Probe => match int(&row, "parentid") {
                Some(p) if p != 0 => continue,
                _ => None,
            },
            _ => int(&row, "parentid"),
        };
        let mut object = Object::new(id, kind, name, parent);
        object.paused = paused(int(&row, "status"));
        if kind == Kind::Device {
            object.host = text(&row, "host").filter(|h| !h.is_empty());
        }
        if kind == Kind::Sensor {
            object.sensor_type = text(&row, "type").map(|t| t.to_ascii_lowercase());
            object.interval_s = int(&row, "interval").and_then(|i| u32::try_from(i).ok());
        }
        out.push(object);
    }
    Ok(out)
}

/// The value of a `getobjectproperty.htm` answer; `None` when the
/// object has no such property.
pub fn property(body: &str) -> Option<String> {
    let start = body.find("<result>")? + "<result>".len();
    let end = start + body[start..].find("</result>")?;
    let value = unescape(body[start..end].trim());
    // PRTG 24 answers `Not found`, without parentheses, for a channel
    // property the channel does not have, like `valuelookup`.
    let missing = value.is_empty()
        || value.starts_with("(Property not found)")
        || value == "(Not found)"
        || value == "Not found";
    (!missing).then_some(value)
}

/// The five XML entities and numeric character references.
pub fn unescape(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(at) = rest.find('&') {
        out.push_str(&rest[..at]);
        rest = &rest[at..];
        let Some(end) = rest.find(';').filter(|e| *e <= 10) else {
            out.push('&');
            rest = &rest[1..];
            continue;
        };
        match crate::xml::entity(&rest[1..end]) {
            Some(c) => {
                out.push(c);
                rest = &rest[end + 1..];
            }
            None => {
                out.push('&');
                rest = &rest[1..];
            }
        }
    }
    out.push_str(rest);
    out
}

/// A number written by PRTG, with a decimal point or comma.
pub fn number(text: &str) -> Option<f64> {
    let text = text.trim();
    let text = if text.contains('.') {
        text.replace(',', "")
    } else {
        text.replace(',', ".")
    };
    text.parse().ok().filter(|v: &f64| v.is_finite())
}

/// The unit after the number of a displayed value, like `%` in `12 %`.
pub fn unit(display: &str) -> String {
    display
        .trim_start_matches(|c: char| c.is_ascii_digit() || " .,-<>".contains(c))
        .trim()
        .to_owned()
}

/// A date of PRTG's settings, `2026-11-20-22-00-00`, in PRTG's local time.
pub fn setting_date(text: &str) -> Option<NaiveDateTime> {
    NaiveDateTime::parse_from_str(text.trim(), "%Y-%m-%d-%H-%M-%S").ok()
}

/// PRTG's clock as shown by `status.json`, in its local time, in the
/// formats its languages use.
pub fn clock(text: &str) -> Option<NaiveDateTime> {
    const FORMATS: &[&str] = &[
        "%m/%d/%Y %I:%M:%S %p",
        "%d.%m.%Y %H:%M:%S",
        "%d/%m/%Y %H:%M:%S",
        "%Y-%m-%d %H:%M:%S",
        "%m/%d/%Y %H:%M:%S",
    ];
    FORMATS
        .iter()
        .find_map(|f| NaiveDateTime::parse_from_str(text.trim(), f).ok())
}

/// How far PRTG's local time is ahead of UTC, rounded to the quarter
/// hour, from its clock and ours.
pub fn utc_offset(prtg_clock: NaiveDateTime, now: DateTime<Utc>) -> Duration {
    let ahead = (prtg_clock - now.naive_utc()).num_minutes();
    Duration::minutes((ahead as f64 / 15.0).round() as i64 * 15)
}

async fn get_property(fetch: &dyn Fetch, id: i64, name: &str) -> Result<Option<String>, ReadError> {
    let body = fetch
        .get(
            "/api/getobjectproperty.htm",
            &[
                ("id", id.to_string()),
                ("name", name.to_owned()),
                ("show", "nohtmlencode".into()),
            ],
        )
        .await?;
    Ok(property(&body))
}

async fn channel_limit(
    fetch: &dyn Fetch,
    sensor: i64,
    channel: i64,
    name: &str,
) -> Result<Option<String>, ReadError> {
    let body = fetch
        .get(
            "/api/getobjectproperty.htm",
            &[
                ("id", sensor.to_string()),
                ("subtype", "channel".into()),
                ("subid", channel.to_string()),
                ("name", name.to_owned()),
                ("show", "nohtmlencode".into()),
            ],
        )
        .await?;
    Ok(property(&body))
}

async fn channels(
    fetch: &dyn Fetch,
    sensor: i64,
    custom_snmp: bool,
) -> Result<Vec<Channel>, ReadError> {
    let body = fetch
        .get(
            "/api/table.json",
            &[
                ("content", "channels".into()),
                ("id", sensor.to_string()),
                ("columns", "objid,name,lastvalue".into()),
            ],
        )
        .await?;
    let mut out = Vec::new();
    for row in table_rows(&body, "channel list")? {
        let Some(id) = int(&row, "objid") else {
            continue;
        };
        let name = text(&row, "name").unwrap_or_default();
        // The displayed value carries the unit; the raw one does not.
        let display = row.get("lastvalue").and_then(Value::as_str).unwrap_or("");
        let mut channel = Channel {
            id,
            name,
            unit: unit(display),
            limits: None,
            ..Default::default()
        };
        if channel_limit(fetch, sensor, id, "limitmode")
            .await?
            .as_deref()
            == Some("1")
        {
            let get = async |name| -> Result<Option<f64>, ReadError> {
                Ok(channel_limit(fetch, sensor, id, name)
                    .await?
                    .as_deref()
                    .and_then(number))
            };
            channel.limits = Some(Limits {
                max_error: get("limitmaxerror").await?,
                max_warning: get("limitmaxwarning").await?,
                min_error: get("limitminerror").await?,
                min_warning: get("limitminwarning").await?,
            });
        }
        if custom_snmp {
            let get = async |name| channel_limit(fetch, sensor, id, name).await;
            channel.lookup = get("valuelookup")
                .await?
                .map(|v| v.trim().to_ascii_lowercase())
                .filter(|v| !v.is_empty() && v != "none")
                .map(|id| crate::model::SnmpLookup {
                    name: id,
                    builtin: None,
                    entries: Vec::new(),
                    otherwise: crate::model::SnmpState::Warning,
                });
            channel.factor_m = get("factorm").await?.as_deref().and_then(number);
            channel.factor_d = get("factord").await?.as_deref().and_then(number);
        }
        out.push(channel);
    }
    Ok(out)
}

/// Fills a sensor's settings and channels.
async fn sensor_details(fetch: &dyn Fetch, mut sensor: Object) -> Result<Object, ReadError> {
    let sensor_type = sensor.sensor_type.clone().unwrap_or_default();
    let (settings, with_channels) = wanted(&sensor_type);
    let custom_snmp = super::sensors::family(&sensor_type) == super::sensors::Family::SnmpCustom;
    for name in settings {
        // Numbered channels of a custom SNMP sensor are asked for only
        // while they exist (ADR-0157).
        if custom_snmp {
            if let Some(n) = name
                .strip_prefix("channel")
                .or_else(|| name.strip_prefix("usechannel"))
                .and_then(|rest| rest.split(|c: char| !c.is_ascii_digit()).next())
                .and_then(|d| d.parse::<usize>().ok())
            {
                let known =
                    |field: &str| sensor.settings.contains_key(&format!("channel{n}{field}"));
                let first = name.ends_with("oid") || name.ends_with("column");
                if !first && !known("oid") && !known("column") {
                    continue;
                }
            }
        }
        if let Some(value) = get_property(fetch, sensor.id, name).await? {
            sensor.settings.insert((*name).to_owned(), value);
        }
    }
    if with_channels {
        sensor.channels = channels(fetch, sensor.id, custom_snmp).await?;
    }
    sensor.depends = depends(fetch, sensor.id).await?;
    Ok(sensor)
}

/// Fills the maintenance window of a probe, group or device.
async fn maintenance(
    fetch: &dyn Fetch,
    mut object: Object,
    offset: Duration,
) -> Result<Object, ReadError> {
    if get_property(fetch, object.id, "maintenable")
        .await?
        .as_deref()
        == Some("1")
    {
        let start = get_property(fetch, object.id, "maintstart").await?;
        let end = get_property(fetch, object.id, "maintend").await?;
        let utc = |t: Option<String>| {
            t.as_deref()
                .and_then(setting_date)
                .map(|d| (d - offset).and_utc())
        };
        if let (Some(start), Some(end)) = (utc(start), utc(end)) {
            object.maintenance = Some(Maintenance { start, end });
        }
    }
    if object.kind != Kind::Probe {
        object.depends = depends(fetch, object.id).await?;
    }
    Ok(object)
}

/// Fills the auto-discovery settings of a probe or group that has it on.
async fn discovery(fetch: &dyn Fetch, mut object: Object) -> Result<Object, ReadError> {
    if object.kind == Kind::Device {
        return Ok(object);
    }
    let Some(kind) = get_property(fetch, object.id, "discoverytype").await? else {
        return Ok(object);
    };
    object.settings.insert("discoverytype".into(), kind);
    if !super::discovery::discovers(&object) {
        return Ok(object);
    }
    for name in &super::discovery::SETTINGS[1..] {
        if let Some(value) = get_property(fetch, object.id, name).await? {
            object.settings.insert((*name).to_owned(), value);
        }
    }
    Ok(object)
}

/// The deliveries a notification template's settings turn on, from the
/// fields of its settings page: addresses in its email fields and URLs
/// in its HTTP fields.
pub fn deliveries(fields: &BTreeMap<String, String>) -> Vec<Delivery> {
    let mut emails = Vec::new();
    let mut out = Vec::new();
    for (name, value) in fields {
        let name = name.to_ascii_lowercase();
        if name.contains("email") || name.contains("address") {
            for part in value.split([',', ';', ' ']) {
                let part = part.trim();
                if part.contains('@')
                    && !part.contains('%')
                    && !emails.iter().any(|e: &String| e.eq_ignore_ascii_case(part))
                {
                    emails.push(part.to_owned());
                }
            }
        } else if name.contains("url")
            && (value.starts_with("https://") || value.starts_with("http://"))
        {
            out.push(Delivery::Http(value.trim().to_owned()));
        } else if (name.contains("phone") || name.contains("sms"))
            && !value.trim().is_empty()
            && !out.contains(&Delivery::Other("SMS".into()))
        {
            out.push(Delivery::Other("SMS".into()));
        }
    }
    if !emails.is_empty() {
        out.insert(0, Delivery::Email(emails));
    }
    out
}

/// The `name` and `value` of the inputs and text areas of an HTML form,
/// leaving out unchecked boxes and radio buttons.
pub fn form_fields(html: &str) -> BTreeMap<String, String> {
    let mut out = BTreeMap::new();
    let attribute = |tag: &str, name: &str| -> Option<String> {
        let pattern = format!(" {name}=\"");
        let start = tag.find(&pattern)? + pattern.len();
        let end = start + tag[start..].find('"')?;
        Some(unescape(&tag[start..end]))
    };
    let mut rest = html;
    while let Some(at) = rest.find('<') {
        rest = &rest[at..];
        let Some(end) = rest.find('>') else { break };
        let tag = &rest[..end];
        let lower = tag.to_ascii_lowercase();
        if lower.starts_with("<input") {
            let kind = attribute(tag, "type")
                .unwrap_or_default()
                .to_ascii_lowercase();
            let checked = lower.contains(" checked");
            if let Some(name) = attribute(tag, "name") {
                if !(kind == "checkbox" || kind == "radio") || checked {
                    out.insert(name, attribute(tag, "value").unwrap_or_default());
                }
            }
        } else if lower.starts_with("<textarea") {
            if let Some(name) = attribute(tag, "name") {
                let body = &rest[end + 1..];
                let close = body.find("</textarea>").unwrap_or(body.len());
                out.insert(name, unescape(&body[..close]));
            }
        }
        rest = &rest[end..];
    }
    out
}

async fn notifications(fetch: &dyn Fetch) -> Result<Vec<Notification>, ReadError> {
    let body = fetch
        .get(
            "/api/table.json",
            &[
                ("content", "notifications".into()),
                ("columns", "objid,name,active".into()),
            ],
        )
        .await?;
    let mut out = Vec::new();
    for row in table_rows(&body, "notification list")? {
        let Some(id) = int(&row, "objid") else {
            continue;
        };
        let active = !matches!(raw(&row, "active"), Some(Value::Bool(false)))
            && !matches!(text(&row, "active").as_deref(), Some("0" | "false"));
        let page = fetch
            .get("/controls/editnotification.htm", &[("id", id.to_string())])
            .await?;
        out.push(Notification {
            id,
            name: text(&row, "name").unwrap_or_else(|| format!("#{id}")),
            active,
            deliveries: deliveries(&form_fields(&page)),
        });
    }
    Ok(out)
}

/// Runs `work` on every object, [`PARALLEL`] at a time, in the order of
/// `objects`.
async fn each<F, Fut>(objects: Vec<Object>, work: F) -> Result<Vec<Object>, ReadError>
where
    F: Fn(Object) -> Fut,
    Fut: std::future::Future<Output = Result<Object, ReadError>> + Send + 'static,
{
    let mut out: Vec<Option<Object>> = vec![None; objects.len()];
    let mut tasks = JoinSet::new();
    for (i, object) in objects.into_iter().enumerate() {
        if tasks.len() >= PARALLEL {
            let Some(done) = tasks.join_next().await else {
                break;
            };
            let (i, object) = done.map_err(|e| unreadable("requests", e))??;
            out[i] = Some(object);
        }
        let future = work(object);
        tasks.spawn(async move { future.await.map(|o| (i, o)) });
    }
    while let Some(done) = tasks.join_next().await {
        let (i, object) = done.map_err(|e| unreadable("requests", e))??;
        out[i] = Some(object);
    }
    Ok(out.into_iter().flatten().collect())
}

/// How far PRTG's local time is ahead of UTC, from a `status.json`
/// answer: its local `Clock` against `jsClock`, the same instant in Unix
/// seconds, or against `now` when it has no `jsClock`.
pub fn status_offset(status: &str, now: DateTime<Utc>) -> Option<Duration> {
    let status: Value = serde_json::from_str(status).ok()?;
    let local = status
        .get("Clock")
        .and_then(Value::as_str)
        .and_then(clock)?;
    let at = status
        .get("jsClock")
        .and_then(Value::as_i64)
        .and_then(|s| DateTime::from_timestamp(s, 0))
        .unwrap_or(now);
    Some(utc_offset(local, at))
}

/// Reads the whole installation. `now` is used to learn PRTG's time
/// zone from its clock, for maintenance windows.
pub async fn read(fetch: Arc<dyn Fetch>, now: DateTime<Utc>) -> Result<Tree, ReadError> {
    // `status.json` is PRTG's status; `getstatus.json` does not exist and
    // answers 404. Only the time zone comes from it: when it cannot be
    // read, windows are read as UTC, but refused credentials stop here.
    let offset = match fetch.get("/api/status.json", &[]).await {
        Ok(status) => status_offset(&status, now),
        Err(e @ ReadError::Refused(_)) => return Err(e),
        Err(e) => {
            tracing::debug!(error = %e, "PRTG's status does not read");
            None
        }
    }
    .unwrap_or_else(Duration::zero);
    let table = |content: &'static str,
                 columns: &'static str,
                 filter: Option<(&'static str, &'static str)>| {
        let fetch = fetch.clone();
        async move {
            let mut query = vec![
                ("content", content.to_owned()),
                ("columns", columns.to_owned()),
                ("count", "*".to_owned()),
            ];
            query.extend(filter.map(|(k, v)| (k, v.to_owned())));
            fetch.get("/api/table.json", &query).await
        }
    };
    let probes = objects(
        &table(
            "probenodes",
            "objid,name,parentid,status",
            Some(("filter_parentid", "0")),
        )
        .await?,
        Kind::Probe,
    )?;
    let groups: Vec<Object> = objects(
        &table("groups", "objid,name,parentid,status", None).await?,
        Kind::Group,
    )?
    .into_iter()
    .filter(|g| !probes.iter().any(|p| p.id == g.id))
    .collect();
    let devices = objects(
        &table("devices", "objid,name,parentid,host,status", None).await?,
        Kind::Device,
    )?;
    let sensors = objects(
        &table("sensors", "objid,name,parentid,type,interval,status", None).await?,
        Kind::Sensor,
    )?;

    let mut containers = probes;
    containers.extend(groups);
    containers.extend(devices);
    let containers = each(containers, |o| {
        let fetch = fetch.clone();
        async move {
            let o = maintenance(fetch.as_ref(), o, offset).await?;
            discovery(fetch.as_ref(), o).await
        }
    })
    .await?;
    let sensors = each(sensors, |o| {
        let fetch = fetch.clone();
        async move { sensor_details(fetch.as_ref(), o).await }
    })
    .await?;
    let mut objects = containers;
    objects.extend(sensors);
    Ok(Tree {
        objects,
        notifications: notifications(fetch.as_ref()).await?,
        local_times: false,
        schedules: Default::default(),
    })
}
