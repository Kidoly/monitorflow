# ADR-0095: Dashboards

Date: 2026-11-15
Status: accepted

## Context

The fleet list and the server and sensor pages answer "what is wrong now" and "what did this one sensor do". People also want their own page: the CPU of the database servers of one customer, the five fullest disks of a site, the open alerts of the servers they look after, side by side. Today that means opening servers one by one. Grafana would do it, but it is a second product to install, secure and keep in step with our tenancy, and it asks for queries.

The owner's rule applies: as simple as possible to use, the interface doing the work by itself. A dashboard here is built by picking from lists, works with no setup, and never shows a customer's data to another one, whoever edits it and however.

## Decision

**What a dashboard holds.** A name, who it is shared with, a default time range and an ordered list of up to 24 panels. A panel is one of:

| Kind | Shows | Choices |
| --- | --- | --- |
| Chart | One metric over time | Servers, metric, what it watches (a disk, a core, "all"), and how servers combine: one line each (the 8 worst), or their average, highest, lowest or total |
| Figure | One number now | The same, with "how many servers report it" instead of one line each |
| Top list | The servers or disks with the highest or lowest value now: "5 fullest disks" | Servers, metric, what it watches, highest or lowest, how many (1 to 20) |
| Open alerts | The open alerts of the servers, worst first | Servers, how many (1 to 20) |

Servers are every server of the dashboard (the default), a site, a customer, or chosen servers (at most 50). Metrics are picked from those the chosen servers report (VictoriaMetrics' label values over the last day, filtered to those servers), with plain titles for the built-in ones. "One line each" draws the 8 worst series, `topk_max(8, …)` or `bottomk_min` for metrics where low is bad (available, free), the same rule as the rollups of ADR-0020, because the chart has 8 colours and a line per server of a thousand is noise.

Servers have no tags yet, so "servers with tag X" is not offered. The servers choice is a tagged object (`{"kind": "site", "id": …}`), so a `tag` kind can be added later without a new schema version.

**No query language.** There is no expert mode with raw MetricsQL. The API builds every query, as it does for the server pages (ADR-0018). Offering one safely would mean parsing MetricsQL on the server, refusing every selector without our tenant matchers, and still passing `extra_filters`; a missed function or subquery would read another customer's series. Nobody asked for it, and the four kinds cover what the server pages do not. If it is ever added, it is for global administrators only, and it goes through the same filters as below.

**Who sees and edits.** A dashboard is shared with one of:

- **Only me**: its owner sees and edits it, whatever their role, viewers included.
- **A site**: everyone who can read the site sees it; operators and above on the site edit it.
- **A customer**: everyone with a role on the customer as a whole, or on everything, sees it; operators and above on the customer as a whole edit it. Someone with a role on one of its sites only does not see it: its name and panels may be about the other sites.

Changing who a dashboard is shared with needs the right to edit it under both the old and the new sharing. Creating, changing and deleting dashboards are audited (`dashboard.create`, `dashboard.update`, `dashboard.delete`) with the name, sharing and number of panels, as silences are. Opening one is not: it reads nothing the user could not read on the server pages.

**Tenancy, twice.** On save, every site, customer or server a panel names must be one the editor can read and lie within the sharing: a site's dashboard names only that site or its servers, a customer's only its sites and servers. Otherwise it is refused with one message for "does not exist" and "not yours", so nothing is revealed. On display, which is what matters, the panel's servers are resolved again for the **viewer**: the servers of the panel, within the dashboard's sharing, that the viewer can read, now. A dashboard edited by hand in the database, or a server moved to another customer since, shows nothing it should not: the query is built from that list only. Each query selects by the site names of those servers (`site=~"…"`), adds their peer names when the panel names servers, excludes removed servers of those sites (`peer!~"…"`), and passes the same site filter as VictoriaMetrics `extra_filters[]`, so a bug in the builder still cannot reach another site's series. Series coming back with a peer outside the list are dropped. Site and peer names are unique, and regex characters in site names are escaped.

**Time range and refresh.** Ranges are the ones of the sensor pages: 1 h, 6 h, 24 h, 7 d, 30 d, with the same steps and rollups (ADR-0020). The dashboard keeps a default; changing it while viewing only changes the address, so a link shares what one sees. The page refreshes every 30 seconds while visible, like the other pages but less often, since a dashboard is looked at, not watched for a change.

**Layout.** Panels sit in a grid of three columns, each one, two or three columns wide, in the order of the list. Editing is done in place: "Move earlier", "Move later" and a width choice on each panel, plus dragging a panel by its handle on a wide screen. On a narrow screen the grid is one column and the buttons still work. Free resizing on a grid of cells, as Grafana does, was left out: it needs a layout library, is not usable by keyboard or on a phone, and three widths cover the cases.

**Ready-made dashboards.** Two are always there, built in code and not stored: "Fleet overview" (servers reporting, average CPU, fullest disk, CPU and memory of the busiest servers, the busiest servers and fullest disks, open alerts) and "Disks and memory". They show every server the user can see, within the customer chosen in the scope switcher. They cannot be edited; "Make a copy" turns one into the user's own dashboard, as it does for any dashboard the user can see. A copy starts as "Only me", so copying needs no right beyond reading.

**Storage.** A row of `dashboards`: owner, site or customer (exactly one), name, `spec` as JSONB, `schema_version`, and a `revision` that a save must name. Two people editing at once do not overwrite each other: the second save is refused with "Someone changed this dashboard since you opened it". The spec is `{"version": 1, "range": "6h", "panels": [...]}`, read with `deny_unknown_fields` and validated on every write: kinds, sizes, ranges, metric names as for the query API, watched instances as for sensors, lengths and counts. A later version of the spec is read by upgrading older ones in code when they are read; a stored spec is never rewritten by a migration. A user's personal dashboards go when the user is deleted, a site's or customer's with them.

**When things disappear.** Nothing is rewritten. A removed server is left out of every panel; when none of a panel's servers is left, the panel says so ("None of these servers can be shown: they were removed, or you cannot see them") and the editor drops removed servers from the list the next time it is saved. A metric no longer reported gives "No data in this range", and the panel stays, so it comes back when the metric does.

**Performance.** One request fetches a whole dashboard, `POST /api/dashboard-data` with the panels: the browser does not send twelve. The API reads the servers the viewer can see once from PostgreSQL, builds every panel's query, runs at most 6 at a time against VictoriaMetrics, and reads the open alerts of all alert panels in one statement. The load test (ADR-0090, `docs/load-test.md`) showed VictoriaMetrics far from its limits at 1,000 servers, and that the costs to avoid are many small requests holding pool connections, not queries; a dashboard request holds a connection for two statements. The end of each range is aligned to its step, as on the sensor pages, so a refresh asks the same query, and answers are kept 15 seconds per instance of the central server, keyed by the exact query, filters included: two users share an entry only when they would have run the same query, so the cache cannot carry data across customers. Panels aggregate in VictoriaMetrics, so at most 8 series cross the network per chart however many servers it covers. A query that would select too many servers by name (over 50 named, or a filter longer than 12 KB) is refused with a sentence instead of reaching VictoriaMetrics' query length limit.

## Consequences

- Users build pages of charts, figures, top lists and alerts without a query, shared with a site or customer by the same roles as everything else.
- Tenancy holds on display whatever is stored, and is checked on save so stored dashboards make sense to those they are shared with.
- No new setting, no new service. One table.
- Not in this change: tags on servers (and so panels by tag), panels of logs (ADR-0091's journald work may add them), thresholds drawn on dashboard charts, a dashboard as the home page, and exporting a dashboard as a file.

## Alternatives considered

- **Grafana, embedded or alongside.** A second product with its own users and query language; making it respect our tenancy needs a proxy that rewrites its queries, which is the hard part anyway.
- **Panels querying the existing per-server endpoint.** Twelve panels over a site would be thousands of requests; one batched request with server-side aggregation is the only way to stay fast at 1,000 servers.
- **Dashboards as rows of panels in their own table.** A JSON spec with a version keeps a dashboard one value to validate, copy, compare and save atomically; panels are never queried on their own.
- **Raw MetricsQL for experts.** See above.
