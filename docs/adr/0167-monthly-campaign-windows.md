# ADR-0167: Monthly and yearly campaign windows

Date: 2026-11-22
Status: accepted; amends ADR-0033

## Context

ADR-0033 gave campaigns weekly windows and one-off ranges. Many teams patch on a monthly rhythm instead: the night after Microsoft's second Tuesday, the first Saturday, the last weekend of the month, a quarterly or yearly freeze-free night. With weekly windows only, an administrator either picks one week by hand each month (a one-off window, typed again every month) or lets the campaign run every week.

Recurring silences (ADR-0151) already have a monthly rule: a day of the month, or a weekday of a week of the month with 5 for the last, in chosen months. Administrators meet both screens; the two rules should read the same.

## Decision

A third kind of campaign window, `monthly`, with the rule of ADR-0151:

- `month_day` 1 to 31, alone. A month without that day has no window, as for silences: "the 31st" is not "the last day".
- or `month_week` 1 to 4, or 5 for the last, with `weekday` (ISO, 1 is Monday): `{"month_week": 2, "weekday": 2}` is the second Tuesday, `{"month_week": 5, "weekday": 5}` the last Friday.
- `months`, 1 to 12, every month when empty or absent; the API answers the list in full. One month makes a yearly window; four make a quarterly one. There is no `yearly` kind: it would be the same rule with fewer choices.
- `start`, `end` and `timezone` as for weekly windows: an end before the start closes the next day.

Storage: `campaign_windows` gains `month_day`, `month_week` and `months`; the weekday is the single element of `weekdays`, so the column means the same for both recurring kinds (migration `20261203095000_campaign_monthly_windows`). Constraints keep each kind's columns consistent.

Evaluation stays in PostgreSQL, as ADR-0033 chose: `campaign_window_day(rule, date)` says whether a recurring window opens on a local date, and `campaign_window_end` calls it for today and, for windows past midnight, yesterday. The last week of the month is the one where the same weekday a week later is in the next month.

The web interface shows each window as a card: Weekly with the weekdays as toggles, Monthly with "On day N" or "On the second Tuesday", Yearly as a monthly window in one month, and One-off; the hours and time zone on one line.

## Consequences

- Existing windows are unchanged; `weekly` and `once` read and evaluate as before.
- API clients that switch on `kind` must expect `monthly`.
- Daily windows are a weekly window with every weekday.
