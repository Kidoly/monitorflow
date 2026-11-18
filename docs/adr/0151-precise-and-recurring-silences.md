# ADR-0151: Precise and recurring silences

Date: 2026-11-18
Status: accepted; amends ADR-0079 (what a silence covers) and ADR-0146 (how maintenances are imported)

## Context

A silence (ADR-0079) covers a whole server, relay, site, customer or everything, for at most 30 days. Two common needs do not fit:

- **One check under work.** Someone rebuilds the database of a server: its PostgreSQL service and port will fail for an hour, but a full disk or the server going offline must still page. Today the choice is to silence the whole server or to be paged for the work.
- **Planned work that comes back.** Patches every Sunday night, a backup that loads the disks on the first day of each month, a reboot on the second Tuesday. Today someone makes a silence each time, or forgets. Every tool people move from has these: Zabbix maintenances (one-time, daily, weekly, monthly, by hosts or tags), Alertmanager's `mute_time_intervals`, PRTG's maintenance windows, schedules and paused sensors. The importer (ADR-0146) can only give the next window of a recurring one, or silence a whole server for a paused sensor.

The promises of ADR-0079 must hold for both: incidents keep opening and resolving, only notifications are held, what is still open at the end is sent once, and a restart loses nothing.

## Decision

### A silence can cover chosen checks

A silence of a server or a site may name **checks**: a list of up to 20 `{name, instance}` pairs, the identity of a check in `sensor_configs` (ADR-0016). `checks` absent or `null` is the whole target, as before, and stays the default everywhere: the "Silence" menu of a server page silences the server. A relay, a customer or everything cannot be narrowed to checks.

**Which incidents a check covers.** An incident's metric is written by the check whose name is the longest one that prefixes it: `disk_data_used_ratio` comes from `disk_data`, not `disk`, as the web interface already reads it. A check `(name, instance)` covers an incident of its server (or of a server of its site) when, with `monitorflow_` removed, the metric is `name` or starts with `name_`, no other check name known to the central server (in `sensor_configs` or built in) is longer and prefixes it too, and the instance is the incident's when the check has one. The offline incident, approvals, failed updates and other events are not about a check, so a check silence never covers them: a server going offline during work on its database still pages.

**What is held.** Exactly what ADR-0079 holds, with the same rules at the end, in the same dispatcher pass. A server with only some checks silenced is **not** silenced: its status (ADR-0130) stays what its other incidents make it, and `silenced_until` of a server, relay or site counts only silences of whole targets. An incident covered by a check silence says so on its own: `GET /api/incidents` gives its `silenced_until`.

### Maintenance windows: silences that come back

A **maintenance window** is a rule in `maintenance_windows`: a reason, a place, a schedule in a time zone, and dates it applies between. It needs no renewing. The 30-day limit stays for one-off silences only.

**Place.** Always one site (`site_id`), which gives who sees and manages it. Either the whole site (`whole_site`), servers added later included, or chosen servers of that site (`maintenance_window_servers`, at most 500); a window of chosen servers whose servers are all removed covers nothing, it never widens to the site. Optionally narrowed to chosen checks, as above. So a window covers a site, a server, a group of servers, or checks of those.

**Schedule.** One of:

- `daily`: every `every` days (1 to 365) from the first date;
- `weekly`: on the chosen weekdays (`weekdays`, a bit per day, Monday first), every `every` weeks (1 to 52) from the week of the first date;
- `monthly`: on a day of the month (`month_day`, 1 to 31; a month without that day has no window), or on a weekday of a week of the month (`month_week` 1 to 4, or 5 for the last, with `weekdays` naming the weekday: "the second Tuesday", "the last Friday"), in the chosen months (`months`, a bit per month, every month by default).

A window starts at a local time (`start_time`) and lasts `duration_minutes`, 1 minute to 7 days. Windows that overlap are overlapping silences, which ADR-0079 allows. `time_zone` is an IANA name. `starts_on` is the first local date a window may start on, `ends_on` the last one, or `null` for no end.

**Daylight saving.** Times are wall-clock times in the window's zone, read with the rules on-call hand-offs use (ADR-0081, RFC 5545): a start that happens twice when clocks go back is the first; one skipped when they go forward is read with the offset before the change, so 02:30 on a night that jumps from 02:00 to 03:00 is 03:30. The end is the local start plus the duration, read the same way, so "22:00 for 8 hours" ends at 06:00 local time on the nights clocks change, after 7 or 9 real hours.

**Windows become silences.** The central server writes each window's occurrences as rows of `silences` with `window_id`, one per covered server, or one for the site, with the window's checks and reason. Everything that reads silences then needs no change: the dispatcher holds and releases exactly as ADR-0079 says, follow-up steps wait (ADR-0082), the fleet, banners and incident lists show them, and a restart loses nothing. Occurrences are written two days ahead, so a row always exists before its window starts: when a window is made or changed, in the same transaction, and every 10 minutes by every central server instance, which write a window under a lock on it and skip an occurrence its rule has written already, so they may run at once (ADR-0089). An occurrence someone ended early is not written again. An occurrence is at most 7 days, so `silences`' 30-day check still holds.

**Changing or removing a window** deletes its occurrences not started yet, which held nothing, and ends the running ones, then writes those of the new rule, in one transaction: a held event is released only when no silence covers it at the dispatcher's next look, so changing the reason during a window releases nothing. Removing a window sets `removed_at`; the row stays, as silences do, as the record of what was quiet and why. "End now" on a running occurrence ends that one only; the window comes back as planned.

**Showing it.** `GET /api/maintenance-windows` gives each window with its rule and its next three windows (`next`, start and end) and whether one is running now (`active_until`). The web interface writes the rule in plain words, "Every Sunday, 02:00 to 04:00 (Europe/Paris)", "On the second Tuesday of every month, 22:00 to 02:00 the next day", and lists the next windows. A running occurrence is a silence like another, so server, site and incident views say "In maintenance until 04:00: Patch night" instead of "Silenced until", from the silence's `window_id`. Dashboards' alert panels show the same, and their rows gain `silenced_until`.

### Who and what is recorded

Making, changing and removing a window takes an operator role on its site, like silencing a site (ADR-0079); anyone who reads the site sees it. They are audited as `maintenance_window.create`, `maintenance_window.update` and `maintenance_window.remove` with the place, the rule and the reason. A check silence is audited as any silence, with its checks. Everything is under `/api/v1` and in the OpenAPI document (ADR-0143). The silence's view gains `checks` and `window_id`.

### What the importer makes

Readers of ADR-0146 give, beside one-off silences with checks, recurring **windows** in the neutral inventory, and the server creates them as maintenance windows of the import's site:

- **Zabbix maintenances:** one-time periods become silences; daily, weekly and monthly periods become windows with the same rule, in the time zone chosen for the import (Zabbix's server zone is not in the export or the API). Hosts and host groups give servers. Tags give the checks made from the triggers carrying those tags on those hosts; a tag that matches no imported check silences the hosts, approximated with the reason. A maintenance "without data collection" holds notifications only, also approximated.
- **Alertmanager:** silences whose other matchers name alert rules (`alertname`) become silences of the checks made from those rules; `mute_time_intervals` and `time_intervals` of routes become windows, one per time range, for the hosts and checks the route's matchers select. Ranges MonitorFlow cannot say (several years, a day of month counted from the end other than the last) are approximated or left behind with why.
- **PRTG:** maintenance windows become silences; a paused sensor becomes a silence of its check instead of being left behind; a schedule that pauses objects becomes a window when its periods are weekly ranges, and stays behind with why otherwise.

## Rejected

- **A rule on the silence row itself.** It breaks "every silence ends" and the 30-day check that keeps it true, and every reader of silences would have to compute recurrences.
- **Computing windows when reading.** Statuses, banners and incident lists read silences in SQL; doing the same rules in SQL and in Rust, with time zones, invites them to differ. Writing occurrences once keeps one implementation, tested with a fixed clock.
- **RRULE text.** Hard to show in plain words and to check; the three shapes above cover what the tools people move from can express.
- **Silencing by metric or severity.** ADR-0079's reasons stand. A check is what people see on the server page and what they mean by "the database is under work".
- **Windows across sites.** A window's place gives who manages and sees it, as a silence's does. Windows for several sites are several windows; the importer makes one per site anyway.

## Consequences

- Work on one service pages nobody, while the rest of the server is still watched.
- Recurring work needs one rule, written once, shown in plain words with its next windows, and never forgotten on or off.
- `silences` gains `checks` and `window_id`; `maintenance_windows` and `maintenance_window_servers` are new. `silenced_until()` counts whole targets only; `incident_silenced_until()` adds check silences for one incident.
- A window covering 500 servers writes 500 rows per occurrence, two days ahead: cheap next to the outbox.
- The importer moves Zabbix, Alertmanager and PRTG maintenances as they are, instead of their next window or a whole server.
