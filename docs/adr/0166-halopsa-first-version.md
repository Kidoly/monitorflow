# ADR-0166: HaloPSA, what the first version does

Date: 2026-11-22
Status: proposed; narrows ADR-0164 for its first version; its "later" list is done in ADR-0191

## Context

ADR-0164 designs the PSA connection as a whole: tickets kept in step both ways, customers and sites matched, servers synced as assets. The owner asked for a first version to be delivered in a fixed order (the connection and its test, then sites matched to Halo clients, then a ticket per incident closed at resolution) and confirmed on a real instance the facts it rests on: OAuth2 client credentials on `{base}/auth/token` with the scope `all`, the API under `{base}/api` with a Bearer token, a ticket created by posting a JSON array, and a category (`category_1`) that some instances require.

Some parts of ADR-0164 do not fit that order, or need a trial instance to settle. This ADR records what the first version does instead, so the later work starts from a written state.

## Decision

**Scope `all`.** The token is requested with the scope `all`, the scope an administrator grants when creating the Halo application in client credentials mode, as the guide says. Narrower scopes, as ADR-0164 wants, come once each feature's needs are checked on a trial instance.

**Sites, not customers.** Each MonitorFlow site is matched to a Halo client and, optionally, to one of that client's sites, chosen from lists read from Halo with a search. Matching customers, name suggestions and a default client for unmatched sites come later. The links have their own table, `psa_site_links`, one row per site, rather than ADR-0164's generic `psa_links`: a site's link holds two Halo ids, and the other kinds of link do not exist yet.

**Ticket defaults.** The connection holds a ticket type and a category, chosen from Halo's lists, since some instances refuse a ticket without a category. The priority map, the requester and the team are left to Halo's defaults for now.

**Tickets.** On `incident.opened`, for incidents at the connection's minimum severity (critical by default) on a matched site, one ticket is created. On `incident.resolved`, a private note is added and the ticket moves to the connection's "resolved" status, Closed (id 9) by default, which an administrator can clear to add the note only. The sync reads `integration_events` with its own delivery state, as ADR-0164 says, and never creates a second ticket for an incident that has one.

**Later, and written down as such** in `docs/halopsa.md`: the status kept in step from Halo to MonitorFlow (the poller), assets, Halo's rate limits beyond backing off on a 429, silences holding tickets, customer matching, the priority map, and checking on a trial instance how a ticket created before a lost answer is found again.

## Consequences

- An MSP can get tickets for incidents with a few minutes of setup, before the rest of ADR-0164 exists.
- A token with the scope `all` can do more than MonitorFlow needs; the guide says to give the Halo application's agent only the permissions it needs.
- Each later part amends this state rather than ADR-0164's full design, which stays the target.
