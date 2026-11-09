# ADR-0101: A public API

Date: 2026-11-18
Status: accepted

## Context

Everything MonitorFlow does goes through its REST API (architecture §9), but that API was made for the web interface: it is signed in with a session cookie that lasts 12 hours, guarded by a CSRF header, undocumented outside `crates/server/src/api/mod.rs`, and free to change with the interface.

Two kinds of people want to use it from their own tools. An MSP wires MonitorFlow into HaloPSA, its billing or its own scripts, often one integration per customer, run by someone who should never see the other customers. A single company automates with `curl` and Ansible: acknowledge an incident from a chat bot, silence a server during a deployment, read the open alerts into another dashboard. Both need a credential that is not a person's password, that is worth little if it leaks, that says what it may touch, and a contract that does not change under them when MonitorFlow is updated.

The owner's rules apply: as simple as possible to set up and use, in plain words, for a single company and an MSP alike, and never a second API to keep in step with the first.

## Decision

**One API, one more path.** The public API is `/api/v1/…`: the same paths as the web interface's, after `v1/`, served by the same handlers with the same rules, so `GET /api/v1/servers` is `GET /api/servers`. Nothing is written twice and the tenancy rules (ADR-0026) are the same code. `/api/…` stays the web interface's, free to change in any release; `/api/v1/…` is the contract. When a change to an `/api` route would break it, `v1` keeps the old shape with an adapter in front of the handler, and a later `v2` is only made when that becomes unreasonable. Additions (fields, routes, parameters) are not breaking; clients must ignore fields they do not know.

**What v1 leaves out.** Signing in and out, first-run setup, password links, acknowledge links, the screens of display links and the API keys themselves: they are for people in a browser, or a key could make itself another key that outlives it. These answer 404 under `/api/v1`. Everything else the web interface does is in v1 from the start, and every route added to the web interface later is in v1 too unless added to that list.

**API keys.**

- **Made in the web interface,** under Admin › API keys, by administrators: a global administrator for anything, a customer's administrator for that customer, a site's administrator for that site. Each sees the page with only the keys they could have made, so an MSP's customer manages their own keys without seeing anyone else's. A key is a credential that outlives a session, so making one is an administrator's decision, as for install keys (ADR-0073) and display links (ADR-0099).
- **Shown once.** `mfk_` and 43 characters of base64url: 256 random bits, of which only the SHA-256 is stored, as for display links. The prefix lets secret scanners and people recognise it; the first 8 characters after it are kept to tell keys apart in the list.
- **What it may do**, chosen when it is made, never changed after (make another and turn the first off):
  - **Access**, one of the roles: "Read only" (viewer), "Read and change" (operator), "Approve commands" (approver), "Administer" (administrator). "Read only" is the default.
  - **Reach**: everything, one customer or one site. Customers are not offered until a second one exists, so a single company sees "Everything" or a site.
  - **Areas**, optional, default all: servers, alerts, updates, remediation, dashboards, administration (below). A HaloPSA connection needs "alerts"; a key that only acknowledges incidents is worth less if it leaks.
  - **Expiry**: 30 days, 90 days, 1 year (the default) or never.
- **It acts as its maker, narrowed, checked on every request.** Each request loads the maker's roles of now, keeps the grants that fall within the key's reach (a global grant becomes one on the reach; a customer grant on another customer is dropped), and lowers each role to the key's access. A key never does more than its maker could at that moment: a maker who loses a role or a customer takes it away from the key too, one who is disabled finds the key refused, one who is deleted takes the key with them. Handlers see an ordinary user with fewer grants and need no change.
- **Areas** are read from the route's path: the area of servers and relays, their metrics and logs is "servers"; incidents, silences, on-call, follow-up, thresholds, checks and their settings, offline alerts and notification channels "alerts"; commands, campaigns, packages, sources, mirrors, rollback and the update settings "updates"; `/api/remediation/…` and AI commands "remediation"; dashboards "dashboards"; everything else "administration". Any key may list servers, relays, sites and customers within its reach, which every integration needs to turn names into IDs. A route outside the key's areas is 403 `api_key_scope`.
- **Used** with `Authorization: Bearer mfk_…` under `/api/v1`, and nowhere else: the web interface's paths do not accept keys, so the contract is the only thing a key can reach. A request with a key needs no CSRF header, as no browser sends one on its own; a request with a cookie under `/api/v1` is treated as under `/api`, CSRF included.
- **Listed** with its name, what it may do, who made it, when, when it expires and when it was last used (written at most once a minute). **Turned off** from that list by anyone who could have made it; it stops at its next request.
- **Audited.** `api_key.create` and `api_key.revoke` with the key's name, access, reach, areas and expiry, never any part of the secret (not even the prefix the list shows). Logs name keys by ID only. What a key changes is audited as its maker's action, as now, with `api_key` (the key's ID) added to the row's details, so the audit log tells a person from their script.

**Errors.** Every error body keeps `error`, a sentence safe to show, and gains `code`, stable and machine-readable: `invalid_request` (not the expected JSON), `invalid` (the content is wrong; `error` says why), `not_found`, `conflict`, `gone`, `unauthenticated`, `invalid_credentials`, `forbidden`, `api_key_scope`, `cross_site`, `rate_limited`, `idempotency_key_reused`, `idempotency_in_progress`, `metrics_unavailable`, `metrics_timeout`, `logs_unavailable`, `logs_timeout`, `internal`. Codes are only added, never renamed; scripts branch on `code` and show `error`. This applies to `/api` too, where the web interface ignores it.

**Pagination and filters.** Lists that can grow without bound already take `limit` and an opaque `cursor` and answer `next_cursor`, `null` on the last page (servers, relays, incidents, commands, campaigns, tokens, remediation incidents and events). v1 keeps that one form: no page numbers and no totals, which are wrong as soon as a row is added. Filters are the query parameters each list already takes, exact values ANDed together, with unknown parameters and bad values 400. Lists that stay small (sites, customers, silences, dashboards) answer at once, in full.

**Idempotent writes.** A `POST`, `PUT` or `DELETE` with a key may carry `Idempotency-Key` (1 to 255 visible ASCII characters, a UUID usually). The first request with a key and value runs and its answer is kept 24 hours; the same request again, with the same method, path and body, gets that answer back with `Idempotent-Replayed: true` instead of running again, so a script that lost its connection can retry without opening two silences. The same value with another request is 422 `idempotency_key_reused`; while the first is still running, 409 `idempotency_in_progress`. Answers of 500 and above are not kept, so the retry runs again. Answers that hand out a secret once (enrollment tokens, install keys, display links, password links, integration signing secrets) are not kept either: a replay of one says it was done and that the secret is shown only once, rather than storing secrets in the database. Values are kept per key, so two keys never see each other's answers. Without the header, writes behave as now: most are already safe to repeat (acknowledging an incident twice, setting a value) and the documentation says which are not.

**Rate limits.** 600 requests a minute per key, in a fixed window, in memory per instance of the central server (as ADR-0099's). Every answer to a key carries `RateLimit-Limit`, `RateLimit-Remaining` and `RateLimit-Reset` (seconds until the window ends); over the limit, 429 `rate_limited` with `Retry-After`. 600 is ten a second, enough for a PSA polling every minute and a script working through a thousand servers, and little enough that a runaway loop does not load the server. Sessions are not limited: they are people.

**OpenAPI.** The document is generated from the code with `utoipa`: the request and response types derive their schemas, and each documented route is described next to the router in `api/openapi.rs`. The server serves it, without a session, at `/api/v1/openapi.json`; the web interface shows it as a page, "API documentation", linked from Admin › API keys. `docs/api/openapi.json` is the generated document committed, and a test fails when it differs from what the code generates (`UPDATE_OPENAPI=1 cargo test -p monitorflow-server openapi` rewrites it), so CI catches a change to the contract that nobody looked at. Another test fails when a documented route does not exist, and when a v1 route is neither documented nor listed as "not yet documented", so a new route is a decision.

**Coverage of v1.** Everything the web interface does is reachable, with the same rules, apart from the routes left out above. The first OpenAPI document describes the part an MSP's PSA or a company's scripts use first:

- Servers, sites and customers: list servers (paged) and read one, list sites and customers, create a customer and a site under it.
- Incidents: list with filters (paged), read one with its timeline, acknowledge. Silences: list, create, end.
- Commands of a server: list (paged), queue (package updates), read one with its output.
- Approvals: pending commands, approve, reject.
- Metric queries, instant and over a range.

Reachable now and described next, in this order: thresholds and check settings, campaigns, dashboards, relays and enrollment (tokens, install keys, pending hosts), notification channels and routing, integration endpoints (ADR-0102), on-call and follow-up, sources and mirrors, remediation rules, incidents and events, settings, users, backups, the certificate authority. The list lives in `api/openapi.rs` as `NOT_YET_DOCUMENTED`; describing a route takes it out.

**Not done:**

- **A second API**, with its own handlers and types. It would drift from the first one, and tenancy would be checked twice in two ways.
- **OAuth 2 client credentials.** A token endpoint, client registrations and token lifetimes, for clients that all send one fixed header anyway. Keys can be put behind OAuth later without changing the routes.
- **Keys tied to no person** ("service accounts"). A key without a maker would need its own roles, its own pages and its own way of losing rights; tying it to its maker reuses all of that and makes it go when they go. An MSP that wants a key to survive a technician leaving makes it as an administrator account kept for that.
- **Fine-grained scopes per route** (`incidents:acknowledge`). Six areas and four levels already give 24 kinds of key; per-route scopes would make the form a matrix nobody fills in right.
- **Page numbers and totals.** Above.
- **Rate limits shared between instances** of the central server. Each instance counts its own, so behind two instances a key gets up to twice the limit; the limit is there to stop accidents, not to bill.
- **Swagger UI.** A large script served from a CDN or bundled; a short page of our own reads the same document.

## Consequences

- Scripts and PSA integrations use one header and one documented, stable path, and the web interface keeps changing freely.
- A leaked key does what its maker could, within its reach, access and areas, until it expires or is turned off, and its use shows in the audit log and the "last used" date.
- A breaking change to an `/api` route now needs a v1 adapter or a deliberate v2; the OpenAPI test makes that visible in review.
- Idempotency answers live in PostgreSQL for 24 hours; rate limits reset on restart and are per instance.

## Amendment (2026-11-09): what belongs to the person is left out

A security review found routes where a key acted with its maker's whole identity rather than its narrowed reach. "Handlers see an ordinary user with fewer grants and need no change" holds for what is checked against grants; it does not hold where a handler acts on the person (`user.id`) or hands out a credential of its own. These are now left out of v1 too and answer 404 under `/api/v1`, changing "everything else … is in v1" and the coverage list above:

- **Display links** (`/api/display-links`, `/api/dashboards/{id}/display-links`). A link reads as its maker with all their roles (ADR-0099) and outlives the key: a key for one site could put a personal dashboard of "all servers" on a link and read every site its maker reads, after the key was turned off too. Links are made, listed and turned off by a person in the web interface.
- **The person's own on-call contact** (`/api/on-call/contact` and its test). It is where their pages go, with links that acknowledge as them; a read-only key could point it at its own webhook. Schedules, overrides and people stay in v1. Through a key, the schedules listed are those of what the key reaches, not also those its maker is on.
- **Inviting a user and issuing a password link** (`POST /api/users`, `POST /api/users/{id}/link`). The link becomes a person's session, for a new administrator or an existing one, which outlives the key: the reason a key cannot make a key. Listing users, granting and revoking roles, disabling and removing stay in v1 for a key that reaches everything.

The handlers also refuse a request made with a key (403), should one of these routes ever be reached another way. Every route added later that acts on the signed-in person or hands out a credential for people goes on this list (`v1::left_out`).

**Idempotent replays check the key.** A stored answer was given again by the key's hash alone. It is now given only for a key that works: not expired, its maker not disabled; otherwise the request goes on to the usual 401. A replay, and a refusal for a reused or running value, counts against the key's rate limit like any request.

Not changed, and known: a key sees, edits and deletes its maker's "Only me" dashboards, and makes them, whatever its reach; their data is still resolved within the key's reach. A key that reaches everything as administrator can grant roles to existing users, as its maker can.
