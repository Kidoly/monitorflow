//! Maintenance windows (ADR-0151): silences that come back every day,
//! week or month, at local times in a time zone.
//!
//! [`Schedule`] is the rule and says when its windows are, with daylight
//! saving handled as on-call hand-offs handle it (RFC 5545): a local time
//! that happens twice is the first, one that is skipped is read with the
//! offset before the change.

use chrono::{DateTime, Datelike, Days, NaiveDate, NaiveTime, TimeDelta, Utc};
use chrono_tz::Tz;

use super::on_call::local_instant;

/// Longest window: a week.
pub const MAX_DURATION_MINUTES: u32 = 7 * 24 * 60;
/// Most "every N" for a daily rule; weekly ones go to 52.
pub const MAX_EVERY_DAYS: u16 = 365;
pub const MAX_EVERY_WEEKS: u16 = 52;
/// Every month of the year, a bit each, January first.
pub const EVERY_MONTH: u16 = 0xfff;
/// The last week of a month, for [`Schedule::month_week`].
pub const LAST_WEEK: u8 = 5;
/// How far [`Schedule::next`] looks: a rule on the 31st of February
/// never comes.
const HORIZON_DAYS: u64 = 4 * 366;

/// How a window comes back.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Repeat {
    Daily,
    Weekly,
    Monthly,
}

impl Repeat {
    pub fn parse(text: &str) -> Option<Self> {
        match text {
            "daily" => Some(Self::Daily),
            "weekly" => Some(Self::Weekly),
            "monthly" => Some(Self::Monthly),
            _ => None,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Daily => "daily",
            Self::Weekly => "weekly",
            Self::Monthly => "monthly",
        }
    }
}

/// One window, from its start, included, to its end, excluded.
pub type Window = (DateTime<Utc>, DateTime<Utc>);

/// The rule of a maintenance window.
#[derive(Debug, Clone, PartialEq)]
pub struct Schedule {
    pub repeat: Repeat,
    /// Every N days (daily) or weeks (weekly) from `starts_on`; 1 for
    /// monthly.
    pub every: u16,
    /// A bit per weekday, Monday first: the days of a weekly rule, the
    /// weekday of a monthly one by week.
    pub weekdays: u8,
    /// Monthly, on this day of the month; a month without it has none.
    pub month_day: Option<u8>,
    /// Monthly, on the weekday of this week of the month (1 to 4, or
    /// [`LAST_WEEK`]).
    pub month_week: Option<u8>,
    /// A bit per month, January first.
    pub months: u16,
    /// Local time each window starts.
    pub start_time: NaiveTime,
    pub duration_minutes: u32,
    pub time_zone: Tz,
    /// The first local date a window may start on.
    pub starts_on: NaiveDate,
    /// The last one, if any.
    pub ends_on: Option<NaiveDate>,
}

impl Schedule {
    /// Why this rule cannot be kept, as a sentence for the form, or `Ok`.
    pub fn validate(&self) -> Result<(), String> {
        if self.duration_minutes == 0 || self.duration_minutes > MAX_DURATION_MINUTES {
            return Err("A window lasts from 1 minute to 7 days.".into());
        }
        if self.weekdays > 0x7f || self.months == 0 || self.months > EVERY_MONTH {
            return Err("Choose weekdays and months that exist.".into());
        }
        if self.ends_on.is_some_and(|end| end < self.starts_on) {
            return Err("The last day must be on or after the first.".into());
        }
        match self.repeat {
            Repeat::Daily if !(1..=MAX_EVERY_DAYS).contains(&self.every) => {
                Err(format!("Every 1 to {MAX_EVERY_DAYS} days."))
            }
            Repeat::Weekly if !(1..=MAX_EVERY_WEEKS).contains(&self.every) => {
                Err(format!("Every 1 to {MAX_EVERY_WEEKS} weeks."))
            }
            Repeat::Weekly if self.weekdays == 0 => Err("Choose at least one weekday.".into()),
            Repeat::Monthly if self.every != 1 => {
                Err("A monthly window comes every month it names.".into())
            }
            Repeat::Monthly => match (self.month_day, self.month_week) {
                (Some(day), None) if (1..=31).contains(&day) => Ok(()),
                (None, Some(week)) if (1..=LAST_WEEK).contains(&week) => {
                    if self.weekdays.count_ones() == 1 {
                        Ok(())
                    } else {
                        Err("Choose one weekday for a monthly window by week.".into())
                    }
                }
                _ => Err("Choose a day of the month, or a week and a weekday.".into()),
            },
            _ => Ok(()),
        }
    }

    /// Whether a window starts on the local date `day`.
    pub fn starts_on_day(&self, day: NaiveDate) -> bool {
        if day < self.starts_on || self.ends_on.is_some_and(|end| day > end) {
            return false;
        }
        let weekday = 1u8 << day.weekday().num_days_from_monday();
        let every = i64::from(self.every.max(1));
        match self.repeat {
            Repeat::Daily => (day - self.starts_on).num_days().rem_euclid(every) == 0,
            Repeat::Weekly => {
                let monday = |d: NaiveDate| {
                    d - TimeDelta::days(i64::from(d.weekday().num_days_from_monday()))
                };
                let weeks = (monday(day) - monday(self.starts_on)).num_days() / 7;
                self.weekdays & weekday != 0 && weeks.rem_euclid(every) == 0
            }
            Repeat::Monthly => {
                if self.months & (1 << day.month0()) == 0 {
                    return false;
                }
                match (self.month_day, self.month_week) {
                    (Some(d), _) => day.day() == u32::from(d),
                    (None, Some(week)) => {
                        let last = (day + Days::new(7)).month() != day.month();
                        self.weekdays & weekday != 0
                            && if week >= LAST_WEEK {
                                last
                            } else {
                                (day.day() - 1) / 7 + 1 == u32::from(week)
                            }
                    }
                    (None, None) => false,
                }
            }
        }
    }

    /// The window that starts on the local date `day`. Its end is the local
    /// start plus the duration, so a night window keeps its local end when
    /// clocks change; when the start falls in the hour clocks skip, and
    /// that end is not after it, the window lasts its duration from the
    /// start instead.
    pub fn window_on(&self, day: NaiveDate) -> Window {
        let local = day.and_time(self.start_time);
        let duration = TimeDelta::minutes(i64::from(self.duration_minutes));
        let start = local_instant(self.time_zone, local);
        let end = local_instant(self.time_zone, local + duration);
        (start, if end > start { end } else { start + duration })
    }

    /// The windows that end after `from` and start before `until`, first
    /// first.
    pub fn windows(&self, from: DateTime<Utc>, until: DateTime<Utc>) -> Vec<Window> {
        let tz = self.time_zone;
        // A window lasts at most a week, and local dates are within a day
        // of UTC ones.
        let first = from.with_timezone(&tz).date_naive() - Days::new(8);
        let last = until.with_timezone(&tz).date_naive() + Days::new(1);
        first
            .iter_days()
            .take_while(|day| *day <= last)
            .filter(|day| self.starts_on_day(*day))
            .map(|day| self.window_on(day))
            .filter(|(start, end)| *end > from && *start < until)
            .collect()
    }

    /// The next `count` windows that end after `now`: the one running now,
    /// if any, then those to come.
    pub fn next(&self, now: DateTime<Utc>, count: usize) -> Vec<Window> {
        let first = now.with_timezone(&self.time_zone).date_naive() - Days::new(8);
        first
            .iter_days()
            .take(usize::try_from(HORIZON_DAYS).unwrap_or(usize::MAX))
            .take_while(|day| self.ends_on.is_none_or(|end| *day <= end))
            .filter(|day| self.starts_on_day(*day))
            .map(|day| self.window_on(day))
            .filter(|(_, end)| *end > now)
            .take(count)
            .collect()
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

    fn date(text: &str) -> NaiveDate {
        text.parse().unwrap()
    }

    fn time(text: &str) -> NaiveTime {
        NaiveTime::parse_from_str(text, "%H:%M").unwrap()
    }

    const SUNDAY: u8 = 1 << 6;
    const TUESDAY: u8 = 1 << 1;
    const FRIDAY: u8 = 1 << 4;

    fn weekly(days: u8, at: &str, minutes: u32, zone: &str) -> Schedule {
        Schedule {
            repeat: Repeat::Weekly,
            every: 1,
            weekdays: days,
            month_day: None,
            month_week: None,
            months: EVERY_MONTH,
            start_time: time(at),
            duration_minutes: minutes,
            time_zone: zone.parse().unwrap(),
            starts_on: date("2026-01-01"),
            ends_on: None,
        }
    }

    fn monthly(day: Option<u8>, week: Option<u8>, days: u8) -> Schedule {
        Schedule {
            repeat: Repeat::Monthly,
            month_day: day,
            month_week: week,
            ..weekly(days, "22:00", 240, "Europe/Paris")
        }
    }

    #[test]
    fn weekly_windows_in_a_time_zone() {
        // Sundays 02:00 to 04:00 in Paris, UTC+1 in winter.
        let rule = weekly(SUNDAY, "02:00", 120, "Europe/Paris");
        // 2026-11-18 is a Wednesday.
        assert_eq!(
            rule.next(utc("2026-11-18T10:00:00Z"), 2),
            vec![
                (utc("2026-11-22T01:00:00Z"), utc("2026-11-22T03:00:00Z")),
                (utc("2026-11-29T01:00:00Z"), utc("2026-11-29T03:00:00Z")),
            ]
        );
        // During a window, that window comes first.
        assert_eq!(
            rule.next(utc("2026-11-22T02:00:00Z"), 1),
            vec![(utc("2026-11-22T01:00:00Z"), utc("2026-11-22T03:00:00Z"))]
        );
        // UTC+2 in summer.
        assert_eq!(
            rule.next(utc("2026-07-01T00:00:00Z"), 1),
            vec![(utc("2026-07-05T00:00:00Z"), utc("2026-07-05T02:00:00Z"))]
        );
    }

    #[test]
    fn every_other_week_counts_from_the_first_date() {
        // From Thursday 2026-01-01: the Sunday of its week is 2026-01-04,
        // and 2026-11-22 is 46 weeks later.
        let mut rule = weekly(SUNDAY, "02:00", 60, "UTC");
        rule.every = 2;
        assert_eq!(
            rule.next(utc("2026-11-18T00:00:00Z"), 1)[0].0,
            utc("2026-11-22T02:00:00Z")
        );
        rule.every = 3;
        assert_eq!(
            rule.next(utc("2026-11-18T00:00:00Z"), 1)[0].0,
            utc("2026-12-06T02:00:00Z")
        );
    }

    #[test]
    fn a_night_window_keeps_its_local_end_when_clocks_change() {
        // Saturdays 22:00 for 8 hours: to 06:00 on Sunday, local time.
        let rule = weekly(1 << 5, "22:00", 480, "Europe/Paris");
        // Clocks go forward on 2026-03-29 at 02:00: 7 real hours.
        assert_eq!(
            rule.next(utc("2026-03-27T00:00:00Z"), 1),
            vec![(utc("2026-03-28T21:00:00Z"), utc("2026-03-29T04:00:00Z"))]
        );
        // And back on 2026-10-25 at 03:00: 9 real hours.
        assert_eq!(
            rule.next(utc("2026-10-23T00:00:00Z"), 1),
            vec![(utc("2026-10-24T20:00:00Z"), utc("2026-10-25T05:00:00Z"))]
        );
    }

    #[test]
    fn a_start_in_the_skipped_or_repeated_hour() {
        // 02:30 does not exist on 2026-03-29 in Paris: read with the
        // offset before, it is 03:30 local, 01:30 UTC; the local end,
        // 03:30, is not after it, so the window lasts its hour.
        let rule = weekly(SUNDAY, "02:30", 60, "Europe/Paris");
        assert_eq!(
            rule.window_on(date("2026-03-29")),
            (utc("2026-03-29T01:30:00Z"), utc("2026-03-29T02:30:00Z"))
        );
        // 02:30 happens twice on 2026-10-25: the first, in summer time.
        // The end, 03:30, is in winter time: two real hours.
        assert_eq!(
            rule.window_on(date("2026-10-25")),
            (utc("2026-10-25T00:30:00Z"), utc("2026-10-25T02:30:00Z"))
        );
        // New York changes on other dates: 2026-03-08 and 2026-11-01.
        let ny = weekly(SUNDAY, "01:30", 60, "America/New_York");
        assert_eq!(
            ny.window_on(date("2026-11-01")),
            (utc("2026-11-01T05:30:00Z"), utc("2026-11-01T07:30:00Z"))
        );
    }

    #[test]
    fn monthly_windows_by_day_or_by_week() {
        let now = utc("2026-11-18T10:00:00Z");
        // On the 1st of every month at 22:00 Paris.
        let first = monthly(Some(1), None, 0);
        assert_eq!(
            first.next(now, 1),
            vec![(utc("2026-12-01T21:00:00Z"), utc("2026-12-02T01:00:00Z"))]
        );
        // On the 31st: months without one have no window.
        let last_day = monthly(Some(31), None, 0);
        let starts: Vec<_> = last_day.next(now, 3).into_iter().map(|w| w.0).collect();
        assert_eq!(
            starts,
            vec![
                utc("2026-12-31T21:00:00Z"),
                utc("2027-01-31T21:00:00Z"),
                utc("2027-03-31T20:00:00Z"),
            ]
        );
        // The second Tuesday: 2026-12-08, then 2027-01-12.
        let second_tuesday = monthly(None, Some(2), TUESDAY);
        let starts: Vec<_> = second_tuesday
            .next(now, 2)
            .into_iter()
            .map(|w| w.0)
            .collect();
        assert_eq!(
            starts,
            vec![utc("2026-12-08T21:00:00Z"), utc("2027-01-12T21:00:00Z")]
        );
        // The last Friday of January and July only.
        let mut last_friday = monthly(None, Some(LAST_WEEK), FRIDAY);
        last_friday.months = 1 | (1 << 6);
        let starts: Vec<_> = last_friday.next(now, 2).into_iter().map(|w| w.0).collect();
        assert_eq!(
            starts,
            vec![utc("2027-01-29T21:00:00Z"), utc("2027-07-30T20:00:00Z")]
        );
    }

    #[test]
    fn daily_windows_and_the_dates_they_apply_between() {
        let mut rule = Schedule {
            repeat: Repeat::Daily,
            every: 2,
            ..weekly(0, "23:00", 60, "UTC")
        };
        rule.starts_on = date("2026-11-18");
        rule.ends_on = Some(date("2026-11-22"));
        let starts: Vec<_> = rule
            .next(utc("2026-11-01T00:00:00Z"), 5)
            .into_iter()
            .map(|w| w.0)
            .collect();
        assert_eq!(
            starts,
            vec![
                utc("2026-11-18T23:00:00Z"),
                utc("2026-11-20T23:00:00Z"),
                utc("2026-11-22T23:00:00Z"),
            ]
        );
        assert!(rule.next(utc("2026-11-23T00:00:00Z"), 1).is_empty());
    }

    #[test]
    fn windows_between_two_instants() {
        let rule = weekly(SUNDAY | 1, "02:00", 120, "UTC");
        // From a Sunday 03:00, running, to the Monday after next.
        let found = rule.windows(utc("2026-11-22T03:00:00Z"), utc("2026-11-30T02:00:00Z"));
        assert_eq!(
            found,
            vec![
                (utc("2026-11-22T02:00:00Z"), utc("2026-11-22T04:00:00Z")),
                (utc("2026-11-23T02:00:00Z"), utc("2026-11-23T04:00:00Z")),
                (utc("2026-11-29T02:00:00Z"), utc("2026-11-29T04:00:00Z")),
            ]
        );
        // A window that starts at `until` is not in.
        assert_eq!(
            rule.windows(utc("2026-11-22T05:00:00Z"), utc("2026-11-23T02:00:00Z")),
            vec![]
        );
    }

    #[test]
    fn rules_that_cannot_be_kept() {
        let ok = weekly(SUNDAY, "02:00", 60, "UTC");
        assert_eq!(ok.validate(), Ok(()));
        let bad = [
            Schedule {
                duration_minutes: 0,
                ..ok.clone()
            },
            Schedule {
                duration_minutes: MAX_DURATION_MINUTES + 1,
                ..ok.clone()
            },
            Schedule {
                weekdays: 0,
                ..ok.clone()
            },
            Schedule {
                every: 53,
                ..ok.clone()
            },
            Schedule {
                ends_on: Some(date("2025-12-31")),
                ..ok.clone()
            },
            Schedule {
                months: 0,
                ..ok.clone()
            },
            monthly(None, None, 0),
            monthly(Some(1), Some(1), 0),
            monthly(None, Some(2), TUESDAY | FRIDAY),
            monthly(Some(32), None, 0),
            Schedule {
                every: 2,
                ..monthly(Some(1), None, 0)
            },
            Schedule {
                repeat: Repeat::Daily,
                every: 0,
                ..ok.clone()
            },
        ];
        for rule in bad {
            assert!(rule.validate().is_err(), "{rule:?}");
        }
        assert_eq!(monthly(None, Some(LAST_WEEK), FRIDAY).validate(), Ok(()));
        assert_eq!(Repeat::parse("weekly"), Some(Repeat::Weekly));
        assert_eq!(
            Repeat::parse(Repeat::Monthly.as_str()),
            Some(Repeat::Monthly)
        );
        assert_eq!(Repeat::parse("yearly"), None);
    }
}
