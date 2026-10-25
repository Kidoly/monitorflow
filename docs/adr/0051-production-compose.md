# ADR-0051: Production compose and the server's storage links

Date: 2026-10-25
Status: accepted

## Context

The repository had a compose file for development dependencies only, so
running the central server meant building it and wiring PostgreSQL,
VictoriaMetrics, VictoriaLogs and a reverse proxy by hand. The architecture
allows no plaintext link (§4). The server's PostgreSQL client supports TLS
with a private CA (`sslmode=verify-full`, `sslrootcert`), but its
VictoriaMetrics and VictoriaLogs clients trust public roots only, so they
cannot verify a certificate from a CA of the deployment.

## Decision

`deploy/docker-compose.yml` runs the whole server on one Docker host: the
`server` image (`deploy/docker/server.Dockerfile`), the `web` image (the
static export served by Caddy with `deploy/caddy/Caddyfile`), PostgreSQL 17,
VictoriaMetrics and VictoriaLogs at pinned versions. Settings come from
`deploy/.env` through `MONITORFLOW_*` variables. Only Caddy (443, 80) and the
agent listeners (7443, 7445) are published.

- PostgreSQL: TLS verified by name. A one-shot `db-certs` service creates a
  CA and a certificate for `postgres` on first start, then deletes the CA
  key. `pg_hba.conf` accepts network clients over TLS only.
- VictoriaMetrics and VictoriaLogs: plain HTTP on the `storage` network,
  which is `internal: true`. It has no published port and no route out;
  only the server, PostgreSQL and the two stores are attached to it. This
  is an exception to §4, like the API hop of ADR-0006: the traffic never
  leaves the Docker host.
- The API listener binds `0.0.0.0:7444` inside the `server` container,
  unpublished, reached by Caddy over the `front` network (ADR-0006).

## Consequences

- One command deploys a server; `deploy/smoke-test.sh` checks it, the TLS
  database link included, and runs in CI.
- Someone with access to the Docker host can read metric and log traffic.
  A `ca_file` for the VictoriaMetrics and VictoriaLogs clients would let them
  move to TLS; it is a follow-up.
- The `serverdata` volume holds the built-in CA. Losing it means enrolling
  every agent and relay again, so it must be backed up.
- The server image has no `aptly`, `reposync` or `createrepo_c`: package
  mirrors (ADR-0030) need an image that adds them.
