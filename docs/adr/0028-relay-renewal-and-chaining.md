# ADR-0028: Renewal through relays

Date: 2026-10-19
Status: accepted

## Context

A relay terminates its agents' mTLS (ADR-0025), so an agent behind a relay sends `CertificateService::Renew` to the relay, which served only `AgentService` and answered `UNIMPLEMENTED`. Agents behind a relay could not renew, and every one of them would be locked out at expiry (7 days by default, ADR-0010).

Renewal must be authenticated as the peer itself (architecture §3), which is why it is a unary RPC and not an envelope. Behind a relay the server never sees the agent's certificate, only the relay's. The relay is the only party that verified the agent's key, against the CA and the CRL.

## Decision

**The relay serves `CertificateService` on its listener and forwards upstream.** An agent's `Renew` goes, unchanged in content, to the upstream as a new unary RPC, `CertificateService.RenewForwarded(ForwardedRenewRequest { csr_pem, peer_cert_der })`, on a connection authenticated with the relay's certificate. `peer_cert_der` is the leaf certificate the agent presented on its connection to the relay, read from the TLS session, never from the request: the agent's `RenewRequest` has no field saying who it is. Agents need no change; they call `Renew` on `upstream_url` as before.

**The server checks the forwarded certificate, then the relay, then the agent:**

- the caller's certificate must have role `relay`; any other role gets `PERMISSION_DENIED`;
- `peer_cert_der` must be a client certificate signed by the CA and valid now (`CertificateAuthority::verify_issued`, the same webpki check as the listener). A relay cannot make up a certificate, serial or name; it can only pass on one it was shown;
- the relay policy of ADR-0004 must allow the relay to speak for that agent (`db::relay_may_speak_for`: same site, neither revoked). A relay cannot renew for an agent of another site, and cannot renew for another relay (or the server) at all;
- then every check of ADR-0010 applies to the agent's certificate as if the agent had called directly: serial recorded for that peer and not revoked or expired, not in the CA's list (even if the relay's copy of the CRL is older), inventory row not revoked, and at most one certificate per hour per agent, under the same advisory lock. The limit is per agent, not per relay.

The identity of the new certificate is the forwarded certificate's, as for a direct renewal; the CSR only provides the key. The `peer.renew` audit row records `via_relay`.

**Statuses go down unchanged.** The agent gets the upstream's code and message, as for streams (ADR-0025), so `RESOURCE_EXHAUSTED` and `PERMISSION_DENIED` keep their meaning in its retry logic. A failure of the relay's own link becomes `UNAVAILABLE` "upstream unavailable".

**The relay does not decide.** It refuses only what it cannot forward: callers whose role is not `agent` (until relay chaining) and `RenewForwarded` from below (likewise). It keeps no state and opens one upstream connection per call; renewals are rare (one per agent every few days).

## Alternatives rejected

- **A token-like proof for the agent (a challenge signed with its key) passed through the relay.** The relay already verified the key in the TLS handshake; a second proof-of-possession protocol next to mTLS is what ADR-0010 rejected for the enrollment listener.
- **TCP passthrough of the agent's renewal to the server.** The agent verifies its upstream's name (`upstream_url` points at the relay), so the server's certificate would not match. And the relay would have to tell apart, without terminating TLS, a renewal from a stream it must forward.
- **The relay renewing its agents itself with a delegated sub-CA.** It would give every relay signing power for every peer ID, which no policy could bound.

## Consequences

- Agents behind a relay renew like direct agents, with no configuration change.
- A compromised relay can obtain certificates, one per hour per agent and each in the audit log, for agents of its own site whose current certificate it holds. It could already speak for them on streams (ADR-0004); with such a certificate it can also connect as them directly. Revoking the relay stops it.
- The relay's `serve` takes the renewal proxy next to the stream forwarder.
