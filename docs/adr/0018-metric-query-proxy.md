# ADR-0018: Metric query proxy with server-built queries

Date: 2026-10-19
Status: accepted

## Context

The web interface needs graphs, so the REST API must serve metrics from VictoriaMetrics (ADR-0002). Tenancy is enforced in the API (architecture §9): a user whose roles cover site A must never see series of site B. Every series carries `peer` and `site` labels set by the server, never by the sample (ADR-0013), so a query filtered on them is trustworthy. Two designs were possible:

- **Accept PromQL** from the browser and enforce the filters with VictoriaMetrics' `extra_label` / `extra_filters[]` parameters, which add matchers to every selector of the query.
- **Accept a structured request** (server, metric, instance, range, step, aggregation) and build the PromQL in the server.

## Decision

**Structured requests.** `GET /api/metrics/query` and `/api/metrics/query_range` take `server_id`, `metric`, optional `instance` and `agg` (`avg`, `min`, `max`, `sum`, `count`), and the time parameters. The server reads the server's row with the user's site scope (404 outside it, before anything is sent), then builds `agg(<metric>{peer="…",site="…",instance="…"})` with every value quoted as a PromQL string. Arbitrary PromQL is not accepted:

- PromQL is a large language (subqueries, `label_replace`, `@` modifiers, functions over many series). `extra_label` relies on VictoriaMetrics rewriting every selector correctly in all of it; a builder for a handful of forms is small enough to test fully.
- Free PromQL also lets a user write expensive queries (wide regexes, long subqueries) that the step and range bounds below do not cover.
- The graphs the interface needs (one metric of one server, per instance or aggregated) fit the structured form. Cross-server views (a site's average CPU) can be added later as more structured forms, with the same scoping.
- The builder does not depend on VictoriaMetrics; only the extra filters below do.

**Defense in depth.** The request also carries `extra_label=peer=…` and `extra_label=site=…`, so VictoriaMetrics enforces the same filters even if the builder had a bug. A Prometheus-compatible store that ignores the parameter still gets the matchers in the query. Discovery drops any returned series whose `peer` is not the server's.

**Metric names.** `metric` must be `monitorflow_` followed by a valid sensor metric name (ADR-0012, ADR-0015). This acts as an allowlist by pattern, not a fixed list: configured sensors choose their names (`port`, `disk_data`), so a fixed list would refuse valid metrics. The pattern keeps everything outside the `monitorflow_` namespace and every PromQL token (`{`, `(`, spaces) out. The instance follows ADR-0012's value rules.

**Discovery.** `GET /api/servers/{id}/metrics` lists the metric names and instances the server wrote in the last day, from `/api/v1/series` with the same matchers and extra filters, limited to 10,000 series (`truncated` says when the limit was hit).

**Bounds.** A range is at most 31 days (VictoriaMetrics' default retention), at most 11,000 points per series (the Prometheus limit), and a step of at least 1 s. Without a step the API uses the range over 720. An answer has at most 500 series; above that the API returns 400 and asks for an instance filter or an aggregation instead of truncating, because a partial graph that looks complete is misleading. Responses are read up to 16 MiB.

**Backend client.** `vm_query::VmQuery` is a separate client from the ingest writer: a 15 s timeout, no redirects, and only the parameters the handler builds. Cookies, `Authorization` and other headers of the browser request are never forwarded. Errors map to clean API errors: unreachable, 5xx, 429 or a malformed answer give 502 `metrics backend unavailable`, a timeout 504, a 4xx (for instance a VictoriaMetrics limit) 400. Details are logged, never returned, and never include the URL.

## Consequences

- The interface cannot run ad-hoc PromQL. An "explore" screen for administrators, if needed, would be a separate route with its own role check and the `extra_label` filters.
- Each query costs one indexed PostgreSQL lookup for the scope check before VictoriaMetrics is called.
- Queries over several servers or sites need new structured forms; each must apply the same scope.
- Instant queries at "now" see points older than VictoriaMetrics' latency offset (30 s by default), so the latest point of a graph can lag by that much.
