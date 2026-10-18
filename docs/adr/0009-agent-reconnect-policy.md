# ADR-0009: Agent reconnect policy

Date: 2026-10-19
Status: accepted

## Context

An agent keeps one stream open to its upstream (ADR-0001) and must come back by itself after network cuts, server restarts and heartbeat timeouts. Some failures are not fixed by retrying: a revoked certificate, a certificate the upstream does not trust, a `Hello` it refuses. Exiting on those looks natural, but a supervisor restarting the agent turns that into a tight loop. A server-side mistake, such as a stale CRL (which locks out every client, ADR-0007), would make every agent of the fleet exit at once, and someone would have to restart them all by hand once it is fixed.

## Decision

The agent sorts every end of a stream into one class and never exits on a remote failure:

| Class | Examples | Retry |
| --- | --- | --- |
| Transient | connect or TLS error, server restart, heartbeat timeout, upstream closed the stream | exponential backoff with full jitter, 1 s doubling to a 60 s cap; reset once a connection stayed up for `max(heartbeat_interval, 30 s)` |
| Replaced | `ABORTED`: a newer connection uses the same identity | same backoff, logged as a warning that a copy of the host may be running |
| Rejected | TLS alert about our certificate (bad certificate, unknown CA, expired, …), `PERMISSION_DENIED`, `UNAUTHENTICATED`, `FAILED_PRECONDITION`, `INVALID_ARGUMENT` | every 5 minutes, logged at error level |
| Revoked | TLS alert `certificate_revoked` | every hour, logged at error level with the re-enrollment steps |

Local problems that only an operator can fix exit with an error: unsupported distribution, unreadable or invalid identity files, no token on first run, a refused enrollment token, an enrollment response that does not match the token's CA pin or our key. Transient enrollment failures are retried with the same backoff.

## Consequences

- A fleet recovers by itself, within 5 minutes, from a server-side certificate mistake.
- A revoked agent costs the server one handshake an hour, not a loop.
- The TLS alert is found by downcasting the error chain, falling back to rustls's own alert wording, because h2 hides its inner `io::Error` from `source()`. A tonic or h2 upgrade that changes this is caught by the integration tests in `crates/agent/tests/stream.rs`.
- A supervisor (systemd unit, later) can use plain `Restart=on-failure`: the agent exits only for local errors.
