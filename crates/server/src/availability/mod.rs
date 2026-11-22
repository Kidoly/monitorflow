//! Availability (ADR-0155): how long checks, servers, devices and business
//! services were up over a period, from when they were down and when they
//! were under planned maintenance.
//!
//! This module is the arithmetic only, with no database: [`measure`] takes
//! the down time of a thing's incidents, each with the silences that
//! excuse it, and [`combine`] applies a business service's rule to its
//! members. Loading incidents and silences is apart.

pub mod csv;
pub mod data;
pub mod pdf;
mod pdf_metrics;
pub mod report;
pub mod schedule;
pub mod sender;
pub mod spans;
pub mod uptime;
pub mod words;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

pub use spans::{Span, Spans};

/// The whole report of `customer` (its sites and services), or of
/// everything when `None`: what a schedule sends, with no user to narrow it.
/// With `zone`, the uptime at the period's end comes with it (ADR-0172).
pub async fn for_customer(
    db: &mut sqlx::PgConnection,
    customer: Option<uuid::Uuid>,
    period: Span,
    now: DateTime<Utc>,
    zone: Option<chrono_tz::Tz>,
) -> Result<(report::Report, Option<uptime::Uptime>), sqlx::Error> {
    let (reach, name) = match customer {
        None => (
            data::Reach {
                sites: None,
                services_of: None,
            },
            None,
        ),
        Some(id) => {
            let name: Option<String> =
                sqlx::query_scalar("SELECT name FROM customers WHERE id = $1")
                    .bind(id)
                    .fetch_optional(&mut *db)
                    .await?;
            let sites: Vec<uuid::Uuid> =
                sqlx::query_scalar("SELECT id FROM sites WHERE customer_id = $1")
                    .bind(id)
                    .fetch_all(&mut *db)
                    .await?;
            (
                data::Reach {
                    sites: Some(sites),
                    services_of: Some(vec![id]),
                },
                name,
            )
        }
    };
    let data = data::load(db, &reach, period, now).await?;
    let mut report = report::build(&data, period, now);
    report.customer_id = customer;
    report.customer_name = name;
    let uptime = match zone {
        Some(zone) => Some(uptime_at_end(db, &reach, period, zone, now).await?),
        None => None,
    };
    Ok((report, uptime))
}

/// The uptime of `reach` at the end of `period` (or now, when it is still
/// going on), days cut in `zone`: what a report's PDF shows (ADR-0172).
pub async fn uptime_at_end(
    db: &mut sqlx::PgConnection,
    reach: &data::Reach,
    period: Span,
    zone: chrono_tz::Tz,
    now: DateTime<Utc>,
) -> Result<uptime::Uptime, sqlx::Error> {
    let at = period.1.min(now);
    let data = data::load(db, reach, uptime::period(at), now).await?;
    Ok(uptime::build(&data, at, zone, now))
}

/// Something that made a thing down for a while: the critical time of an
/// incident, or of what it depends on, with the planned maintenance that
/// excuses part of it. `cause` says what it was, for the outage list.
#[derive(Debug, Clone)]
pub struct Source<C> {
    pub down: Spans,
    /// Silences covering this cause: their time does not count as down.
    pub cover: Spans,
    pub cause: C,
}

impl<C> Source<C> {
    /// The down time that counts.
    fn counted(&self) -> Spans {
        self.down.minus(&self.cover)
    }
}

/// One unbroken stretch of down time.
#[derive(Debug, Clone, PartialEq)]
pub struct Outage<C> {
    pub start: DateTime<Utc>,
    pub end: DateTime<Utc>,
    /// What was down during it, the earliest first.
    pub causes: Vec<C>,
}

impl<C> Outage<C> {
    pub fn seconds(&self) -> f64 {
        (self.end - self.start).num_milliseconds() as f64 / 1000.0
    }
}

/// When a thing was down and when it was under planned maintenance, within
/// its period: what a business service reads of its members.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Timeline {
    pub down: Spans,
    pub maintenance: Spans,
    /// When the central server was not running (ADR-0173), outside planned
    /// maintenance: nothing was watched, so it is neither up nor down.
    pub unobserved: Spans,
}

/// The availability of one thing over a period.
#[derive(Debug, Clone, PartialEq)]
pub struct Measured<C> {
    /// The time counted: the period without planned maintenance, seconds.
    pub counted: f64,
    /// Down time, seconds.
    pub down: f64,
    /// Planned maintenance, seconds, left out of `counted`.
    pub maintenance: f64,
    /// Time the central server was not running, seconds, left out of
    /// `counted` too (ADR-0173).
    pub unobserved: f64,
    pub outages: Vec<Outage<C>>,
    pub timeline: Timeline,
}

impl<C> Measured<C> {
    /// The share of the counted time that was up, 0 to 1; `None` when
    /// nothing was counted, which is "Not measured", never 100 %.
    pub fn availability(&self) -> Option<f64> {
        (self.counted > 0.0).then(|| ((self.counted - self.down) / self.counted).clamp(0.0, 1.0))
    }

    /// The longest outage, seconds; 0 without any.
    pub fn longest(&self) -> f64 {
        self.outages.iter().map(Outage::seconds).fold(0.0, f64::max)
    }
}

/// The availability over `period` of a thing whose planned maintenance is
/// `maintenance` and whose down time comes from `sources`, without the
/// `gaps` when the central server was not running. `period` is already
/// narrowed to when the thing existed.
pub fn measure<C: Clone>(
    period: Span,
    maintenance: &Spans,
    gaps: &Spans,
    sources: &[Source<C>],
) -> Measured<C> {
    let maintenance = maintenance.clip(period);
    let unobserved = gaps.clip(period).minus(&maintenance);
    let counted_spans: Vec<Spans> = sources.iter().map(Source::counted).collect();
    let down = Spans::from_spans(counted_spans.iter().flat_map(|s| s.spans().iter().copied()))
        .clip(period)
        .minus(&maintenance)
        .minus(&unobserved);
    let timeline = Timeline {
        down: down.clone(),
        maintenance: maintenance.clone(),
        unobserved,
    };
    let outages = outages(&down, |span| {
        let mut causes: Vec<(DateTime<Utc>, C)> = sources
            .iter()
            .zip(&counted_spans)
            .filter_map(|(source, counted)| {
                let within = counted.clip(span);
                within
                    .spans()
                    .first()
                    .map(|first| (first.0, source.cause.clone()))
            })
            .collect();
        causes.sort_by_key(|(start, _)| *start);
        causes.into_iter().map(|(_, cause)| cause).collect()
    });
    finish(period, timeline, outages)
}

/// The outages of `down`, each with the causes `causes` gives for it.
fn outages<C>(down: &Spans, causes: impl Fn(Span) -> Vec<C>) -> Vec<Outage<C>> {
    down.spans()
        .iter()
        .map(|&(start, end)| Outage {
            start,
            end,
            causes: causes((start, end)),
        })
        .collect()
}

fn finish<C>(period: Span, timeline: Timeline, outages: Vec<Outage<C>>) -> Measured<C> {
    let whole = Spans::one(period.0, period.1).seconds();
    let maintenance = timeline.maintenance.seconds();
    let unobserved = timeline.unobserved.seconds();
    Measured {
        counted: whole - maintenance - unobserved,
        down: timeline.down.seconds(),
        maintenance,
        unobserved,
        outages,
        timeline,
    }
}

/// A business service's rule.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Rule {
    /// Down while any member is down.
    All,
    /// Down while fewer than this many members are up.
    AtLeast(u32),
}

/// The availability of a business service over `period` from its members'
/// timelines. A member under planned maintenance counts as up; the time
/// the service would be down only because of such members is its planned
/// maintenance. The `gaps` when the central server was not running are
/// left out. Outages name the members down, by index.
pub fn combine(period: Span, rule: Rule, members: &[Timeline], gaps: &Spans) -> Measured<usize> {
    let total = members.len();
    let needed = match rule {
        Rule::All => total,
        Rule::AtLeast(n) => usize::try_from(n).unwrap_or(usize::MAX).min(total),
    };
    let mut cuts: Vec<DateTime<Utc>> = vec![period.0, period.1];
    for member in members {
        for spans in [&member.down, &member.maintenance] {
            for &(a, b) in spans.spans() {
                cuts.extend([a, b]);
            }
        }
    }
    cuts.retain(|t| *t >= period.0 && *t <= period.1);
    cuts.sort_unstable();
    cuts.dedup();
    let within =
        |spans: &Spans, at: DateTime<Utc>| spans.spans().iter().any(|&(a, b)| a <= at && at < b);
    let (mut down, mut maintenance) = (Vec::new(), Vec::new());
    for pair in cuts.windows(2) {
        let (start, end) = (pair[0], pair[1]);
        let failing = members.iter().filter(|m| within(&m.down, start)).count();
        let resting = members
            .iter()
            .filter(|m| within(&m.maintenance, start))
            .count();
        if total - failing < needed {
            down.push((start, end));
        } else if total - failing - resting < needed {
            maintenance.push((start, end));
        }
    }
    let maintenance = Spans::from_spans(maintenance);
    let unobserved = gaps.clip(period).minus(&maintenance);
    let timeline = Timeline {
        down: Spans::from_spans(down).minus(&unobserved),
        maintenance,
        unobserved,
    };
    let outages = outages(&timeline.down, |span| {
        members
            .iter()
            .enumerate()
            .filter(|(_, m)| !m.down.clip(span).is_empty())
            .map(|(i, _)| i)
            .collect()
    });
    finish(period, timeline, outages)
}

/// The down time an SLA target allows over `counted` seconds, minus the
/// down time there was: negative when the target was missed. `target` is
/// a percentage, like 99.9.
pub fn error_budget(target: f64, counted: f64, down: f64) -> f64 {
    (1.0 - target / 100.0) * counted - down
}

#[cfg(test)]
mod tests;
