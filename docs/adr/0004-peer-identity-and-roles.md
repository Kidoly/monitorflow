# ADR-0004: Peer identity and roles in certificates

Date: 2026-10-18
Status: accepted

## Context

Every link is mTLS (ADR-0001), so each connection carries a client certificate verified against the MonitorFlow CA. Handlers need to know who is calling. A first version used the subject Common Name as the ID. A review found two problems with that:

- Agents, relays and the server shared one ID namespace with nothing in the certificate saying which kind of component it was. A compromised agent could present its valid certificate to an endpoint meant for relays, and nothing would tell it apart.
- The architecture said both "relays forward envelopes transparently" and "a peer's `Hello.agent_id` must match its certificate ID". Behind a relay, the server sees the relay's certificate, so either every relayed agent is rejected or relays may claim any agent, which reopens impersonation.

## Decision

Each certificate carries exactly one URI SAN `monitorflow://<role>/<id>`:

- role is `agent`, `relay` or `server`;
- the ID is 1 to 64 characters from `[A-Za-z0-9._-]` (`common::tls::validate_id`);
- the CN is informational and never used to decide identity; other URI SANs are ignored;
- a certificate with zero or several `monitorflow://` SANs, an unknown role or an invalid ID is rejected.

`common::tls::peer_identity` returns `PeerIdentity { role, id }` from the verified certificate.

Who may speak for an agent:

- a direct `agent` peer: `Hello.agent_id` must equal its certificate ID;
- a `relay` peer may forward envelopes for other agents. The central server accepts an `agent_id` from a relay only if that agent is assigned to the relay's site in the database (implemented with the server stream, not in the TLS layer).

## Consequences

- A compromised relay can forge upstream data (metrics, logs, command results) only for agents of its own site. It cannot inject commands downstream, because commands are signed by the central server and verified by agents.
- A compromised agent can only speak as itself, and endpoints can refuse roles they do not serve.
- The CA must write the URI SAN when issuing certificates, and the database must record each agent's site and each relay's site.
- Certificates from other tools need the URI SAN to work; `scripts/dev-certs.sh` adds it for local runs.
