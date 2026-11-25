# ADR-0191: HaloPSA, the rest of the connection

Date: 2026-11-25
Status: accepted; completes ADR-0164 after ADR-0166

## Context

ADR-0166 delivered the first part of ADR-0164: the connection and its test, sites matched to HaloPSA clients and sites, and a ticket per incident closed at resolution. It left for later the ticket status read back from HaloPSA, servers synced as assets (on the hook ADR-0187 prepared), customers matched as well as sites, a priority map, silences holding tickets, and HaloPSA's rate limits.

No trial instance was used for this work, and the owner's production instance must not be called. Everything below rests on HaloPSA's public API documentation and on the facts the spike of ADR-0164 recorded, and is tested against an in-process stand-in. Where the public documentation is silent, the choice made is the one that fails safe, and says so.

What the public documentation says that shapes this design:

- **Rate limit:** 700 requests in a rolling 5 minutes. It does not say per what, nor what an excess answers; a 429 with `Retry-After` is the usual form.
- **Tickets:** `GET /api/Tickets` filters by client, agent, asset and a search, but not by update time. `GET /api/Tickets/{id}` gives `status_id`, `agent_id` (1 is unassigned), `deleted` and `merged_into_id` (0 when not merged). `priority_id` is "the ID of the SLA priority". The ticket type gives its `default_sla`. How a ticket names an asset at creation is not documented.
- **Assets:** `GET /api/Asset` filters by client and asset types and returns each asset's `fields` (id, name, value) with `includeassetfields`; there is no search parameter. `POST /api/Asset` takes an array, with `fields: [{id, value}]`, `inventory_number`, `inactive`. Key fields are configured per asset type. `third_party_id` is not documented on assets.
- **External Links** are created by posting `new_external_link` (`integration_name`, `third_party_id`, `third_party_desc`, `third_party_url`) with the record; they cannot be deleted through the API.

## Decision

### Rate limits: one process, one budget

Every request to HaloPSA, the token request included, goes through a limiter that allows the process 600 requests in a rolling 5 minutes, leaving the rest of HaloPSA's 700 to the tenant's other integrations and to pages opened on other central server instances. A request over the budget is not sent: it fails at once with a retryable error saying when to try again, so background loops retry later and pages show it. A 429 pauses every request of the process for its `Retry-After` in seconds (60 when absent or a date, at most 300).

So that one budget covers all background work, the ticket sync, the poller and the asset sync run on the leader only (ADR-0089), instead of on every instance as ADR-0166 had the ticket sync. Delivery claims stay leased, so a change of leader loses nothing. The poller takes at most 30 tickets a minute (150 of the 600) and the asset sync at most 20 servers a minute.

### Status from HaloPSA to MonitorFlow: tickets read one by one

As the documentation offers no "updated since" filter, the poller reads open tickets by id, oldest checked first, 30 a minute: with 30 open tickets or fewer each is read every minute, with more each is read less often, never more than the budget allows. This replaces ADR-0164's "tickets updated since the last read", which needs an undocumented filter.

A ticket moved to one of the connection's closed statuses (default: 9, Closed), or assigned to an agent (when the connection says so, the default), while its incident is open and not acknowledged, acknowledges the incident with `via = 'psa'`, without a user: the name shown is the assigned agent's, "Alice Martin (HaloPSA)", or "HaloPSA", and the note says "Closed in HaloPSA, ticket 5001." or "Assigned in HaloPSA, ticket 5001.". The application's own agent (read by the connection test) and agent 1 are nobody taking the ticket: the ticket type's defaults may assign tickets to the application. The incident stays open: it resolves from measurements only (ADR-0017). A ticket deleted, merged, or answered 404 ends the link (`psa_tickets.state = 'ended'`) with the reason. Both are audited as `psa.ticket.acknowledge` and `psa.ticket.end`, actor `system`, id `psa`, with Halo ids only.

At resolution, a ticket the poller last saw in a closed status gets the note but is not moved: setting another status could reopen a ticket a person closed.

### Customers, and a default client

`psa_customer_links` matches a customer to a client, one client to one customer at most. A site's own match (`psa_site_links`) comes first; a site of a matched customer without one opens tickets for the customer's client without a Halo site; the connection's default client and optional site take sites matched to nothing; otherwise no ticket, and the ticket list says why. A site of a matched customer may only be matched to that client, and a customer may not be matched to a client other than its sites'. ADR-0164's generic `psa_links` table is not used: each kind keeps its own table, as ADR-0166 began.

Suggestions compare names folded without case, accents, punctuation and company forms (SAS, SARL, Ltd, Inc, GmbH...), and are made only when exactly one client and one customer fold the same. They are read on request (the page asks, the list of clients is read once), and nothing is matched until confirmed, one by one or all at once.

A single company sees "the HaloPSA client of this installation": the link of its one customer.

### Priorities

The connection maps warning and critical to a priority of the ticket type's SLA, by the priority number (`priorityid`) that a ticket's `priority_id` names, listed from `/api/Priority` and filtered by the type's `default_sla`. Absent, HaloPSA's default applies. An incident escalated to critical adds a note to its ticket and moves it to the critical priority.

### Silences hold tickets

Before creating a ticket, the sync looks for an active silence covering the incident's event, with the same rules as notifications (`Silence::concerns`, ADR-0079, ADR-0151). If one does, the ticket is `held` and the delivery waits until the silence's end, without counting an attempt; its later events wait behind it. When the silence ends, the ticket is created if the incident is still open; an incident opened and resolved during the silence gets none (`skipped`, with the reason), as it gets no notification.

### Servers as assets

One way, as ADR-0164 says, when the connection's asset sync is on, for servers of matched sites and customers. The administrator chooses an asset type per system family (Linux, Windows; the `other` of ADR-0164 has no servers) and where the host name, serial number, maker and model go: `inventory_number` (the asset tag) or one of the type's fields. The type's fields come from `GET /api/AssetType/{id}?includedetails=true` as the spike saw it, else from the `fields` of one of its assets, which is documented.

A server without a link is matched among the active assets of its client and type: same serial number in the serial's field, else same host name in the key field or the host name's field, case aside, skipping assets of other servers. A match is `proposed` and nothing is sent until an administrator confirms it, chooses another asset of the client, or asks for a new one. Without a match the asset is created, with an External Link to the server's page. What an asset receives is hashed; a linked server is looked at again at most once an hour, and its asset updated only when the hash changed. A removed server's asset is marked inactive once when the connection says so (off by default), otherwise left as it is. MonitorFlow never deletes assets and never reads them into servers. Changing the fields pushes every linked asset again, without a second External Link.

A ticket for a server with a linked asset names it (`assets: [{id}]`); as this is not in the public documentation, a 4xx to that body is followed by the same ticket without it.

### Everything through the public API

New routes under `/api/v1/psa`, documented and audited: priorities; customer links, suggestions and the default client; asset types and fields, asset settings, asset links (confirm, choose, create, forget) and "sync now". Global administrators only, like the rest of the connection.

## Consequences

- Closing or taking a ticket in HaloPSA stops MonitorFlow's reminders within a minute for up to 30 open tickets, later beyond, and never resolves an incident.
- The background work of the connection stays under HaloPSA's limit on its own; pages opened on several instances at once each have their own budget, which the 100 requests of margin are for.
- Moving background PSA work to the leader means one instance does it; it is light (a few requests a minute).
- Several choices rest on what the public documentation does not say and are checked by the code at run time rather than assumed: the asset type's field list, the ticket's asset, the meaning of a 429. A trial instance can confirm them later; nothing here needs one to be safe.
- Still not done: Halo webhooks to an inbound route (ADR-0164 left them as a later option), MonitorFlow's acknowledgement and escalation written as notes beyond the priority change, the requester and the team of tickets, and the ticket shown on the incident page.
