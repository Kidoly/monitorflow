# ADR-0180: Uptime at a glance

Date: 2026-11-22
Status: accepted

Numbered ADR-0172 when merged, renumbered because ADR-0172 is the history of the server's updates.

## Context

Reports (ADR-0155) answer "was it up last month?" with one number per thing and the outages behind it. What people check most is shorter and visual: the availability over the last day, week, month and quarter, and a row of daily bars where a bad day stands out in color, per server or check. Customers expect that page and the same picture in the PDF they receive.

## Decision

**One rule, new slices.** Uptime uses the measures of ADR-0155 unchanged (what counts as down, planned maintenance left out, unreachable counted as down, "Not measured" rather than 100 %). The report's code gives, for each business service, server, device and check with outages of its own, when it existed and its timeline of down time and planned maintenance over the 90 days; uptime cuts that timeline into:

- **Windows**: the last 24 hours, 7, 30 and 90 days before an instant (now by default).
- **Days**: 90 bars, the oldest first, today so far last, cut at midnight in a chosen time zone (the viewer's on screen), each with its availability, down time and planned maintenance. A day before the thing existed, or entirely under planned maintenance, is "Not measured".

Checks without an outage in the 90 days are left out, as in reports: they would be 90 green bars each.

**API.** `GET /api/v1/availability/uptime?customer_id&at&time_zone`, area `alerts`, any role, with the same reach as reports: only the sites the caller reads, services only of customers granted as a whole. Computed on each request from the incident history, nothing stored.

**Page.** Reports opens with "Uptime": each row with its name, the four percentages and its 90 bars, colored by how much of the day was up (all of it, a little less, much less, or not measured), each bar saying its date, availability and down time on hover and to screen readers. The report of the chosen period, with its outages, follows below.

**PDF.** The report PDF gains a header on every page (the logo, drawn as vectors, the product name and the period) and an uptime section after the summary: a table of the four windows ending with the period and, under each row, the daily bars of the period's days, with a legend. Same base fonts and no embedded files (ADR-0155).

## Consequences

- One computation behind the report, the page and the PDF, so their numbers agree.
- A view reads 90 days of incidents and silences of what it shows; that is the same order of work as a quarterly report.
- Colors alone never carry the meaning: every bar has its numbers in text, and the table keeps the percentages.
