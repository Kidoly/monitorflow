# ADR-0010: Certificate renewal driven by the peer

Date: 2026-10-19
Status: accepted

## Context

Peer certificates are valid for 7 days by default (ADR-0007). An agent loads its certificate once and, until now, only logged a warning when two thirds of its validity had passed; at expiry it was locked out and needed a new token. A running agent must get a new certificate before expiry, with no token and no downtime beyond one reconnect. The identity in the certificate decides everything (ADR-0004), so renewal must not let a peer change it, and a stolen key must not turn into unlimited certificates.

## Decision

**A unary `Renew` RPC on the mTLS listener.** `CertificateService` (`proto/monitorflow/v1/certificate.proto`) has one RPC, `Renew(RenewRequest { csr_pem })`, served next to `AgentService` on the main gRPC listener. The current client certificate authenticates the call: the TLS handshake already checked the chain, its expiry and the CRL. The response has the same fields as `EnrollResponse`.

**Identity comes from the presented certificate.** The server reads `monitorflow://<role>/<id>` and the serial from the connection's leaf certificate and signs the CSR's public key for that identity. Everything else in the CSR is ignored, as at enrollment.

**Checks, in one transaction.** The server refuses with `PERMISSION_DENIED` and the reason when:

- the role is `server` (the server renews its own certificate at start, ADR-0007);
- the presented certificate has expired, or is revoked in the CA's list: a long HTTP/2 connection can outlive both checks of its handshake;
- its serial is not in `certificates`, is recorded for another peer, or is revoked or expired there;
- the peer's inventory row (`servers` or `relays`) is revoked, or a relay has no row. The row is read `FOR SHARE`, so a revocation waits for the renewal to commit or sees it. An agent that never said Hello has no row yet; its recorded certificate proves the enrollment.

The caller is authenticated, so the reason is safe to return and helps whoever reads the peer's logs. Then the CA issues the certificate (in `spawn_blocking`), and its `certificates` row and a `peer.renew` audit row (new serial, previous serial, `not_after`) commit together.

**Rate limit per peer.** If the latest certificate of the peer, enrollment included, was issued less than one hour ago, the server answers `RESOURCE_EXHAUSTED`. A transaction-scoped advisory lock on the peer URI serialises concurrent calls, so they cannot both pass the check. A thief with a copy of the key gets at most one certificate an hour, each of which shows in the audit log; revoking the inventory row stops renewal for good. A new index on `certificates (peer_role, peer_id, issued_at)` serves the lookup.

**The old certificate stays valid until it expires.** The server does not revoke it:

- the response can be lost, or the peer can fail to write its files (full disk, crash); revoking at issue time would then lock out a healthy peer that needs a new token;
- the agent keeps its open stream on the old certificate until the new files are stored, then reconnects;
- every renewal would add a CRL entry: about 200 a day for 1,000 agents, all sent with every handshake config, for a certificate that expires within days anyway.

Revoking a compromised certificate stays an explicit administrator action.

**Agent side.** A renewal task checks `AgentIdentity::renewal_due` (two thirds of the validity) at start, then every hour with up to 5 minutes of jitter, or sooner if renewal falls due before then. When due it generates a new P-256 key and CSR, calls `Renew` on `upstream_url` with its current identity, and checks the response before writing anything: the CA must be the stored one (same SHA-256), the certificate must hold the new key, and its URI must equal the current identity. The new key and certificate are written as `key.pem.next` and then `cert.pem.next`, each atomically, and then renamed over the live files. `cert.pem.next` marks the commit point: when the identity is loaded, a `cert.pem.next` that is present finishes the renames, and a `key.pem.next` alone is deleted. A crash at any step leaves a matching key and certificate. The stream then closes itself and reconnects with the new identity at once, without backoff. Failures are retried with backoff from 30 s up to 30 minutes, logged as warnings, and as errors once less than a day is left. After expiry renewal is impossible: the agent logs the re-enrollment steps every hour, and the stream keeps the "Rejected" retry policy of ADR-0009.

## Alternatives rejected

- **A `RenewRequest` envelope on the stream.** Relays forward envelopes without interpreting them (ADR-0001), and on a relayed stream the server sees the relay's certificate, so it could not prove which agent holds the key. The stream also has no request/response correlation, deadlines or status codes, and renewal would depend on the stream being accepted: a refused `Hello` would also block renewal.
- **Renewal on the enrollment listener**, with the old key signing a challenge. That listener has no client authentication on purpose (ADR-0007); proof of possession would be a hand-made protocol next to mTLS, which already provides it.
- **Server-issued tokens for re-enrollment.** Tokens are an administrator's approval by design; issuing them automatically would make that approval meaningless.
- **Allowing only the newest certificate of a peer to renew.** It would end a thief's access at the stolen certificate's expiry if the agent renews first. But a lost response would leave the agent with a certificate that can never renew again and a manual re-enrollment days later. The rate limit and the audit trail bound the risk instead.
- **Keeping the same key.** Renewal is the chance to rotate the key; a fresh key costs nothing.

## Consequences

- An agent renews about 2.3 days before expiry with the default 7 days, leaving many hourly retries for a server outage.
- An agent behind a relay cannot renew until relays serve `CertificateService` for their agents, forwarding upstream under the relay policy of ADR-0004. That belongs to the relay tasks in phase 3. The server already accepts `Renew` from relays for their own certificates.
- Certificates from `scripts/dev-certs.sh` are not in `certificates` and cannot be renewed.
- `certificates` grows by one row per peer every few days. Pruning expired rows is a later task.
- The interval is not configurable yet; `Renewer::with_min_interval` exists for tests.
