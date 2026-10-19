# ADR-0007: Enrollment bootstrap

Date: 2026-10-18
Status: accepted

## Context

Every link is mTLS (ADR-0001), but a new agent or relay has no client certificate yet: it cannot open the `AgentService` stream to ask for one. It must still authenticate the server before it sends anything secret, without trust on first use, and the server must hand out a certificate only to a peer an administrator approved. The identity in that certificate decides everything afterwards (ADR-0004), so it must not come from the peer.

## Decision

**A separate enrollment listener.** The server runs `EnrollService` (`proto/monitorflow/v1/enroll.proto`, one unary `Enroll` RPC) on its own address, `enroll_listen_addr` (default port 7445). It uses TLS 1.3 with ALPN `h2` and server authentication only: no client certificate is requested. Nothing else is served there. The main gRPC listener keeps requiring a client certificate; there is no plaintext listener.

**One-time tokens that carry the CA pin.** An administrator creates a token for one role (`agent` or `relay`), one peer ID and one site, valid for a limited time (default 1 hour, at most 7 days). The token string is

```
mfe1.<secret>.<ca>
```

where `<secret>` is 32 random bytes from `ring::rand::SystemRandom` and `<ca>` is the SHA-256 of the DER CA certificate, both in unpadded base64url. Only `SHA-256(secret)` is stored, in `enrollment_tokens`. The token is printed once when created and never logged.

**The client pins the CA.** The enrolling peer connects without system roots. The server presents its certificate followed by the CA certificate. The client accepts the handshake only if one of the presented certificates has the pinned fingerprint and the server certificate verifies against it as the only trust anchor, with the usual name and validity checks. The `ca_cert_pem` in the response must have the same fingerprint before the peer stores it. A stolen token is enough to enroll as its peer ID until it is used or expires, but nobody can impersonate the server to a peer holding a token.

**The server's own certificate comes from the built-in CA.** The CA under `<data_dir>/ca/` is the only trust anchor. At start the server checks `tls.cert`: if it is missing, it issues a `monitorflow://server/<server_id>` certificate for `server_names` and writes `tls.cert`, `tls.key` and `tls.ca_cert`; if the server issued it and it is close to expiry, it issues a new one; if another CA issued it, the server refuses to start rather than overwrite a file it did not create.

**Identity is imposed by the token.** The request holds the token and a PEM CSR, nothing else. The CA ignores everything in the CSR except its public key and signature, and writes `monitorflow://<role>/<id>` from the token row.

**Single use, atomically.** One transaction consumes the token with `UPDATE … SET used_at = now() WHERE token_hash = $1 AND used_at IS NULL AND expires_at > now() RETURNING …`, issues the certificate, records it in `certificates`, writes an `audit_log` row and commits. Concurrent requests with the same token serialise on the row lock and the losers see `used_at` set; the existing trigger rejects any later update of a used row. If anything fails, the transaction rolls back and the token stays usable.

**No oracle, bounded failures.** The token format and the CSR are checked before the database is touched. Unknown, expired and used tokens, and a token pinned to another CA, all get the same `PERMISSION_DENIED` "enrollment refused". Lookup is by hash: comparing hashes of a 256-bit secret in an index leaks nothing useful through timing, so no constant-time comparison is needed on the server; the fingerprint pin is public. Each source address (IPv6 by /64) gets 10 failed attempts per 10 minutes, then `RESOURCE_EXHAUSTED`. Requests are capped at 16 KiB and the listener accepts at most 64 concurrent handshakes.

**CRL freshness.** The mTLS listener enforces CRL expiry, so a stale CRL locks every client out. The server builds a new CRL (in `spawn_blocking`) and pushes a new `ServerConfig` through the watch channel read by `tls::incoming` after every revocation and every quarter of the CRL validity (6 hours for the default 24 hours), retrying every minute after a failure.

## Consequences

- Operators open a second port, 7445 by default, to agents (or to relays in phase 3).
- Tokens are long (about 92 characters) but self-contained: the agent needs only the token and the enrollment address.
- Tokens can be created from the server host (`monitorflow-server token create`) before any web user exists; such tokens have a NULL `created_by` and the audit log records the actor as `system/cli`.
- A token is bound to one peer ID. Enrolling the same ID again needs a new token; revoking the old certificate is a separate action.
- The server's own certificate is renewed at start only. Renewal while running and agent-driven renewal are follow-up tasks.
- Rate limiting is in memory and per process; it resets on restart and is not shared between servers.

## Amendment 2026-10-19: server certificate paths follow `data_dir`

The server's `tls.ca_cert`, `tls.cert` and `tls.key` used to default to `/etc/monitorflow/tls/`, whatever `data_dir` said, so a server run with `data_dir = "/tmp/x"` and no `[tls]` section failed at start trying to create `/etc/monitorflow/tls`. Each unset path now defaults to `<data_dir>/tls/{ca.pem,cert.pem,key.pem}`, the same rule as the agent. The server writes these files itself from the built-in CA, which already lives under `<data_dir>/ca/`, so its state is in one directory owned by the service user, and `/etc` can stay read-only for it.

Explicit `[tls]` paths and `MONITORFLOW_TLS_*` variables keep working, path by path. No release shipped with the old default and there is no package yet; packaging that wants the files under `/etc` sets the paths explicitly. An install that relied on the old default and moves to this version gets a new server certificate under `data_dir` on first start, issued by the same CA, so enrolled peers keep trusting it.

## Amendment 2026-10-19: relay tokens carry server names

A relay accepts its agents over mTLS, so its certificate needs the DNS names or IP addresses agents connect to and `serverAuth`. A relay token now carries those names (`token create --role relay --server-name …`, `enrollment_tokens.server_names`), and the CA writes them with `serverAuth`. As with the identity, the CSR's names are still ignored. Agent tokens carry none. See ADR-0025.
