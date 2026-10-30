# ADR-0062: Remove a server

Date: 2026-10-31
Status: accepted

## Context

Revoking a server or relay (ADR-0029) is final and leaves it in every list. The ID is then lost: a revoked row refuses enrollment (ADR-0008), and an install key gives the same host `web-01-2` (ADR-0058). Open incidents wait for the offline check, and threshold incidents stay open for good. The uninstall commands live only in the docs. The simplicity audit (M10, journey j) asks for one action that does all of this.

## Decision

**Removed is revoked.** There is one action, "Remove", and no separate state: `revoked_at` means removed, and the `POST /api/servers/{id}/revoke` and `/api/relays/{id}/revoke` routes do it. No migration is needed, and servers revoked before this change leave the lists on upgrade. The interface no longer asks for a reason and sends `cessation_of_operation`. The API still accepts the others, for scripts.

**What removal does.** In one transaction, with the certificates and the CRL as before: delete the peer's unused single-use tokens, so an old token cannot bring it back; resolve its open incidents of every metric with the reason `revoked`; delete its `alert_states`. Each offline check repeats the resolving step, which covers a threshold incident a last queued sample opens just after, and incidents of servers revoked before this change.

**Hidden, not deleted.** `GET /api/servers`, `GET /api/relays`, the counts of `GET /api/sites` and a relay's attached servers leave removed peers out. Metrics, logs, incidents and the audit log keep following their own retention. `GET /api/servers/{id}` and `GET /api/relays/{id}` still answer, which is what incident links need, and those pages show the uninstall command. The offline task no longer loads removed peers, so it can neither open an incident nor say "back online". The threshold engine treats them as gone. Notifications read "Resolved: web-1 was removed", for any metric.

**Uninstall command.** The page builds it, like the install commands. On Linux, `MF_UNINSTALL=1 MF_PURGE=1` through `install.sh` from the default install source, at the public address for `central`, with GitHub as the fallback when that source has no installer; relays add `MF_ROLE=relay`. On Windows, `msiexec /x` with the product code read from the registry and `REMOVE_DATA=1`, since the MSI file may be gone. Both remove the identity, because a kept identity holds a revoked certificate and the installer would keep using it.

**Coming back.** Enrolling a removed peer in its own site brings the same row back, with its ID, history, thresholds and mute settings, and records `server.restore` or `relay.restore`. A single-use token does it for its ID. An install key does it for the latest server removed from the key's site with that host name or ID, ahead of the suffix search of ADR-0058, so a server first enrolled under another ID is found too and no second row appears. `last_seen_at` is set, so no offline incident opens before the first Hello. With `require_approval`, the row stays removed until the approval, and the host's Hello gets "waiting for an administrator", directly or through a relay. A second enrollment while it waits gets a suffixed ID, as for any host in use.

**Other sites.** A removed row stays in its site, because moving it would show its history to another customer. An install key in another site gives a suffixed ID. A token for that ID in another site is refused with 409 at creation, which says where it was removed from, rather than at install time. Removed relays come back with the token's parent. Tokens from the interface name none, so a chained relay returns directly attached. A token naming a different parent is refused as before, since accepting it could create a loop.

## Consequences

- Revocation is no longer final for an ID. After a key compromise, removal still stops the old certificate for good, but anyone holding a valid token or install key for the site can enroll under that host name, as they could enroll a new host. The new certificate has a new key and serial, and the audit log shows `server.restore`.
- This revises ADR-0008 (a revoked row refuses enrollment) and ADR-0058 ("never an existing ID", for removed servers of the key's site only). Two different hosts with the same name in one site count as one: the second takes over the removed row's history.
- A relay whose CRL is not yet refreshed could forward the old certificate during the short time between a re-enrollment and the CRL update, since the row is no longer revoked. The removal must be followed by a re-enrollment within seconds for this to happen.
- A host that was removed but not uninstalled keeps its revoked identity, and the installer does not enroll it again. It says so and points to the uninstall command. Detecting this needs the agent to record the refusal, which is left for later.
- The removed server's page is reached through incident links or its address, since no list shows it.
