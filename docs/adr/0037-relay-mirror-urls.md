# ADR-0037: Relay mirror URLs

Date: 2026-10-25
Status: accepted

## Context

ADR-0034 lets a relay copy the server's mirrors and serve them on its own plain HTTP listener, opt-in with `mirror.enabled`. ADR-0035 writes one base URL into a server's `ConfigureSources` spec, chosen by `commands::sources::base_url`, which returns the server's `mirror_public_url` for every server. Agents behind a mirroring relay should get the relay's URL instead, but the server knows neither the URL the agents' hosts reach the relay at (NAT, DNS, proxies, as for `mirror_public_url`) nor which mirrors and snapshots the relay holds: mirroring may be off, may have failed to start, may be narrowed with `mirror.mirrors`, may not have finished its first sync, and keeps only `mirror.keep_snapshots` snapshots.

Two ways for the server to learn the URL: an administrator sets it on the relay record through the API, or the relay reports it.

## Decision

**The relay reports**, on its own mTLS stream to the server, a new `Envelope` variant `RelayMirrors`: `public_url` and, for each mirror it serves, its family, name, the snapshot `current` names and the complete snapshots it holds (those with a `.manifest`, ADR-0034). The URL is a new relay setting, `mirror.public_url` (http or https, no userinfo, query or fragment, checked like `mirror_public_url`). An API setting was rejected: only the relay knows what it actually serves, and the URL alone cannot tell the server whether a snapshot is there. The URL is still configured by an operator, on the relay host, next to `mirror_listen_addr`, as the server's own is.

- The relay sends the report right after its `Hello` and again whenever it changes: the mirror task scans its tree at start and after each mirror it syncs. A relay with mirroring off, failed, or without `mirror.public_url` sends an empty report, so the server forgets an earlier one. A report the full send queue refuses is retried with the next heartbeat.
- The server accepts the report only on a relay's own stream (role `relay`, its Hello ID); an agent's is ignored. It checks the URL, family and names (mirror and snapshot name rules of ADR-0030), at most 1024 mirrors and 64 snapshots each; an invalid report is logged and clears the relay's mirrors. It stores the URL and report time on `relays` (`mirror_url`, `mirror_reported_at`) and the mirrors in `relay_mirrors`, replaced as a whole in one transaction, unless a report received later is already stored (each report is stored by a task of its own). The relay API shows `mirror_url` and `mirror_reported_at`.
- `base_url(public_url, relay, selected)` returns the relay's URL when the server's current relay (`servers.relay_id`, the relay its latest connection came through, not revoked) holds every selected snapshot: a named snapshot it lists, or a `current` for `current`. Otherwise it returns `mirror_public_url`, as before; with neither, configuration is refused (409). The choice is all or nothing because the spec has one base URL.
- Old servers ignore the new variant (it decodes as an envelope without payload); old relays send none, and their servers keep `mirror_public_url`. No protocol version change.

A chained relay's report reaches the server through its parents like any of its envelopes. Only the relay the agent connects to is considered: an agent behind a chained relay may not reach the parent's listener.

## Consequences

- An operator who enables relay mirroring sets `mirror.public_url` on the relay; nothing is needed on the server, and agents behind the relay get its URL in the next `configure_sources` from that moment on.
- A relay could report any URL, so sources of its agents could point anywhere. This gives it nothing it lacks: it already carries those agents' traffic, and packages are checked against the key the server pins by fingerprint in the spec (ADR-0032). It can only redirect the servers attached to it.
- `current` on the relay may lag the server's by up to a sync interval; a host pointed at the relay's `current` gets the relay's. Campaigns that need a fixed state name a snapshot, which the relay must hold.
- A server's sources are fixed when the command is queued. A relay that later drops a snapshot (retention) or stops mirroring leaves hosts pointing at a URL that answers 404 until a new `configure_sources`; agents' hosts that move to another relay or connect directly keep their old sources likewise.
- The report is the only status the server has of a relay's mirrors; sync errors are still in the relay's logs only.
