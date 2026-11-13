# ADR-0122: Revoking a relay revokes the certificates obtained through it

Date: 2026-11-12
Status: accepted

## Context

`CertificateService.RenewForwarded` (ADR-0028) needs only the peer's certificate, which is public and seen at every handshake to the relay, and a CSR the relay chooses. Nothing proves the peer asked. A compromised relay can therefore obtain a certificate, with a key it holds, for every agent of its site and for its child relays, including a child relay's server names, once per hour per peer.

Revoking the relay (ADR-0029) revoked only the relay's own certificates. The certificates it obtained stayed valid. Nothing ties an agent to its relay, so the attacker could connect to the server directly as each of those agents and renew there, indefinitely. ADR-0028 said "Revoking the relay stops it", which the code did not do.

## Decision

**Each renewal records where it came from.** `certificates` gains three nullable columns, filled in the same transaction as the certificate:

- `renewed_from`: the serial the peer presented to renew;
- `relay_path`: the relays a forwarded renewal went through, from the one connected to the server down to the one the peer is attached to (the whole checked path of ADR-0028, not only `via_relay`);
- `relay_serial`: the certificate the relay connected to the server presented.

Enrollments leave all three NULL; direct renewals set only `renewed_from`. The migration fills `renewed_from` and a one-relay `relay_path` for older renewals from their `peer.renew` audit rows, reading them only.

**Revoking a relay revokes everything that descends from it.** In the transaction of `POST /api/relays/{id}/revoke`, after the relay's own certificates, a recursive query marks revoked every unexpired, unrevoked certificate of another peer that is:

- renewed with the relay anywhere on `relay_path`, or through a connection presenting one of the relay's certificates, then, transitively,
- renewed from one of these (`renewed_from`), or through a relay connection presenting one of these (`relay_serial`).

The second step covers what the attacker could do with the first certificates: renew them directly at the server, and use a child relay's certificate to forward renewals for that child's agents and children. Every serial goes through `CrlPublisher::revoke` before the commit, with the relay's reason, like the relay's own; HA instances follow `certificates.revoked_at` as before. The response and the `relay.revoke` audit row list them under `issued_through` (serial, role, peer ID), and the streams of connections presenting one of them close.

Certificates the relay could not have influenced stay valid: enrollment certificates, and the lines of certificates renewed directly or through other relays from them.

## Operational impact

The relay chose the keys of the certificates it forwarded, so the server cannot tell the peers' legitimate certificates from forged ones. Revoking a relay therefore cuts off every agent and child relay whose certificate line went through it at least once, even if they renewed through another relay since:

- their streams close and their handshakes fail once the CRL reaches the relays;
- they must enroll again with a token of the same site (and, for a child relay, the same parent). Their inventory rows are not revoked, so enrollment keeps their history (ADR-0062);
- `issued_through` in the response and the audit row lists them.

The cost is the same for a healthy relay being retired: moving its agents to a replacement relay first does not help, because their new certificates descend from ones renewed through the old relay. The follow-up below would remove that cost.

## Follow-up: proof of possession for forwarded renewals

A forwarded renewal could carry the peer's signature, with its current key, of the CSR and a server nonce. The server would then know the peer asked, and could record the certificate as not forged: revoking the relay would leave it valid. This needs an additive protocol change (a nonce RPC or field, a signature field in `RenewRequest` and `ForwardedRenewRequest`) and a compatibility window for older agents, whose forwarded renewals would keep today's treatment. Not done in this change.

## Alternatives rejected

- **Revoking every certificate of every peer of the relay's site.** Revokes agents that never used the relay, and misses child relays of other sites.
- **Only the relay's direct descendants, without the transitive step.** A minted agent certificate renewed once at the server, or a minted child relay certificate used to renew that child's agents, would survive.
- **Following `via_relay` in the audit log at revocation time.** It names only the attached relay, not the whole path, and the revocation would depend on parsing JSON details.
