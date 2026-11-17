//! Maintenance periods: the window a silence covers. A one-time period is
//! its window; a daily, weekly or monthly one gives its next window after
//! now. Both are bounded by the maintenance's active dates.
//!
//! Zabbix computes recurring periods in the server's time zone, which the
//! API does not give: windows are read in UTC, the zone of the official
//! container images.

use chrono::{DateTime, Datelike, Duration, NaiveDate, TimeZone, Utc};

use super::config::{Period, PeriodKind};

/// Days looked ahead for a recurring period's next window.
const HORIZON_DAYS: i64 = 800;

/// A window, start and end.
pub type Window = (DateTime<Utc>, DateTime<Utc>);

fn at(seconds: i64) -> Option<DateTime<Utc>> {
    Utc.timestamp_opt(seconds, 0).single()
}

/// The window of `period` that ends after `now`, within the active dates
/// `since` and `till` (Unix time): the current one if it is on, the next
/// one otherwise. `None` when none is left.
pub fn next_window(period: &Period, since: i64, till: i64, now: DateTime<Utc>) -> Option<Window> {
    let since = at(since)?;
    let till = at(till)?;
    let length = Duration::seconds(i64::from(period.length));
    let clip = |(start, end): Window| -> Option<Window> {
        let start = start.max(since);
        let end = end.min(till);
        (start < end && end > now).then_some((start, end))
    };
    if period.kind == PeriodKind::Once {
        let start = at(period.start_date)?;
        return clip((start, start + length));
    }
    // Days from the one a window started on before `now` could still be
    // on, to the horizon or the end of the maintenance.
    let first = (now - length).max(since).date_naive();
    let last = till.date_naive().min(first + Duration::days(HORIZON_DAYS));
    let mut day = first;
    while day <= last {
        if matches(period, day, since.date_naive()) {
            let start = Utc.from_utc_datetime(&day.and_hms_opt(0, 0, 0)?)
                + Duration::seconds(i64::from(period.start_time));
            if let Some(window) = clip((start, start + length)) {
                return Some(window);
            }
        }
        day = day.succ_opt()?;
    }
    None
}

/// Whether a window of a recurring `period` starts on `day`; `origin` is
/// the first day of the maintenance, which "every N days or weeks" counts
/// from.
fn matches(period: &Period, day: NaiveDate, origin: NaiveDate) -> bool {
    let every = i64::from(period.every.max(1));
    let weekday_bit = 1u32 << day.weekday().num_days_from_monday();
    match period.kind {
        PeriodKind::Once => false,
        PeriodKind::Daily => (day - origin).num_days().rem_euclid(every) == 0,
        PeriodKind::Weekly => {
            let monday =
                |d: NaiveDate| d - Duration::days(i64::from(d.weekday().num_days_from_monday()));
            let weeks = (monday(day) - monday(origin)).num_days() / 7;
            period.weekdays & weekday_bit != 0 && weeks.rem_euclid(every) == 0
        }
        PeriodKind::Monthly => {
            if period.months & (1 << day.month0()) == 0 {
                return false;
            }
            if period.day > 0 {
                return day.day() == period.day;
            }
            if period.weekdays & weekday_bit == 0 {
                return false;
            }
            let week = (day.day() - 1) / 7 + 1;
            let last = (day + Duration::days(7)).month() != day.month();
            if period.every >= 5 {
                last
            } else {
                week == period.every
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn utc(text: &str) -> DateTime<Utc> {
        DateTime::parse_from_rfc3339(text)
            .unwrap()
            .with_timezone(&Utc)
    }

    fn period(kind: PeriodKind) -> Period {
        Period {
            kind,
            every: 1,
            months: 0,
            weekdays: 0,
            day: 0,
            start_time: 0,
            length: 7200,
            start_date: 0,
        }
    }

    const SINCE: i64 = 1_767_225_600; // 2026-01-01T00:00:00Z
    const TILL: i64 = 1_798_761_600; // 2027-01-01T00:00:00Z

    #[test]
    fn one_time_windows() {
        let mut once = period(PeriodKind::Once);
        once.start_date = utc("2026-11-24T22:00:00Z").timestamp();
        once.length = 4 * 3600;
        let now = utc("2026-11-17T10:00:00Z");
        assert_eq!(
            next_window(&once, SINCE, TILL, now),
            Some((utc("2026-11-24T22:00:00Z"), utc("2026-11-25T02:00:00Z")))
        );
        // Cut by the end of the maintenance.
        let till = utc("2026-11-25T00:00:00Z").timestamp();
        assert_eq!(
            next_window(&once, SINCE, till, now),
            Some((utc("2026-11-24T22:00:00Z"), utc("2026-11-25T00:00:00Z")))
        );
        // Over.
        assert_eq!(
            next_window(&once, SINCE, TILL, utc("2026-11-26T00:00:00Z")),
            None
        );
    }

    #[test]
    fn weekly_windows() {
        // Sundays 02:00 to 04:00.
        let mut weekly = period(PeriodKind::Weekly);
        weekly.weekdays = 64;
        weekly.start_time = 7200;
        // 2026-11-17 is a Tuesday.
        assert_eq!(
            next_window(&weekly, SINCE, TILL, utc("2026-11-17T10:00:00Z")),
            Some((utc("2026-11-22T02:00:00Z"), utc("2026-11-22T04:00:00Z")))
        );
        // During a window: that window.
        assert_eq!(
            next_window(&weekly, SINCE, TILL, utc("2026-11-22T03:00:00Z")),
            Some((utc("2026-11-22T02:00:00Z"), utc("2026-11-22T04:00:00Z")))
        );
        // Every two weeks from the week of 2026-01-01 (a Thursday): the
        // Sunday of week 0 is 2026-01-04; 2026-11-22 is 46 weeks later.
        weekly.every = 2;
        assert_eq!(
            next_window(&weekly, SINCE, TILL, utc("2026-11-17T10:00:00Z")),
            Some((utc("2026-11-22T02:00:00Z"), utc("2026-11-22T04:00:00Z")))
        );
        weekly.every = 3;
        assert_eq!(
            next_window(&weekly, SINCE, TILL, utc("2026-11-17T10:00:00Z")),
            Some((utc("2026-12-06T02:00:00Z"), utc("2026-12-06T04:00:00Z")))
        );
    }

    #[test]
    fn daily_and_monthly_windows() {
        let mut daily = period(PeriodKind::Daily);
        daily.start_time = 23 * 3600;
        assert_eq!(
            next_window(&daily, SINCE, TILL, utc("2026-11-17T10:00:00Z")),
            Some((utc("2026-11-17T23:00:00Z"), utc("2026-11-18T01:00:00Z")))
        );
        // Every month on the 1st at 03:00.
        let mut monthly = period(PeriodKind::Monthly);
        monthly.months = 0xfff;
        monthly.day = 1;
        monthly.start_time = 3 * 3600;
        assert_eq!(
            next_window(&monthly, SINCE, TILL, utc("2026-11-17T10:00:00Z")),
            Some((utc("2026-12-01T03:00:00Z"), utc("2026-12-01T05:00:00Z")))
        );
        // The last Friday of January and July.
        monthly.day = 0;
        monthly.months = 1 | (1 << 6);
        monthly.weekdays = 16;
        monthly.every = 5;
        assert_eq!(
            next_window(&monthly, SINCE, 1_830_297_600, utc("2026-11-17T10:00:00Z")),
            Some((utc("2027-01-29T03:00:00Z"), utc("2027-01-29T05:00:00Z")))
        );
        // The second Monday of every month.
        monthly.months = 0xfff;
        monthly.weekdays = 1;
        monthly.every = 2;
        assert_eq!(
            next_window(&monthly, SINCE, TILL, utc("2026-11-17T10:00:00Z")),
            Some((utc("2026-12-14T03:00:00Z"), utc("2026-12-14T05:00:00Z")))
        );
    }
}
