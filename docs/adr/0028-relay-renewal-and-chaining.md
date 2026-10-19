# ADR-0028: Renewal through relays and relay chaining

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

**The relay does not decide.** It refuses only what it cannot forward: callers with role `server`, and `RenewForwarded` from anything but a relay. It keeps no state and opens one upstream connection per call; renewals are rare (one per agent every few days).

### Relay chaining

A relay's `upstream_url` may point at another relay, its parent. The downstream relay connects to it with its relay certificate like an agent would: its own stream, CRL fetches, renewals and the streams of its agents all go through the parent.

**Each relay has at most one parent, chosen by the administrator.** `relays.parent_relay_id` is NULL for a relay that connects to the server. It comes from the relay's enrollment token (`token create --role relay --parent-relay a`, `enrollment_tokens.parent_relay_id`), like the site, never from the relay. The parent must be enrolled and not revoked when the token is created. Re-enrolling under another parent is refused like re-enrolling in another site (ADR-0008). Links only point to rows that existed before, so they cannot form a cycle. An administrator action that moves a relay must check for cycles.

**Each hop states only what it verified: the `monitorflow-via` metadata.** A relay that forwards a stream or a renewal adds the call's path as gRPC metadata: peer URIs, comma-separated, from the origin to the peer the relay received the call from (`common::via`).

- If the sender is an agent, the path is just that agent. Anything the agent sent is ignored.
- If the sender is a relay, the relay appends the sender to the path the sender gave. A sender relay that gave no path speaks for itself.
- The relay checks that the `Hello` names the path's origin. Everything else is for the server.
- Paths are at most 8 peers long, and only the origin may be an agent.

The envelopes are still forwarded unchanged; the path is per-hop transport metadata, not envelope content.

**The server checks the whole path against the parent links** (`RelayPolicy::may_route`, `db::relay_route_allowed`). The connecting relay R0 is verified by TLS. The path below it lists R1 … Rk and then the origin. The server requires:

- `parent(R1) = R0`, `parent(R2) = R1`, and so on, with none of them revoked;
- the `Hello.agent_id` (or, for a renewal, the forwarded certificate) to be the origin;
- if the origin is an agent, that it belongs to the site of Rk, the relay it is attached to (`relay_may_speak_for`, unchanged);
- if the origin is a relay, that it is a child of Rk.

The registry, `servers.relay_id` and the audit rows record Rk as `via_relay`: the relay the agent is directly attached to, or a chained relay's parent. A call without a path keeps the meaning it had in ADR-0025, so relays of the previous version keep working when connected to the server.

**What each hop can forge.** A compromised relay controls the path below itself, but the server only accepts paths that follow the parent links down from it, so it can claim only peers of its own subtree: agents of the sites of its descendant relays, and those relays. That is the subtree equivalent of ADR-0004's single-site limit. It cannot claim its parent, a sibling or an unrelated relay, because the next hop up appends its real certificate identity. An agent cannot add a path at all. A relay's site and the sites below it are independent: a parent in one customer's site can be given children in another's, and the administrator who chooses the parent decides that trust.

**The CRL goes down the chain.** Relays serve `RevocationService.GetCrl` to downstream relays from the copy their listener enforces, refreshed hourly from their upstream. The CRL is signed by the CA, which webpki checks, so a relay can serve an old one but not a forged one. Its `next_update` (24 h) bounds how old it can get. Each hop adds up to an hour of delay before a revocation reaches the bottom.

**Renewals chain the same way.** The first relay sends `RenewForwarded` with the requesting peer's certificate and the path `[peer]`. Each relay above passes on the request with the sender appended. A downstream relay renews its own certificate with `Renew` on its parent, which forwards it as `RenewForwarded`. The server checks the path as for streams and applies the per-peer limit to the origin. A relay can therefore renew its child relays but no other relay.

### Enrollment through a relay (follow-up)

Not implemented. A peer in a network that only reaches its relay still needs the server's enrollment listener. The intended design is an opaque TCP passthrough: the relay listens on its own enrollment port and splices each connection to its upstream's enrollment address, without terminating TLS. A chained relay targets its parent's passthrough port.

- TLS stays end to end between the peer and the server, so the token and the CA pin of ADR-0007 hold unchanged, and the relay never sees a token.
- The peer verifies the server's certificate. That certificate names the server, not the relay, so the client needs an `enroll_server_name` override for the name it verifies. Without the override, the name check fails.
- The server's per-source failure limit then counts the relay's address for every peer behind it. The passthrough would forward with the PROXY protocol header, or the limit would apply per relay.

A relay-hosted enrollment proxy was considered instead. It would have to hold tokens in clear and present its own certificate under the pin, so a compromised relay could steal tokens.

## Alternatives rejected

- **A token-like proof for the agent (a challenge signed with its key) passed through the relay.** The relay already verified the key in the TLS handshake; a second proof-of-possession protocol next to mTLS is what ADR-0010 rejected for the enrollment listener.
- **TCP passthrough of the agent's renewal to the server.** The agent verifies its upstream's name (`upstream_url` points at the relay), so the server's certificate would not match. And the relay would have to tell apart, without terminating TLS, a renewal from a stream it must forward.
- **The relay renewing its agents itself with a delegated sub-CA.** It would give every relay signing power for every peer ID, which no policy could bound.

## Consequences

- Agents behind a relay renew like direct agents, with no configuration change.
- A compromised relay can obtain certificates, one per hour per agent and each in the audit log, for agents of its own site whose current certificate it holds. It could already speak for them on streams (ADR-0004); with such a certificate it can also connect as them directly. Revoking the relay stops it.
- The relay's `serve` takes `Services`: the stream forwarder, the renewal proxy and the CRL cache.
- A chained relay must be enrolled with `--parent-relay`; a relay of the server keeps none.
- For the admin API (not in this change): relay listings should show `parent_relay_id`, as the parent's peer ID, and token creation should accept a parent relay for relay tokens, with the same check as the CLI.
- The parent stays fixed until an administrator action to move relays exists; that action must refuse cycles.
