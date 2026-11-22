# ADR-0155: Availability reports

Date: 2026-11-18
Status: accepted; the time the central server was stopped is left out of the count (ADR-0173)

## Context

People who run servers for others, an MSP for its customers or an IT team for the rest of its company, are asked one question every month: "was it up?". Every tool they come from answers it: Zabbix services with SLA objects and their reports, PRTG's uptime per sensor, business process sensors and SLA reports, Nagios availability reports. MonitorFlow keeps everything needed (incidents with their timeline, offline incidents, silences and maintenance windows), but shows none of it as availability, so people copy incident lists into spreadsheets.

What a customer wants is short: one number per thing that matters to them ("Email: 99.95 % this month, target 99.9 %, met"), the outages behind it, and the planned maintenance kept apart, because it was agreed in advance. What the person sending it wants is not to think: the report goes out on the first of the month on its own, and a customer's report never shows another customer.

## Decision

### What counts as down

One rule for everything, which the report prints in its own words:

> **Down** is any time a critical incident was open: a check failing at its critical threshold, a server offline, or a server unreachable because what it depends on was down. Warnings never count. **Planned maintenance** is time under a silence or a maintenance window: it is shown apart and left out of the calculation.

Precisely, over a period:

- **A check** (`{name, instance}` of a server or device, the identity of ADR-0151) is down while an incident it writes (the matching of ADR-0151, `check_covers`) is critical, or while its server or device is down for being offline or unreachable: a check of a machine nobody can reach did not answer either.
- **A server or device** is down while any of its incidents is critical: its offline incident (ADR-0072) and its checks' incidents alike. A full disk at its critical threshold is an outage of the server; one at its warning threshold is not. The outage list says which incident each outage was, so a strict rule stays readable.
- **Unreachable** is down. A server behind a relay that is offline opens no incident of its own (ADR-0072), and a device whose checks a relay runs (ADR-0148) is in the same place: the relay's offline incident counts as theirs, with the relay named as the cause. Its users could not reach it either, and leaving it out would make a broken relay improve the numbers. The dependencies of ADR-0152 extend this: while a parent is down, what depends on it is unreachable and counts the same way, with the parent named. The relay assignment and the dependencies used are those of today, not of the time of the outage, which only matters when they changed during the period.
- The critical part of an incident runs from the event that made it critical (opened critical, or escalated) to the one that ended it (deescalated or resolved), from `incident_events`; an incident still open counts until now. Incidents are never deleted while their server exists, so any past period can be computed again with the same result.

**Planned maintenance** is time under a silence (ADR-0079) covering the thing: a silence of everything, its customer, its site or itself, an occurrence of a maintenance window, and for a check, a silence naming that check. For an incident, a silence naming the check that wrote it covers it too, so the work on one database counts against neither the database check nor its server. A silence counts from when it started or was created, whichever is later: making a silence during an outage stops the count from then on, never before. Silences of a relay cover the relay's offline incident, so planned work on a relay is planned for what is behind it.

**Availability** is the time up divided by the time counted: the period, from when the thing was added (or its first moment in the period) to its end or now, without the planned maintenance. With nothing counted (all of it under maintenance), availability is shown as "Not measured", never as 100 %. An **outage** is one unbroken stretch of down time; the report gives their number, total length, the longest, and each of them with its cause, start and end.

### Business services

A **business service** is a named group with one availability: "Email", "Website", "Accounting". It belongs to a customer, and its **members** are servers, devices or chosen checks of them, all of that customer. Its **rule** is one of:

- **All of them** (the default): the service is down while any member is down.
- **At least N**: the service is down while fewer than N members are up, for members that back each other up ("at least 2 of the 3 web servers").

A member under planned maintenance counts as up for the rule, and the time the service would be down only because of members under maintenance is the service's planned maintenance. So taking one of three web servers out for patches costs nothing, and taking all three out at once in a window is planned, not an outage.

A service may have an **SLA target**, a percentage between 50 and 100 with up to three decimals (`99.9`). The report says whether it was **met** (availability at or above the target) or **missed**, and gives the **error budget**: the down time the target allows over the time counted, minus the down time there was. What is left is shown as time, or how far it was exceeded.

Services are customer-wide objects, like dashboards shared with a customer (ADR-0095): users granted the customer as a whole see them, operators of the whole customer and above change them. A user with grants on sites only sees servers, devices and checks of those sites in reports, not services. Members must be of the service's customer, so a service never mixes customers.

### Reports

A **report** is the availability of a customer's services, servers, devices and checks over a period, any period from one hour to 13 months:

- **Summary first**, in plain words: the overall line ("2 services, 14 servers and devices; 3 outages for 1 h 12 min in all; planned maintenance 4 h"), each service with its availability, target and whether it was met.
- **Then the details**: services with outages, longest outage and error budget; servers and devices with the same; checks that had outages (checks at 100 % are counted, not listed); every outage with what it was, start, end and length; planned maintenance with its reason.

It is computed on demand from incidents and silences, never stored: a report for last month is the same today and next week, except when a server or service was removed since, which takes it out. The web interface shows it on **Reports** for any period (this month by default), and gives it as **PDF** and **CSV** in the same words. The CSV has one row per service, server, device or check with its numbers, then one row per outage, with a `kind` column, so a spreadsheet can filter it.

The **PDF** is made by the central server with `pdf-writer` (MIT or Apache 2.0, no system library, no browser) and the standard Helvetica fonts of every PDF reader, so it needs no font files and stays small. It is meant to be sent to a customer as is: a title with the customer and the period, the summary, then the details, page numbers, nothing internal (no IDs, no internal hostnames of the central server, no other customer). Times are in the report's time zone, written out with it. English and French, chosen per report.

### Scheduled reports

A **report schedule** sends a report by email: for one customer or, for administrators of everything, for everything; **monthly** (the default, on the first day of the month for the month before) or **weekly** (on Monday for the week before), at 06:00 in its time zone, in English or French, with the PDF, the CSV or both attached (both by default). It is sent through an email channel of the notifications (ADR-0060) that covers its customer, to the channel's recipients or to its own list.

The leading central server looks every minute for schedules whose last period sent is not the latest one ended, and sends it; the row is also claimed for 10 minutes (`claimed_until`), so two central servers (ADR-0089) never both send it even while leadership changes hands, and a failure is tried again 15 minutes later, with the error shown on the schedule. A report that was due while the server was down is sent when it comes back, once: only the latest period is ever sent. A new schedule starts with the next period. **Send now** sends the latest period at once, to check the setup, and leaves the schedule's own sending as planned.

Who: a schedule sends through a notification channel, and channels are managed by administrators of what they cover (ADR-0078), so schedules are too: administrators of the whole customer for its schedules, global administrators for everything. Every change is audited, and so is each report sent (`report.sent`), with its period and recipients.

### Single company and MSPs

With one customer, nothing says "customer": the report is titled with the period alone, services have no customer picker, and schedules default to everything. With a second customer, the report, the services and the schedules name the customer, and a report is always for one customer or for everything; a customer's report is computed only from its own sites, so it cannot show another's.

### API

Under the `alerts` area of API keys, documented in OpenAPI:

| Method | Path | Who |
| --- | --- | --- |
| GET | `/api/v1/availability?from&to&customer_id` | any role; what the key reads |
| GET | `/api/v1/availability/report?from&to&customer_id&format=pdf\|csv&language&time_zone` | any role |
| GET, POST | `/api/v1/business-services` | read: customer-wide grant; create: operator of the customer |
| PUT, DELETE | `/api/v1/business-services/{id}` | operator of the customer |
| GET, POST | `/api/v1/report-schedules` | administrator of the customer |
| PUT, DELETE | `/api/v1/report-schedules/{id}` | administrator of the customer |
| POST | `/api/v1/report-schedules/{id}/send` | administrator of the customer |

### Moving from another tool

The importer (ADR-0146) maps:

- **Zabbix services** (6.0 and later, read through the API: exports have none) to business services of the import's customer. A service watches the problems its tags select: its members are the checks whose triggers carry those tags, on the import's hosts, which is exact. "Most critical of child services" is all of them, "most critical if all children have problems" at least one, an additional rule "at least N child services have a problem" at least the others, and "fewer than N child services are OK" at least N. Child services are flattened into their members under the parent's rule; rules by percentage or weight, weights and "set status to OK" are approximated, each with its reason. The target is the SLO of the enabled SLAs whose service tags select the service, the strictest when several do; an SLA's schedule other than 24x7 and its excluded downtimes are approximated (maintenances already move as maintenance windows, which are planned maintenance). A service whose tags select no problem of the import's hosts is listed as not moved.
- **PRTG business process sensors** are listed as not moved, with their channels: neither PRTG's API nor its configuration file says which objects a channel watches, so the service is made by hand under Reports (a channel with an error threshold of T % over N objects is "at least" the smallest whole number not under N × T / 100). PRTG reports are not read by the importer.

## Consequences

- One rule explains every number, and the report prints it. It is strict for servers (a critical check is an outage of its server), which the outage list makes readable, and business services let people choose exactly what matters.
- Nothing is stored for reports: the incident history is the record, so numbers never drift from what the incident pages show. Computing a month for a customer reads its incidents and silences of that month, which is small.
- A removed server leaves past reports. Keeping reports as files was considered and left out: the schedule's email is the copy that customers keep.
- The PDF uses the base fonts: Latin text only, enough for English and French. A language with other scripts would need an embedded font.

## Alternatives considered

- **Availability from the metrics** (a check's success series in VictoriaMetrics): exact to the sample, but metrics have a retention and no notion of maintenance or of an incident's severity, and two numbers for the same outage would differ.
- **Warnings as degraded time**: a third state is common in other tools and seldom read; it can be added later without changing what down means.
- **HTML to PDF through a browser**: heavy, a browser in the server image, for a document with headings and tables.
