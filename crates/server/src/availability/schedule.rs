//! When scheduled reports are due (ADR-0155): the latest period ended in a
//! schedule's time zone, a calendar month or a week from Monday, sent at
//! 06:00 local time the day it ends.

use chrono::{DateTime, Datelike, Duration, LocalResult, NaiveDate, NaiveTime, TimeZone, Utc};
use chrono_tz::Tz;

use super::Span;

/// Local time reports are sent at, the day their period ends.
pub const SEND_AT: NaiveTime = match NaiveTime::from_hms_opt(6, 0, 0) {
    Some(t) => t,
    None => NaiveTime::MIN,
};

/// How often a schedule sends.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Frequency {
    Monthly,
    Weekly,
}

impl Frequency {
    pub fn parse(text: &str) -> Option<Self> {
        match text {
            "monthly" => Some(Self::Monthly),
            "weekly" => Some(Self::Weekly),
            _ => None,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Monthly => "monthly",
            Self::Weekly => "weekly",
        }
    }
}

/// `date` at `time` in `zone`: the first of two when clocks go back, and
/// the time after the gap when they go forward.
fn local(date: NaiveDate, time: NaiveTime, zone: Tz) -> DateTime<Utc> {
    let naive = date.and_time(time);
    match zone.from_local_datetime(&naive) {
        LocalResult::Single(t) | LocalResult::Ambiguous(t, _) => t.to_utc(),
        LocalResult::None => zone
            .from_local_datetime(&(naive + Duration::hours(1)))
            .earliest()
            .map_or_else(|| naive.and_utc(), |t| t.to_utc()),
    }
}

/// The start of the period of `frequency` that holds the local date `day`.
fn start_of(frequency: Frequency, day: NaiveDate) -> NaiveDate {
    match frequency {
        Frequency::Monthly => day.with_day(1).unwrap_or(day),
        Frequency::Weekly => day - Duration::days(i64::from(day.weekday().num_days_from_monday())),
    }
}

fn previous(frequency: Frequency, start: NaiveDate) -> NaiveDate {
    match frequency {
        Frequency::Monthly => start_of(frequency, start - Duration::days(1)),
        Frequency::Weekly => start - Duration::days(7),
    }
}

/// The latest period that ended by `now`, from local midnight to local
/// midnight.
pub fn latest(frequency: Frequency, zone: Tz, now: DateTime<Utc>) -> Span {
    let today = now.with_timezone(&zone).date_naive();
    let end = start_of(frequency, today);
    let start = previous(frequency, end);
    (
        local(start, NaiveTime::MIN, zone),
        local(end, NaiveTime::MIN, zone),
    )
}

/// The period a schedule sends next: the latest one while it is still to
/// send, else the one after it.
pub fn next(
    frequency: Frequency,
    zone: Tz,
    last_sent_end: Option<DateTime<Utc>>,
    now: DateTime<Utc>,
) -> Span {
    let period = latest(frequency, zone, now);
    if !last_sent_end.is_some_and(|end| end >= period.1) {
        return period;
    }
    // A day into the period after the following one ends it.
    let ahead = match frequency {
        Frequency::Monthly => Duration::days(32),
        Frequency::Weekly => Duration::days(8),
    };
    latest(frequency, zone, period.1 + ahead)
}

/// When the report of a period ending at `end` is sent: 06:00 local time
/// that day.
pub fn send_time(end: DateTime<Utc>, zone: Tz) -> DateTime<Utc> {
    local(end.with_timezone(&zone).date_naive(), SEND_AT, zone)
}

/// The period to send at `now`, if any: the latest one, once its sending
/// time has come, unless it was sent already.
pub fn due(
    frequency: Frequency,
    zone: Tz,
    last_sent_end: Option<DateTime<Utc>>,
    now: DateTime<Utc>,
) -> Option<Span> {
    let period = latest(frequency, zone, now);
    let sent = last_sent_end.is_some_and(|end| end >= period.1);
    (!sent && now >= send_time(period.1, zone)).then_some(period)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(text: &str) -> DateTime<Utc> {
        DateTime::parse_from_rfc3339(text).unwrap().to_utc()
    }

    const PARIS: Tz = chrono_tz::Europe::Paris;

    #[test]
    fn the_latest_month_in_the_zone() {
        // 23:30 UTC on 31 October is already November in Paris.
        let (from, to) = latest(Frequency::Monthly, PARIS, at("2026-10-31T23:30:00Z"));
        assert_eq!(from, at("2026-09-30T22:00:00Z"));
        assert_eq!(to, at("2026-10-31T23:00:00Z"));
        let (from, to) = latest(Frequency::Monthly, Tz::UTC, at("2026-01-15T10:00:00Z"));
        assert_eq!(
            (from, to),
            (at("2025-12-01T00:00:00Z"), at("2026-01-01T00:00:00Z"))
        );
    }

    #[test]
    fn the_latest_week_from_monday() {
        // Wednesday 18 November: the week of 9 November.
        let (from, to) = latest(Frequency::Weekly, Tz::UTC, at("2026-11-18T10:00:00Z"));
        assert_eq!(
            (from, to),
            (at("2026-11-09T00:00:00Z"), at("2026-11-16T00:00:00Z"))
        );
    }

    #[test]
    fn sent_at_six_once() {
        let monthly = Frequency::Monthly;
        assert_eq!(due(monthly, PARIS, None, at("2026-11-01T04:59:00Z")), None);
        let october = due(monthly, PARIS, None, at("2026-11-01T05:00:00Z")).unwrap();
        assert_eq!(october.1, at("2026-10-31T23:00:00Z"));
        // Sent: nothing until December.
        assert_eq!(
            due(monthly, PARIS, Some(october.1), at("2026-11-20T10:00:00Z")),
            None
        );
        // A server down on the first sends it when it is back, once.
        let late = due(
            monthly,
            PARIS,
            Some(at("2026-09-30T22:00:00Z")),
            at("2026-11-03T10:00:00Z"),
        );
        assert_eq!(late, Some(october));
        assert_eq!(Frequency::parse("weekly"), Some(Frequency::Weekly));
        assert_eq!(Frequency::parse("daily"), None);
    }

    #[test]
    fn the_next_period() {
        let utc = Tz::UTC;
        let now = at("2026-02-10T10:00:00Z");
        let january = latest(Frequency::Monthly, utc, now);
        assert_eq!(next(Frequency::Monthly, utc, None, now), january);
        let february = next(Frequency::Monthly, utc, Some(january.1), now);
        assert_eq!(
            february,
            (at("2026-02-01T00:00:00Z"), at("2026-03-01T00:00:00Z"))
        );
        let now = at("2026-03-10T10:00:00Z");
        let march = next(
            Frequency::Monthly,
            utc,
            Some(at("2026-03-01T00:00:00Z")),
            now,
        );
        assert_eq!(
            march,
            (at("2026-03-01T00:00:00Z"), at("2026-04-01T00:00:00Z"))
        );
        let week = next(
            Frequency::Weekly,
            utc,
            Some(at("2026-03-09T00:00:00Z")),
            now,
        );
        assert_eq!(
            week,
            (at("2026-03-09T00:00:00Z"), at("2026-03-16T00:00:00Z"))
        );
    }

    #[test]
    fn a_skipped_hour_is_read_after_the_gap() {
        // No 02:30 in Paris on 29 March 2026.
        let naive = NaiveDate::from_ymd_opt(2026, 3, 29).unwrap();
        let t = local(naive, NaiveTime::from_hms_opt(2, 30, 0).unwrap(), PARIS);
        assert_eq!(t, at("2026-03-29T01:30:00Z"));
    }
}
