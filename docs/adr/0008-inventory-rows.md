# ADR-0008: Inventory rows of enrolled peers

Date: 2026-10-18
Status: accepted

## Context

Enrollment (ADR-0007) issues a certificate but wrote no `servers` or `relays` row, so `GET /api/servers` stayed empty and the relay policy (ADR-0004) denied every relay. The two facts a row needs arrive at different times: the site is known at enrollment, from the token; hostname and OS only at the agent's first `Hello`. `servers.hostname` and `servers.os_family` are NOT NULL, and `relays` needs neither. The row also decides the peer's site, so a peer must never be able to choose or change it, nor write another peer's row.

## Decision

**Agents: the row is created at the first `Hello`.** The site is the one of the latest enrollment token consumed for that peer ID (`enrollment_tokens.peer_id`, `role = 'agent'`, `used_at` set), which only an administrator chose. The Hello only provides hostname, `os_family`, `os_version` and `agent_version`. Later Hellos update those fields, `relay_id` and `last_seen_at`, never `site_id`. A pending row at enrollment was rejected: it would need nullable hostname and OS columns and an API filter for half-created servers.

**Relays: the row is created at enrollment,** in the token transaction, in the token's site, named after its peer ID. A relay has no OS fields to wait for, and the row must exist before any agent can be relayed.

**Nothing moves a peer.** Enrollment refuses (the generic "enrollment refused", nothing committed) when the peer already has a row that is revoked or in another site than the token's. Moving a peer between sites will be an explicit administrator action.

**Who writes which row.** The row written is the one named by `Hello.agent_id`, which the stream already checked: equal to the certificate ID for an agent, or allowed by the relay policy for a relay. The relay policy compares the relay's site with the agent's row, or with its enrollment token's site when the agent has no row yet, so an agent can make its first Hello through a relay of its own site only. `servers.relay_id` follows the latest connection: the relay's row when relayed, NULL when direct; the composite foreign key keeps it in the agent's site.

**Validation.** An agent's Hello needs `os_family` `debian` or `rhel` and a hostname of 1 to 253 bytes; otherwise the stream is refused with `INVALID_ARGUMENT`. A relay's own Hello carries no OS.

**Revocation and unknown peers.** A revoked row refuses the stream with `PERMISSION_DENIED` even if the certificate is still valid. So does a peer with no row and no consumed token ("not enrolled"), such as a certificate made outside the CA.

**Database failures.** At Hello the row is written before the stream is registered; a database error refuses the stream with `UNAVAILABLE` and the peer retries. After that, `last_seen_at` is written at most once per `last_seen_interval` (60 s) per stream, on any accepted envelope, in a separate task: a failure is logged and the stream continues. The live status comes from the registry, so a missed write only makes the stored `last_seen_at` older.

**Audit.** Creating a server row writes `server.create`, creating a relay row writes `relay.create`, in the same transaction, with the connecting peer as actor.

## Consequences

- `GET /api/servers` lists every agent that has said Hello once. An enrolled agent that never connected is not listed yet.
- An agent must send `os_family`; agents built before that field existed are refused until they are updated.
- Certificates made with `scripts/dev-certs.sh` need matching rows (a consumed token for agents, a relay row for relays) to open a stream against a server with a database.
- A revocation applies at the next connection. Closing live streams when a row is revoked belongs to the revocation feature.
- Re-enrolling a revoked peer needs a new peer ID until un-revocation exists.
