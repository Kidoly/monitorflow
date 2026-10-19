# MonitorFlow — Architecture

Status: draft, 2026-10-18. This document is the technical reference for every contributor and every Claude Code thread. Product scope, roadmap and business model live in [product.md](product.md). Changes to a decision below go through an ADR in [adr/](adr/).

## 1. Overview

MonitorFlow is an open source monitoring and fleet management platform written in Rust, in the spirit of Zabbix or PRTG. Target scale for the first version: about 1,000 Linux servers (Debian/Ubuntu and RHEL/Rocky).

It does three things:

1. **Monitor** servers through lightweight Rust agents (sensors, thresholds, alerts, graphs).
2. **Update** servers from central apt and dnf mirrors, through scheduled campaigns run in waves.
3. **Remediate** incidents with an external AI that proposes shell commands, executed under a risk policy with human approval, audit and rollback.

## 2. Components

Three binaries, one language. Every connection is opened from the lower tier upward, so isolated sites need no inbound ports.

```
 +-----------------+        +-----------------+        +-----------------+
 |     Agent       | -----> |     Relay       | -----> | Central server  |
 | (each server)   |  mTLS  | (per site, can  |  mTLS  | (main site)     |
 |                 |  gRPC  |  be chained)    |  gRPC  |                 |
 +-----------------+        +-----------------+        +-----------------+
         \_______________________ direct mTLS/gRPC ___________/
                      (agents may also connect without a relay)

 Arrows show who opens the connection. Commands travel back down the same stream.
```

| Component | Crate | Runs on | Responsibilities |
| --- | --- | --- | --- |
| Agent | `crates/agent` | Each managed server | Collects sensor data, applies package source configuration, runs updates and approved commands, reports results |
| Relay | `crates/relay` | One or more per isolated network or MSP site | Forwards metrics, logs and commands; buffers on disk when the uplink is down; serves a local copy of the package mirrors |
| Central server | `crates/server` | Main site | gRPC ingress, REST API, threshold engine, update scheduler, AI orchestration, audit log, certificate authority, master mirrors, serves the web interface |

Shared code:

| Crate | Contents |
| --- | --- |
| `crates/common` | Generated Protobuf/gRPC code, TLS and certificate helpers, configuration loading, shared types and error types |
| `crates/sensors` | Sensor trait and implementations. No transport code (gRPC, TLS, HTTP), so it can be reused by a future Windows agent; probes like a TCP connect are allowed (ADR-0021) |

Relays can be chained. Each relay keeps a store-and-forward buffer on disk: if its uplink drops, data is kept and sent once the link returns, in order.

## 3. Communication

### Transport

gRPC over HTTP/2 with mutual TLS on every link: agent ↔ relay, relay ↔ relay, relay ↔ central, agent ↔ central. Rust stack: `tonic` + `prost` for gRPC, `rustls` for TLS (no OpenSSL dependency).

### Why gRPC

- **Push with a return channel.** Each agent opens one long-lived bidirectional stream. Metrics go up; commands (updates, approved AI actions) come back down the same stream. The agent never listens on a port.
- **Typed contracts.** Protobuf schemas in `proto/` define every message and document the protocol for contributors. They are the compatibility boundary between versions.
- **Efficiency.** Binary encoding and HTTP/2 multiplexing keep bandwidth low at 1,000 agents.

### Protocol principles

- One service, `AgentService`, with a bidirectional `Stream` RPC. Messages are a `oneof` envelope so new message types are additive.
- Request/response calls that must be authenticated as the connecting peer itself, not relayed, are unary RPCs of their own service on the same mTLS listener, like `CertificateService::Renew` (ADR-0010).
- Every message carries a monotonic sequence number for ordering and deduplication after reconnects. Who sent it is known from the connection's certificate and the stream's `Hello` (see §4, peer identity).
- Relays are transparent for message semantics: they forward envelopes, they do not interpret them, except to buffer and to serve mirrors. The upstream checks which agents a relay may speak for, as described in §4.
- Protocol versions are negotiated in the first `Hello` message. A server must accept agents one minor version behind.

## 4. Security model

- **mTLS everywhere.** No plaintext link, ever, including in development. TLS 1.3 only, ALPN `h2`; peers trust only the MonitorFlow CA, never system roots. Helpers live in `common::tls`; `scripts/dev-certs.sh` creates a throwaway dev CA for local runs.
- **Peer identity and roles.** Every certificate carries exactly one URI SAN `monitorflow://<role>/<id>`, with role `agent`, `relay` or `server` and an ID of 1 to 64 characters from `[A-Za-z0-9._-]`. The CN is informational and never used to decide identity. Handlers read the verified identity with `common::tls::peer_identity`. See ADR-0004.
- **Who may speak for an agent.** A peer whose certificate role is `agent` may only send a `Hello.agent_id` equal to its own certificate ID. A peer whose role is `relay` forwards for other agents: the central server accepts an `agent_id` from a relay only if that agent is assigned to the relay's site in the database. Consequence: a compromised relay can forge upstream data (metrics, logs, command results) only for agents of its own site. It still cannot inject commands downstream, because every command is signed by the central server (below).
- **Built-in certificate authority** on the central server. Enrollment: an administrator creates a one-time token; a new agent or relay presents it and receives a short-lived client certificate; the peer renews it over mTLS before expiry (ADR-0010).
- **Revocation.** An agent or relay can be revoked from the web interface; its certificate is rejected at the next connection (the server keeps a revocation list checked on handshake).
- **Signed commands.** Every command sent to an agent is signed by the central server's command key. The agent verifies the signature before execution, so a compromised relay cannot inject commands.
- **Least privilege.** The agent runs as a dedicated system user. Privileged operations go through sudo rules the administrator can review, generated by the package installer.
- **Web API.** Browsers reach the REST API through a TLS reverse proxy; the API listener itself is plain HTTP on loopback (ADR-0006). Sessions are server-side, cookie `HttpOnly`, `SameSite=Strict`, `Secure`.
- **Secrets never leave the site in clear.** AI context is redacted (see §8) before it is sent to an external API.

## 5. Monitoring

### Sensors

| Sensor | What it measures | Typical threshold |
| --- | --- | --- |
| CPU | Usage per core, load average | Load above N for M minutes |
| Memory | Used, available, swap | Available below a percentage |
| Disk | Space and inodes per mount, I/O | Free space below a percentage |
| Service | systemd unit state | Unit not active |
| Port | TCP/UDP reachability, local or remote | Closed or timeout |
| Log | Pattern matches in files under the agent's log directories (journald later) | Match count above N in a window |
| Custom script | A script installed on the host by its administrator: Nagios exit code and `name=value` perfdata | Status WARNING or CRITICAL |

Each sensor has a collection interval and one or more thresholds (warning, critical) with a duration, to avoid alerting on short spikes. Sensors implement one trait in `crates/sensors` and produce `Sample` values; the agent schedules them and ships samples upstream in batches. Sensors configured by the server only touch what the agent's own configuration allows (scripts it installed, log directories), see ADR-0024.

### Threshold engine

Runs on the central server, not on the agent, so rules can change without touching the fleet. It evaluates incoming samples against the sensor's thresholds, tracks state per (server, sensor), and opens or closes incidents. Alert state is persisted in PostgreSQL so a server restart does not lose or duplicate incidents.

## 6. Storage

| Data | Store | Why |
| --- | --- | --- |
| Metrics | VictoriaMetrics | Efficient at this scale, single binary, Prometheus-compatible query language, Apache 2.0 |
| Logs | VictoriaLogs | Same family and operating model, Apache 2.0 |
| Inventory, configuration, campaigns, incidents, audit log | PostgreSQL | Relational data, transactions, mature tooling. Accessed with `sqlx`, queries checked at compile time |

The audit log table is append-only: the application role has INSERT and SELECT only.

## 7. Package mirroring and updates

The central server hosts full repository mirrors. Relays sync a copy. Agents point their package sources at the nearest relay or at the central server.

| Family | Distributions | Mirror tool | Notes |
| --- | --- | --- | --- |
| apt | Debian, Ubuntu | `aptly` | Snapshots let a campaign pin an exact repository state |
| dnf | RHEL, Rocky | `reposync` + `createrepo_c` | Snapshots kept as dated directories |

Mirroring reuses proven tools, orchestrated from Rust, rather than reimplementing repository formats. Pulp is an alternative that handles both families in one tool, at the cost of a heavier deployment; it is not used in the first version.

### Source configuration

- The agent rewrites `/etc/apt/sources.list.d/` or `/etc/yum.repos.d/` to point at its assigned mirror.
- Original files are backed up before any change, so a server can be reverted.
- Repository signing keys are distributed by the central server.

### Update campaigns

1. An administrator creates a campaign: target group, repository snapshot, maintenance window.
2. The campaign runs in waves (for example a pilot group first), with automatic stop on failure.
3. Agents apply updates and report installed packages and whether a reboot is required.
4. Results appear per server in the web interface and in the audit log.

## 8. AI-driven remediation

When a threshold is crossed, the central server can ask an external AI API to diagnose and propose shell commands. The AI layer is provider-agnostic: one trait, one adapter per provider. The Claude API is the first adapter.

### Flow

1. A sensor crosses a threshold and an incident opens.
2. The server gathers context: sensor history, recent logs, server inventory, recent changes.
3. The AI receives the context and proposes a diagnosis and commands, one at a time.
4. Each command is classified by risk and either runs automatically or waits for approval.
5. The agent runs the command and returns its output; the AI decides the next step.
6. The incident closes when the sensor returns to normal, or escalates to a human.

### Risk tiers

| Tier | Examples | Default policy |
| --- | --- | --- |
| Read-only | `df -h`, `systemctl status`, `journalctl`, `ss -tlnp` | Runs automatically |
| Low-impact change | Restart a service, clear a cache directory | Approval required; can be set to automatic per server group |
| High-impact change | Package changes, config edits, kill processes, reboot | Approval always required |
| Forbidden | Administrator-configured patterns (`rm -rf /`, disk formatting, …) | Blocked |

Classification uses rules first (allow-lists and deny patterns). Unknown commands default to high-impact. The AI's own opinion of a command's risk is never used for classification.

### Approval, audit, rollback

- **Approval:** pending commands appear in the web interface with the AI's reasoning; an approver accepts, edits or rejects each one.
- **Audit:** each incident records the context sent to the AI, every proposal, who approved it, the exact command, its output and exit code.
- **Rollback:** an arbitrary shell command cannot be undone reliably. Before any change-tier command, the server takes a snapshot when the platform supports it (Proxmox VM snapshot first; LVM, Btrfs, ZFS later). Config files touched by a command are also backed up. Without snapshot support, the interface shows that rollback is limited.

### Threats addressed

- **Prompt injection.** Logs and command outputs may contain text crafted to steer the AI. Context is passed as clearly delimited data, and risk tiers apply whatever the AI says.
- **Data leaving the network.** Secrets are redacted before sending. In MSP mode one AI configuration serves all customers, and each request carries data from a single customer only.
- **Runaway loops.** Each incident has a limit on commands and duration before it escalates to a human.

## 9. Web interface

Next.js (React, TypeScript) built as a static export and served by the central server. It talks to the `axum` REST/JSON API only; it never speaks gRPC.

Screens for the first version: fleet overview, graphs, alerts and incidents, approval queue, update campaigns, audit log, administration (agents, relays, tokens, certificates, users, roles).

Access is role-based: viewer, operator, approver, administrator. In MSP mode each customer sees only its own sites; tenancy is enforced in the API layer, never only in the frontend.

## 10. Tech stack

| Layer | Choice |
| --- | --- |
| Language | Rust 2021 for agent, relay and server; toolchain pinned in `rust-toolchain.toml` (ADR-0005) |
| Async runtime | `tokio` |
| RPC | `tonic` + `prost` |
| TLS | `rustls` |
| HTTP API | `axum` |
| Database | PostgreSQL via `sqlx` |
| Metrics / logs | VictoriaMetrics / VictoriaLogs |
| Mirrors | `aptly`, `reposync`, `createrepo_c` |
| Frontend | Next.js static export |
| Packaging | `.deb` and `.rpm` for agent and relay; containers for the server |
| Logging | `tracing` with JSON output in production |

## 11. Repository layout

```
proto/            Protobuf definitions shared by all components
crates/common     Generated gRPC code, TLS, configuration, shared types
crates/sensors    Sensor trait and implementations (no transport code)
crates/agent      Agent binary
crates/relay      Relay binary
crates/server     Central server binary (gRPC, REST API, engines, CA)
web/              Next.js frontend
deploy/           Dockerfiles, compose files, packaging scripts
docs/             This document, product.md, ADRs
scripts/          Developer scripts
```

## 12. Decisions

Recorded in short form here; rationale in `docs/adr/`.

| Topic | Decision |
| --- | --- |
| Licence | AGPLv3 |
| Contributions | Contributor License Agreement |
| Frontend | Next.js, static export |
| AI provider | Provider-agnostic trait; Claude API first |
| AI in MSP mode | One shared configuration; one customer per request; secrets redacted |
| Snapshots | Proxmox first |
| Notifications | Not in the first version |
| Threshold evaluation | Server side |
| Time series | VictoriaMetrics, not a custom store |
| Peer identity | Role and ID in a `monitorflow://<role>/<id>` URI SAN; relays speak only for agents of their site |
| Web API transport | Plain HTTP on loopback behind a TLS reverse proxy; server-side sessions (ADR-0006) |
| Sensor scheduling | One task per sensor, jittered first tick, timeout within the interval, missed ticks skipped, full channel drops new samples (ADR-0012) |
| Sensor configuration | Built-in defaults, then site rows, then server rows in PostgreSQL; pushed after Hello and on revision change, polled every 10 s; agents acknowledge and persist it (ADR-0016) |
| Threshold engine | Thresholds per metric with site and server layers over built-in defaults; per-severity timers on sample time with hysteresis; one open incident per series by partial unique index; state in `alert_states` reloaded at start (ADR-0017) |
| Relay forwarding | The relay terminates agent mTLS (CA, CRL fetched from upstream, Hello ID = certificate ID) and opens one upstream stream per agent stream with its own certificate, forwarding envelopes unchanged; relay certificates carry admin-chosen server names (ADR-0025) |
| Renewal through relays | The relay serves `CertificateService::Renew` and forwards it as `RenewForwarded` with the agent's verified certificate; the server checks it against the CA, the relay policy and ADR-0010's rules (ADR-0028) |
