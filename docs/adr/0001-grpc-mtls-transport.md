# ADR-0001: gRPC over mTLS as the only transport

Date: 2026-10-18
Status: accepted

## Context

Agents sit on customer networks that allow no inbound connections. The server must still push commands to them. The protocol must stay compatible across versions deployed on 1,000 servers that update at different times.

## Decision

Every link uses gRPC (tonic + prost) over HTTP/2 with mutual TLS (rustls). Each agent opens one long-lived bidirectional stream; commands travel back down that stream. Protobuf messages in `proto/` are the compatibility contract; changes must be additive.

## Consequences

- No inbound port on agents or relays.
- Relays can forward envelopes without understanding them.
- Development needs a local CA; the dev scripts generate one.
- HTTP/1 clients cannot talk to agents; the web interface uses the separate REST API on the server.
