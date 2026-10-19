# ADR-0029: Relay, enrollment and revocation administration

Date: 2026-10-22
Status: accepted

## Context

Relays (ADR-0025), enrollment tokens (ADR-0007) and revocation (architecture §4) could only be handled from the server host: `token create` on the CLI, revocation not at all outside tests. Architecture §9 lists an administration section in the web interface (agents, relays, tokens, certificates), and MSP installs need it per customer (ADR-0026). The API must stay the only place that decides who may do what, the token string must be returned once and never again, and revocation must go through the CA code the CRL publisher already uses rather than a second copy of it.

## Decision

**Who administers.** Enrolling and revoking peers need an administrator role on the **customer** of the site, global or by customer grant, which is the right that creates sites (ADR-0026). A site administrator grant is not enough: a new identity on a site, or cutting one off, is a decision for whoever owns the customer. `access.rs` gains `administered_sites()` (sites of global and customer administrator grants) and `authorize_admin(user, site)`: an unreadable site is 404, a readable one without that right 403, as with `authorize`. Every route has a case in the tenancy test.

**Relays.** `GET /api/relays` lists the relays of readable sites, any role, sorted by site name and peer ID with the keyset pages of `/api/servers`. An item has the row (`id`, `peer_id`, `name`, site, customer, `created_at`, `revoked_at`), the live state of the relay's own stream from the registry (`status`, `connected_at`, `last_heartbeat`, else the stored `last_seen_at`), `cert_not_after` (latest unrevoked certificate), `server_names`, `server_count` (servers whose `relay_id` is the relay: their latest connection came through it, ADR-0008) and `connected_agents` (registry entries with `via_relay` = the relay now). `server_names` are those of the relay's latest consumed token; renewal copies the presented certificate's names (ADR-0025), so they are the certificate's. `GET /api/relays/{id}` adds `certificate` as for servers and `servers`, the attached servers with their live status through this relay. `GET /api/sites` items gain `relay_count`.

**Tokens.** `POST /api/tokens` takes `{"role", "peer_id", "site_id", "ttl_s"?, "server_names"?}` and calls `enroll::token::create`, the CLI's function, so validation (peer ID, TTL 60 s to 7 days, names for relays only, at most 16), storage of the hash only and the `enrollment_token.create` audit row are shared. The 201 answer is the stored row plus `token`, `upstream_url` and `enroll_url`, so the interface can show the exact peer configuration. The URLs are built from the first of the server's `server_names` and its listener ports; an agent behind a relay sets `upstream_url` to the relay instead. The response type has no `Debug`, and nothing else returns the token: `GET /api/tokens` lists rows (newest first, keyset pages, optional `state` of `pending`, `used`, `expired`) without secret or hash.

**Revoking a token** is `DELETE /api/tokens/{id}` on an unused token, expired or not; a used one is 409. The row is deleted rather than marked: enrollment's consuming `UPDATE … WHERE used_at IS NULL AND expires_at > now()` then cannot find it, with no change to the enrollment code or a migration, and a concurrent enrollment either commits first (the delete then finds `used_at` set: 409) or finds nothing. The `enrollment_token.revoke` audit row keeps the record, with the token ID.

**Revoking a peer.** `POST /api/servers/{id}/revoke` and `POST /api/relays/{id}/revoke` take `{"reason"?}`, one of the CA's RFC 5280 reasons (`unspecified` by default, `key_compromise`, `affiliation_changed`, `superseded`, `cessation_of_operation`). In one transaction the handler sets the row's `revoked_at`, which refuses streams and renewals (ADR-0008, ADR-0010), sets `revoked_at` and `revocation_reason` on every unexpired, unrevoked certificate of the peer, writes `server.revoke` or `relay.revoke` with the reason and serials, and before committing passes each serial to `CrlPublisher::revoke`: the CA records it and the listener and `RevocationService` get a new CRL at once. If the CA fails, the transaction rolls back; if the commit fails after the CA succeeded, the certificate is revoked anyway, which is the safe side, and a retry finishes the job, as CA revocation is idempotent. A revoked row is 409. There is no un-revocation; a revoked peer re-enrolls under a new peer ID (ADR-0008).

**Live streams close on revocation.** The registry gains `close(peer_id, CloseReason::Revoked)`, and the stream ends with `PERMISSION_DENIED` "revoked", which agents treat as a rejection (ADR-0009). Revoking a relay also closes the agent streams it forwards (`via_relay`): their upstream identity is the relay's certificate. This is the only change to the gRPC code.

**Sites.** Creation stays `POST /api/customers/{id}/sites`. Renaming is not added: the site name is the `site` label of every stored series (ADR-0013) and the metric proxy filters on it (ADR-0018), so a rename would hide a site's history. Moving a site between customers stays out (ADR-0026).

**Web.** An Administration entry shows in the header for users with an administrator grant of any kind (`/api/auth/me`); the API still decides. Pages follow ADR-0020's query-parameter routing: `/admin/relays/`, `/admin/relays/view/?id=`, `/admin/sites/` (sites grouped by customer, with server and relay counts and a create form per administered customer) and `/admin/tokens/` (create form, pending and past tokens, revoke). A created token lives only in the component's state, with a copy button and the agent or relay configuration snippet; leaving the page loses it. Revoking a server or relay asks for confirmation in the page, with the reason.

## Consequences

- Customer administrators can run their own enrollments and revocations without shell access to the server; site administrators cannot.
- Token rows of revoked tokens disappear from the list; the audit log is the history.
- The snippet's URLs are right only if the first `server_names` entry is the name peers use; installs behind NAT need their own value, a `public_url` setting being the follow-up.
- Revocation cannot be undone, by design for now.
- Renaming sites needs a stable site ID label on series first.
