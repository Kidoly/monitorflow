# ADR-0164: PSA connection, HaloPSA first

Date: 2026-11-21
Status: proposed; first version in ADR-0166, the rest in ADR-0191

## Context

The roadmap asks for a PSA connection, HaloPSA first: a ticket per incident with its status kept in step both ways, customers and sites matched to PSA clients and sites, servers synced as assets.

What exists today does not cover it. An integration (ADR-0102) is a URL that receives a signed JSON request per event: a PSA can be told about an incident, but MonitorFlow never learns the ticket's number or status, cannot avoid a second ticket when an event is delivered twice, and knows nothing of the PSA's clients. A PSA needs MonitorFlow to call *its* API, with credentials MonitorFlow keeps, and to read back.

A spike against a production HaloPSA (read-only apart from one ticket created and deleted in the same run) established how its API behaves. The parts that shape this design:

- **Auth.** OAuth2 client credentials: `POST {base}/auth/token` (form: `grant_type=client_credentials`, `client_id`, `client_secret`, `scope`), a Bearer token valid one hour; the API is under `{base}/api`. Hosted tenants also take `?tenant=<name>` on the token request. A Halo application acts as one Halo agent (`GET /api/Agent/me`), and tickets it creates can be assigned to that agent by the ticket type's defaults.
- **Clients and sites.** A Halo site belongs to exactly one client, like ours to one customer. Each site has a "General User" without an email address, the usual requester for machine tickets. Halo groups clients under "top levels", which MonitorFlow has no use for.
- **Tickets.** `POST /api/Tickets` takes an array and answers 201 with the created ticket. What is mandatory is instance configuration: on the instance tested, `category_1` was, and a missing one gave a 400 with a localized plain-text message, not JSON. `priority_id` is the priority's number *within the ticket's SLA*, not the GUID that `/api/Priority` lists as `id`. Every ticket status has the same `type`, so which statuses mean "closed" cannot be read from Halo; only the built-in ids are stable (1 New, 9 Closed).
- **Assets.** An asset belongs to a client and a site and has an asset type, whose key fields (serial number, name, model...) are chosen per type and per instance. `third_party_id` is an integer. `/api/ExternalLink` is Halo's own table linking a Halo record to a third-party id (string) and type, shown on tickets and assets.
- **Webhooks.** Halo can call a URL on events, with several authentication methods.

## Decision

### One PSA connection per installation, made by a global administrator

A table `psa_connections` holds at most one row in v1 (a unique index on a constant), with the kind (`halopsa`), the base URL, the optional tenant, the client ID, the client secret, the defaults below and its state (enabled, last success, last error). The client secret is sealed at rest (ADR-0125, column `psa_connections.client_secret`), never returned by the API, and kept when an update leaves it out, as channel secrets are.

Only a global administrator manages it: one PSA serves every customer of an MSP, and its credentials reach all of them. The URL may name a private address, since the person is a global administrator (ADR-0163); requests follow no redirects, have a timeout and a cap on answers, and never log the token, the secret, or ticket text.

The token is requested with the narrowest scopes the features in use need (`read:customers read:tickets edit:tickets read:assets edit:assets`, not `all`), cached in memory until a minute before it expires, and never stored. The refresh and ID tokens Halo also returns are ignored: client credentials can always ask again. The guide tells administrators to create a dedicated Halo agent and application for MonitorFlow, so that what MonitorFlow does in Halo is attributed to it and nobody's queue fills with assignments.

**Test connection** is read-only: it gets a token, reads `/api/Agent/me` and the ticket types, and shows the agent's name or Halo's error text. It never creates a ticket. The connection's forms are filled from Halo's own lists, read through the connection: ticket types whose `use` is `tickets`, categories, priorities of the chosen type's SLA, teams, statuses and asset types.

### Customers and sites matched to Halo clients and sites

A table `psa_links` maps a MonitorFlow object to a Halo record: `(connection, kind, local_id, remote_id, remote_name, linked_at)` with `kind` one of `customer`, `site`, `server`, `incident`. One local object has at most one link of a kind, and one Halo record at most one local object. Names are kept only to show what was matched; ids are what is used.

The matching page lists customers and sites beside Halo's clients and sites, with suggestions by normalized name (case, accents, punctuation and common company suffixes ignored) that an administrator confirms one by one or all at once. Nothing is linked without that confirmation. A site can only be linked to a site of the client its customer is linked to. Halo sites that match nothing are not created in MonitorFlow, and MonitorFlow never creates clients or sites in Halo: the PSA is where the MSP keeps its contracts.

For a single company (ADR-0104) the page shows no customer wording: one choice, "the Halo client and site for this installation", stored as the links of the one customer and its sites.

An incident on a site without a link uses the connection's default client and site when one is set; otherwise no ticket is opened, and the incident page and the connection's state say which site needs a link.

### A ticket per incident

The sync reads `integration_events` (ADR-0102) as one more consumer with its own delivery state, so it inherits at-least-once delivery, retries over a day and replay, and is fed from every place incidents come from. Which incidents open tickets is the connection's setting: by default critical ones only, with a choice of warning and critical, and a per-customer and per-site off switch. Silences hold tickets as they hold notifications.

On `incident.opened` the sync creates one ticket:

- **summary** from the incident's title, cut to Halo's limit, **details** with the server, the measure, the values, the time and a link to the incident, all redacted (ADR-0040). The AI's text is untrusted (it never chooses fields) and, when included, goes into a private note.
- **client, site** from the links; **requester** the site's "General User" by default, never a person with an email address unless an administrator picks one; **ticket type, category, team** from the connection's defaults; **priority** from a map of warning and critical to two priorities of the type's SLA. Agent assignment is left to Halo's rules.
- **No duplicates.** Before the request, the sync writes a pending `incident` link. After a 201, it stores the ticket id. After a timeout or an error that does not prove the ticket was refused, it looks for a ticket of that client carrying the incident's marker (an External Link with type `MonitorFlow` and the incident id, or, if Halo refuses those for custom integrations, the id in a user-defined field) before creating again. A replayed event finds the link and does nothing.
- A 4xx from Halo is a configuration error, not a reason to retry: it is stored with Halo's text as the connection's last error and on the incident, and shown on both.

### Status in step both ways

**MonitorFlow to Halo.** `incident.acknowledged` adds a private note naming who acknowledged it. `incident.escalated` adds a note and, when the map says so, raises the priority. `incident.resolved` adds a note and moves the ticket to the connection's "resolved" status (default: none, the note only; Closed for those who want MonitorFlow to close tickets). MonitorFlow never reopens a ticket a person has closed; a new incident opens a new ticket.

**Halo to MonitorFlow.** A poller reads, every minute, the tickets of linked incidents updated since its last read (`GET /api/Tickets` filtered by the update time, paged). A ticket moved to one of the connection's "closed" statuses (default: id 9) while its incident is open acknowledges the incident as "Closed in HaloPSA by <agent>", which stops escalation and repeat notifications; it does not resolve it, since an incident resolves from measurements only (ADR-0017). A ticket assigned to an agent while its incident is not acknowledged acknowledges it the same way. A deleted or merged ticket ends the link, and the incident page says so. Both are audited as actions of the connection.

Polling was chosen over Halo's webhooks for v1: it needs no route reachable from the PSA, works for a MonitorFlow behind a firewall, and costs one request a minute when nothing changed. Halo webhooks to a signed inbound route can come later as an option for speed; they would be a new inbound surface and need their own ADR.

### Servers synced as assets

One way, MonitorFlow to Halo, for servers of linked sites, when the connection's asset sync is on (off by default). The administrator picks a Halo asset type per system family (Linux, Windows, other) and which of that type's fields receive the host name, the serial number, the manufacturer and the model, from the inventory (ADR-0008).

A server is matched to an existing Halo asset of its client by serial number, then by host name in the type's key field; a match is shown for confirmation the first time, as clients are. Without a match the asset is created. The `server` link holds the asset id, and the asset gets an External Link back to the server. Changes of name, inventory or site are pushed on `server.added` and on inventory changes, at most once an hour per server. A removed server is left as it is in Halo by default; marking its asset inactive is an option. MonitorFlow never deletes Halo assets and never reads Halo's assets into servers. A ticket opened for a server's incident is attached to its asset when Halo accepts it.

### Everything through the public API

The connection (create, read without the secret, update, delete, test), Halo's lists read through it, the matching (suggestions, links, unlink), the per-incident ticket link and a "sync now" are under `/api/v1/psa`, in the OpenAPI document, used by the web interface, and audited (`psa.connection.*`, `psa.link.*`, `psa.ticket.*`, with Halo ids, never ticket text or secrets).

### The code

A module `psa` in the server with a `Psa` trait (token, lists, create ticket, add note, set status, tickets updated since, upsert asset) and one implementation, `halopsa`. Request and answer types are tested against sanitized fixtures; a real test runs the whole flow against a Halo trial instance or a local stand-in of its API, never against a customer's or an MSP's production instance. A second PSA is a second implementation of the trait, with the same tables.

## Consequences

- An MSP gets tickets in the tool its technicians already use, without a script between the two, and acknowledging in either place is seen in both.
- MonitorFlow now stores one more outbound secret and makes requests to a PSA on its own; both follow the rules already used for channels and imports.
- What Halo requires is instance configuration; the connection must offer Halo's own lists and show Halo's errors as they are, and the first ticket that fails tells an administrator what is missing.
- Halo's status ids beyond New and Closed are per instance, so "closed" is a setting, not a guess.
- Open questions to settle on a trial instance before implementing the parts that need them: whether `dont_do_rules` and External Links work for a custom integration, how a ticket names its asset at creation, Halo's rate limits (the client backs off on 429), and the length limits of summary and details.
- Work, as stacked PRs: the connection and its test; matching; tickets from incidents; the poller; asset sync; then the web pages, each with its API routes and documentation. The glossary gains "PSA connection" (Connexion PSA) in the first of them.
