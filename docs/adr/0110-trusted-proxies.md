# ADR-0110: Trusted proxies for the client's address

Date: 2026-11-12
Status: accepted

## Context

Sign-in (ADR-0106) and screen links (ADR-0099) are limited per source address. The API speaks plain HTTP behind a reverse proxy (ADR-0006), so the source was taken from the last `X-Forwarded-For` entry, whoever sent it. ADR-0006 left the list of trusted proxies for later.

A security audit (2026-11-12) found that a local user on the central server's host could connect to the API directly: in the compose files it listens on the `front` bridge, which the host reaches, and with the bare Caddyfile it is on `127.0.0.1:7444`. Sending a different `X-Forwarded-For` with each attempt put every attempt in a fresh bucket, so the per-source sign-in limit and the screen limit did not apply. The per-account limit still did. Caddy replaces the header that remote clients send, so they could not do this.

## Decision

**`api_trusted_proxies` lists the proxies whose `X-Forwarded-For` is believed:** IP addresses, CIDR networks or host names. The API is served with the TCP peer's address. When the peer is a trusted proxy, the client is the last entry of `X-Forwarded-For`, the one that proxy appended; when that proxy sends none that parses, all such clients share one bucket, as before. Any other peer counts as its own address.

- **Default: loopback** (`127.0.0.1`, `::1`), which the bare Caddyfile needs.
- **Compose: `web`**, the web service, by name. A connection from the host to a container's bridge address comes from the bridge's gateway, which is in the `front` subnet, so trusting the subnet would trust the host too. Fixing the subnet and the web service's address would clash with networks that exist already on some hosts and would make upgrades recreate the network. Docker's DNS gives the current address instead.
- **Host names are looked up again when a peer matches nothing**, at most once a second. A web container restarted with a new address is trusted again within a second, and a stranger cannot cause more lookups than that.

## Consequences

- In the compose deployments, the host and the other containers can no longer choose the address they are counted as.
- With the bare Caddyfile, every local process connects from loopback like Caddy, and TCP cannot tell them apart. A host with untrusted local users can set `api_trusted_proxies = []`: they are then counted as themselves, and so is every remote client, behind the proxy's one address. A Unix socket between proxy and API would remove this trade-off; it is not done here.
- A deployment with another proxy in front of the API, on another address, must list it, or all its clients share the proxy's address in the limits.
