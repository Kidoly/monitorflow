# ADR-0143: A complete public API, proved by a test

Date: 2026-11-16
Status: accepted; amends ADR-0101

## Context

The owner's rule: everything that can be done in MonitorFlow must be doable through the public API (`/api/v1`, API keys) and described in its OpenAPI document. ADR-0101 made `/api/v1/<path>` serve `/api/<path>` with the same handlers, minus a list left out for people, and let routes stay undocumented for a while (`NOT_YET_DOCUMENTED`). Nothing checked that what the web interface calls is in the contract, and the escape hatch let a new route skip its documentation.

## Audit (2026-11-16)

Every route of the router (212 under `/api`, from `crates/server/src/api/mod.rs`), every call of `web/src/lib/api.ts` and `web/src/lib/routes.ts`, and every other request the web interface makes. By the area of an API key (ADR-0101):

| Area | Routes | Documented before | Only a global administrator's key |
|---|---|---|---|
| servers, with the four listings any key reads | 23 | 19 | 0 |
| alerts | 42 | 40 | 0 |
| updates | 41 | 41 | 19 |
| remediation | 14 | 14 | 0 |
| dashboards | 7 | 7 | 0 |
| administration | 57 | 57 | 35 |
| health, no key needed | 2 | 2 | 0 |
| left out of v1, for people (below) | 26 | not in v1 | none |

All 186 routes in v1 are served to a key with the same handlers and rules as to a person. The full table, route by route, is in the pull request that added this ADR.

Routes served but not documented: `GET /api/checks/usage`, `GET /api/servers/{id}/sensor-values`, `GET /api/servers/{id}/recommendations` with its `dismiss` and `restore`, and `PUT /api/relays/{id}/offline-alerts` (its server and site twins were documented). Every web call matches a route; `GET /api/v1/openapi.json` is served without a key. Seven routes have no caller in the web interface and exist for scripts: the five of remediation rules and policies, `POST /api/remediation/classify` and `GET /api/health/ready`.

**Global administrators' actions.** A key acts as its maker narrowed to its reach, and a global grant stays global when the reach is "Everything" (ADR-0101). So a global administrator's key of reach "Everything" and access "Administer" does what they do: mirrors, server update, automatic update settings, users, licence, AI settings, plugins, backups. No new kind of key is needed. A test now proves it, and that a key of one customer made by the same person cannot.

**Areas.** `GET /api/checks/usage` fell in "administration" while ADR-0101 puts checks in "alerts"; it is now "alerts". `sensor-values` and `recommendations` stay in "servers", next to the server's metrics.

**Files outside `/api`.** Agent release files and `install.sh` (`/agent/<version>/<file>`) are public and signed, fetched without a key (ADR-0054). Backups are downloaded from `GET /api/v1/backups/{name}`. The sensor CSV of the web interface is built in the browser from `GET /api/v1/metrics/query_range`, which a script calls itself. Nothing else is downloaded.

## Decision

**What stays out, each with its reason**, in `v1::LEFT_OUT` and `v1::left_out`, answering 404 to a key:

- `auth`, `setup`, `password-link`, `acknowledge-link`: signing in, first-run setup and the one-time links people open in a browser; a key is already signed in, and these hand out sessions.
- `me` (preferences, own password, ADR-0137, ADR-0141): the signed-in person's own settings; a key is not a person.
- `display`: the screen of a display link, signed in by the link's own secret, never by a key.
- `display-links`, `dashboards/{id}/display-links`: a link reads as its maker with all their roles and outlives the key (ADR-0101 amendment). Listing and turning off links were considered for keys: a link's reach is its maker's, not a key's, so a key's reach cannot bound what it would list or affect. People manage links in the web interface; turning off the maker also stops their links.
- `api-keys`: a key never makes, lists or turns off keys.
- `on-call/contact`: where the person's own pages go.
- `POST /api/users` and `POST /api/users/{id}/link`: they hand out a person's credential that outlives the key. Listing users, roles, disabling and removing stay in v1.

**Enforced by tests.** `NOT_YET_DOCUMENTED` and `NOT_YET_DOCUMENTED_UNDER` are removed: every route of the router is documented in the OpenAPI document or left out of v1 with a reason, or the build fails. A second test reads `web/src/lib/api.ts` and `web/src/lib/routes.ts` and fails when a path the web interface calls matches no route, or matches only routes that are neither documented nor left out. A new feature is done only when its routes are in the document.

**What the web interface chains.** Adding a check with its alert (a sensor row, then a threshold), adding a recommended check, and stopping a check (its thresholds, then the row) take two or more calls, as in the web interface. Each call is documented and a script makes the same calls; the defaults the web interface picks for an alert are its own choice, which a script makes for itself. No combined route is added now. The install command shown with an enrollment token is assembled in the browser from `GET /api/v1/settings/agent-install` and the token; `deploy/install.sh` documents its variables.

## Consequences

- A script can do everything a person does in the web interface, except what belongs to the person, listed above with reasons.
- A route added without documentation fails `cargo test`; so does a web call to a path the router does not serve.
- A key of reach "Everything" and access "Administer" is as strong as a global administrator; the API keys page already says what each key may do, and its creation is audited (ADR-0101).
