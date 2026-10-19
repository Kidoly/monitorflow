# ADR-0020: Server page routing and hand-rolled SVG charts

Date: 2026-10-19
Status: accepted

## Context

The web interface needs a page per server with its details, graphs of its metrics (ADR-0018) and its sensor configuration (ADR-0016). It is a static export with no Node runtime (ADR-0014), so a route like `/servers/[id]` only exists for IDs known at build time, and servers are created at run time. The graphs need a charting approach, kept small for a short dependency list, and must later show threshold lines (ADR-0017) whose API is not there yet.

## Decision

**Routing.** The server page is one static route, `/servers/view/`, and the server ID is a query parameter: `/servers/view/?id=<uuid>`. The page reads it on the client with `useSearchParams` under a `Suspense` boundary, as Next requires for a prerendered page. Any static file server serves it, the Caddy example included (`try_files {path}/` ignores the query string), so the reverse proxy needs no rewrite rule. A malformed ID shows "not found" without calling the API. Alternatives set aside: a catch-all rewrite in the proxy to one `index.html` (every deployment would need the rule, and `next dev` would differ from production), and a hash route (`/servers/#id`), which hides the ID from the server logs but breaks the usual link semantics for no gain here.

**Charts.** A hand-rolled SVG line chart, `TimeSeriesChart`, of about 250 lines, instead of a library:

- The need is narrow: lines over time, one unit per chart (ADR-0015 puts the unit in the metric name), a crosshair readout and horizontal reference lines. That is a few scale functions and `<path>` elements.
- uPlot, the smallest serious candidate (about 50 kB minified), draws on a canvas. jsdom has no canvas, so its output could not be tested with Testing Library; SVG is plain DOM, so tests check lines, labels and accessible names directly.
- An SVG inside React needs no imperative lifecycle, resize observer or teardown, and follows CSS variables and CSS Modules like the rest of the interface.
- Recharts or ECharts weigh hundreds of kilobytes and bring transitive dependencies to keep up to date.

The cost is that we maintain axis ticks, gaps and hover ourselves; a chart with zoom, thousands of series or stacked areas would justify revisiting this. Series colours come from a fixed eight-colour order checked for colour-blind separation; a chart shows at most eight series and says how many it left out.

**Threshold lines.** The chart takes `referenceLines: { value, label, severity }[]` and draws each as a dashed horizontal line with a text label, included in the y scale. The server page reads `GET /api/servers/{id}/thresholds/effective` (ADR-0019) in the same 15 s cycle as its header and passes the lines through the graph section's `referenceLines(plan, instances)`, where `instances` are the series the chart shows. Each instance gets the threshold the engine would apply (ADR-0017): its exact entry if there is one, even muted, else the entry for every instance; a muted result draws nothing. Lines are then grouped by severity and value: a value every instance shares is drawn once, labelled "Warning" or "Critical"; one that only some instances have is drawn once per distinct value and names them ("Warning (/data)", "and N more" past two). A chart of eight disks with the default threshold keeps two lines, not sixteen. If the thresholds request fails, the graphs still show, without lines, and the page says so.

**Ranges and steps.** The graphs offer 1 h, 6 h, 24 h and 7 d. Each has a fixed step that keeps a series to a few hundred points, far below the API's 11,000 (ADR-0018), and never below the default 60 s collection interval for long ranges: 1 h at 30 s (120 points), 6 h at 2 min (180), 24 h at 5 min (288), 7 d at 30 min (336). The end of the range is aligned to the step, so successive refreshes return the same timestamps. The step samples the series (a value per step), it does not average it, so short spikes can be missed on long ranges.

**Refresh.** Like the fleet overview, the page reloads every 15 s, and a shared `usePolling` hook stops while the tab is hidden and reloads when it is shown again.

**Incident pages.** Incidents (ADR-0019) follow the same routing. The list is `/incidents/`, and its filters live in the query string (`state`, `severity`, `site`, `server`), so a filtered view can be shared and reloaded. Values the page does not recognise are dropped rather than sent to the API, which would answer 400. "Load more" follows `next_cursor`. A refresh asks for as many rows as are shown (up to the API's 500) in one request, so the list keeps its length every 15 s. An incident's page is `/incidents/view/?id=<uuid>`. It shows the threshold snapshot taken at opening, the events in API order (insertion order, not `occurred_at`, which can go backwards), and a chart of the series from a margin before it opened (a quarter of its duration, at least 15 minutes) to as long after it resolved, or to now. The chart's step comes from the graphs' steps, keeping it under 360 points. Its reference lines are the snapshot's values, not today's thresholds, and dashed vertical markers show the open and resolve times. A resolved incident's page stops refreshing.

## Consequences

- Links to a server are `/servers/view/?id=…`; `serverHref` builds them. `incidentHref` and `incidentsHref(filters)` do the same for incidents.
- There is no `GET /api/servers/{id}`, so the page header reads the server list and picks the server out. One endpoint for a single server would make the page cheaper on large fleets.
- Long ranges need a server-side rollup (`max_over_time`, `avg_over_time`) in the query proxy to show spikes; until then the 7 d view is sampled.
- The newest point lags by about 30 s (VictoriaMetrics' latency offset, ADR-0018) plus the collection interval; empty graphs say so.
- Charts have no table view or keyboard readout yet; the legend gives the latest values in text.

## Amendment 2026-10-19: server detail endpoint, certificates and rollups

**Header from `GET /api/servers/{id}`.** The server page reads its header from the detail endpoint instead of every page of `/api/servers`, so a refresh costs one row on any fleet size. A 404 (missing, outside the user's sites) shows "not found" as before. The header adds the agent certificate's expiry from `certificate.not_after`, as a warning under 14 days (`CERT_WARNING_DAYS`) and as an error once expired, and the revocation date and reason when the certificate is revoked. The fleet overview shows the same warning next to the status, from `cert_not_after`, and nothing for a certificate with more time left. Incident pages already read their header from the incident itself, so they do not change.

**Rollups on long ranges.** 24 h and 7 d send `rollup` (ADR-0018), so each point summarizes its step instead of sampling one value of it. The function is the step's worst value, chosen from the metric name: `min` for headroom and health (`available`, `free`, `_up`, `_active`, `_loaded`), so a dip or an outage shows; `max` for usage and pressure (`_ratio`, `_used_bytes`, `cpu_load*`, `_seconds`, `_status`, `_failed`, `_matches`, `_lag_bytes`, `_restarts`), so a spike shows; `avg` for sizes and custom gauges of unknown meaning. The worst value rather than the average matches what thresholds alert on: a threshold line next to an averaged curve would hide the crossing that opened an incident. The step is the range's step or, when longer, the interval of the sensor that writes the metric (the sensor with the longest name prefixing it, longest interval across its instances, from the configuration the page already loads; 60 s until it is known) rounded up to a multiple of the range's step, so every step holds a sample instead of drawing gaps. 1 h and 6 h stay raw: at 30 s and 2 min steps a point is close to one sample, and the raw value reads best when zoomed in. This replaces the "Ranges and steps" caveat and the rollup consequence above.
