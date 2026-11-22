//! Uptime at a glance (ADR-0172): for each business service, server,
//! device and check with outages, its availability over the last 24 hours,
//! 7, 30 and 90 days, and one bar per day for 90 days, from the same
//! measures as reports (ADR-0155), so the numbers never differ.

use chrono::{DateTime, Days, NaiveDate, TimeDelta, Utc};
use chrono_tz::Tz;
use serde::Serialize;
use uuid::Uuid;

use super::data::Data;
use super::report::{measures, Kind, Measure};
use super::{Span, Spans, Timeline};
use crate::notify::on_call::local_instant;

/// Days of bars, and the longest window.
pub const DAYS: u64 = 90;

/// Availability over the windows that end at the view's instant, in
/// percent; `null` when nothing was counted ("Not measured").
#[derive(Debug, Clone, PartialEq, Serialize, utoipa::ToSchema)]
#[schema(as = UptimeWindows)]
pub struct Windows {
    /// The last 24 hours.
    pub day: Option<f64>,
    /// The last 7 days.
    pub week: Option<f64>,
    /// The last 30 days.
    pub month: Option<f64>,
    /// The last 90 days.
    pub quarter: Option<f64>,
}

/// One day of one thing.
#[derive(Debug, Clone, PartialEq, Serialize, utoipa::ToSchema)]
#[schema(as = UptimeDay)]
pub struct Bar {
    /// The local date, in the view's time zone.
    pub date: NaiveDate,
    /// In percent; `null` when nothing was counted: before it existed, all
    /// of it under planned maintenance, or the central server not running.
    pub availability: Option<f64>,
    /// Down time that day, seconds.
    pub down_seconds: f64,
    /// Planned maintenance that day, seconds, left out of the count.
    pub maintenance_seconds: f64,
}

/// One service, server, device or check.
#[derive(Debug, Clone, Serialize, utoipa::ToSchema)]
#[schema(as = UptimeRow)]
pub struct Row {
    /// `service`, `server`, `device` or `check`.
    pub kind: Kind,
    /// The service's ID, or the server's or device's (a check's too).
    pub id: Uuid,
    /// The service's, server's or device's name.
    pub name: String,
    /// The check's name, for a check; `null` otherwise.
    pub check: Option<String>,
    /// The site; `null` for a service.
    pub site_name: Option<String>,
    pub windows: Windows,
    /// Outages over the 90 days.
    pub outages: usize,
    /// One per day, the oldest first, the last being today so far.
    pub days: Vec<Bar>,
}

/// Body of `GET /api/availability/uptime`.
#[derive(Debug, Clone, Serialize, utoipa::ToSchema)]
#[schema(as = Uptime)]
pub struct Uptime {
    /// The instant the windows end at.
    pub at: DateTime<Utc>,
    /// The IANA zone days are cut in.
    pub time_zone: String,
    /// Services first, then each server or device followed by its checks
    /// that had outages; checks without any are left out.
    pub rows: Vec<Row>,
}

/// The local dates of the bars ending with the one of `at`, and the
/// instant the first one starts.
pub fn days(at: DateTime<Utc>, zone: Tz) -> (Vec<NaiveDate>, DateTime<Utc>) {
    let last = at.with_timezone(&zone).date_naive();
    let first = last.checked_sub_days(Days::new(DAYS - 1)).unwrap_or(last);
    let dates: Vec<NaiveDate> = first.iter_days().take_while(|d| *d <= last).collect();
    (dates, midnight(first, zone))
}

fn midnight(date: NaiveDate, zone: Tz) -> DateTime<Utc> {
    local_instant(zone, date.and_hms_opt(0, 0, 0).unwrap_or_default())
}

/// What a view over `at` must load: the 90 days before it, which start
/// before the first bar's day (that day is 89 days before today's).
pub fn period(at: DateTime<Utc>) -> Span {
    (at - TimeDelta::days(i64::try_from(DAYS).unwrap_or(90)), at)
}

/// Counted, down and maintenance seconds of `timeline` over `window`,
/// within `life`; the time the central server was not running is not
/// counted (ADR-0176).
pub fn slice(life: Span, timeline: &Timeline, window: Span) -> (f64, f64, f64) {
    let alive = Spans::one(life.0.max(window.0), life.1.min(window.1));
    let maintenance = timeline.maintenance.intersect(&alive).seconds();
    let unobserved = timeline.unobserved.intersect(&alive).seconds();
    let down = timeline.down.intersect(&alive).seconds();
    (
        alive.seconds() - maintenance - unobserved,
        down,
        maintenance,
    )
}

fn percent((counted, down, _): (f64, f64, f64)) -> Option<f64> {
    (counted > 0.0).then(|| ((counted - down) / counted * 100.0).clamp(0.0, 100.0))
}

fn row(m: Measure, at: DateTime<Utc>, zone: Tz, dates: &[NaiveDate]) -> Row {
    let back = |days: i64| percent(slice(m.life, &m.timeline, (at - TimeDelta::days(days), at)));
    let days = dates
        .iter()
        .map(|date| {
            let start = midnight(*date, zone);
            let end = date
                .succ_opt()
                .map_or(at, |next| midnight(next, zone))
                .min(at);
            let measured = slice(m.life, &m.timeline, (start, end));
            Bar {
                date: *date,
                availability: percent(measured),
                down_seconds: measured.1,
                maintenance_seconds: measured.2,
            }
        })
        .collect();
    Row {
        kind: m.kind,
        id: m.id,
        name: m.name,
        check: m.check,
        site_name: m.site_name,
        windows: Windows {
            day: back(1),
            week: back(7),
            month: back(30),
            quarter: back(90),
        },
        outages: m.outages,
        days,
    }
}

/// The uptime of everything in `data` at `at`, days cut in `zone`;
/// `data` is loaded over [`period`] and `now` is when it was read.
pub fn build(data: &Data, at: DateTime<Utc>, zone: Tz, now: DateTime<Utc>) -> Uptime {
    let (dates, _) = days(at, zone);
    let at = at.min(now);
    let rows = measures(data, period(at), now)
        .into_iter()
        .map(|m| row(m, at, zone, &dates))
        .collect();
    Uptime {
        at,
        time_zone: zone.name().to_owned(),
        rows,
    }
}

#[cfg(test)]
mod tests;
