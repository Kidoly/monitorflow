# ADR-0002: VictoriaMetrics, VictoriaLogs and PostgreSQL

Date: 2026-10-18
Status: accepted

## Context

The platform stores three kinds of data: metrics at high volume, logs, and relational state (inventory, configuration, campaigns, incidents, audit). Writing a time-series store is out of scope. Dependencies must have permissive licences so that the platform's AGPL licence remains the only copyleft in play.

## Decision

Metrics go to VictoriaMetrics, logs to VictoriaLogs, both Apache 2.0, single binary, operated the same way. Relational data goes to PostgreSQL through `sqlx` with compile-time checked queries. The audit table is append-only at the database role level.

## Consequences

- The server speaks Prometheus remote write and the VictoriaLogs ingest API.
- The web interface queries metrics through a server-side proxy, never directly.
- A dev environment needs three services; `deploy/docker-compose.dev.yml` provides them.
