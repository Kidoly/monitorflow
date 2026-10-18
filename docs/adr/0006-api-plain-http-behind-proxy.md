# ADR-0006: REST API over plain HTTP behind a local reverse proxy

Date: 2026-10-18
Status: accepted

## Context

Every link is mTLS (ADR-0001). The REST API is the exception by nature: it is
reached by browsers, which do not present client certificates, and it serves
the web interface under the organisation's own domain and public certificate
(often from ACME). Terminating that TLS inside the server would mean ACME,
certificate reloads and HTTP/1.1 handling in our binary for Phase 1.

## Decision

In Phase 1 the server's API listener (`api_listen_addr`) speaks plain HTTP and
defaults to `127.0.0.1:7444`. It is meant to be reached only through a TLS
reverse proxy (Caddy, nginx, Traefik) on the same host or the same container
network. The browser-facing link is HTTPS; only the loopback hop is plain.

Users authenticate with a server-side session (cookie `HttpOnly`,
`SameSite=Strict`, `Secure`). `api_cookie_secure` defaults to true and is only
turned off for local development without a proxy. The agent, relay and
server links are unchanged: mTLS only.

## Consequences

- Deployments need a reverse proxy; an example configuration in `deploy/` is a follow-up.
- Binding `api_listen_addr` to a non-loopback address exposes plain HTTP;
  the server logs a warning when it does.
- Client IPs seen by the API are the proxy's. Trusting `X-Forwarded-For` needs
  an explicit list of trusted proxies, left for later.
- Native TLS on the API listener can be added later without changing the API.
