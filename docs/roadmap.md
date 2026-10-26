# Roadmap — task breakdown

Each task is sized to be one Claude Code thread on its own branch. Check a task when its PR is merged. Order within a phase matters unless noted.

## Phase 1 — Foundations

- [x] Repository skeleton: workspace, crates, CI, docs, dev compose.
- [x] `proto/monitorflow/v1/agent.proto`: Hello, Heartbeat, SampleBatch, Command, CommandResult envelope; build with `tonic-build` in `crates/common`.
- [x] `common::config`: TOML configuration loading for the three binaries, with env overrides and validation.
- [x] `common::tls`: rustls client and server configs from PEM files; mTLS with client cert verification.
- [x] `server::ca`: certificate authority (root key at rest, issue short-lived client certs, revocation list) using `rcgen` and `x509-parser`.
- [x] `server::enroll`: one-time enrollment tokens stored in PostgreSQL; gRPC `Enroll` RPC that exchanges a token for a certificate.
- [x] `server::grpc`: `AgentService::Stream` bidirectional stream; registry of connected agents; heartbeat timeout.
- [x] `agent`: connect, enroll on first run, keep the stream open with reconnect and backoff, send heartbeats.
- [x] PostgreSQL migrations: servers, enrollment_tokens, certificates, users, roles.
- [x] `server::api`: axum skeleton, health endpoint, session auth, `GET /api/servers`.
- [x] Certificate renewal before expiry, driven by the agent.
- [x] End-to-end test: start server, enroll agent, see it listed via the API.

## Phase 2 — Monitoring

- [x] `sensors`: Sensor trait, Sample type, scheduler.
- [x] Sensors: cpu, memory, disk (one task each).
- [x] Sensors: systemd service, port, log pattern, custom script (one task each).
- [x] Agent: sensor configuration pushed from the server, sample batching and upload.
- [x] Server: write samples to VictoriaMetrics (remote write) and logs to VictoriaLogs.
- [x] Server: threshold engine with warning/critical and duration; incidents table.
- [x] API: sensors, thresholds, incidents, metric query proxy.
- [x] Web: Next.js project, auth, fleet overview.
- [x] Web: server page with graphs and threshold lines.
- [x] Web: incidents list and timeline.

## Phase 3 — Relays

- [x] Relay binary: accept agent connections, forward to upstream.
- [x] Store-and-forward buffer on disk with ordering and bounded size.
- [x] Relay chaining and relay enrollment.
- [x] Multi-tenancy: customers, sites, tenancy enforcement in the API.
- [x] Web: administration of relays and sites.

## Phase 4 — Packages and updates

- [x] Mirror orchestration: aptly for apt, reposync + createrepo_c for dnf; snapshots.
- [x] Relay mirror sync.
- [x] Agent: rewrite package sources with backup and revert; distribute signing keys.
- [x] Campaigns: model, waves, maintenance windows, stop on failure.
- [x] Agent: apply updates, report packages and reboot-required.
- [x] Web: campaign creation and progress.

## Phase 5 — AI remediation

- [x] AI provider trait and Claude adapter.
- [x] Context gathering with secret redaction.
- [x] Risk classifier: allow-lists, deny patterns, default to high-impact.
- [x] Incident loop with command and duration limits.
- [x] Approval queue, API and web.
- [x] Append-only audit log.
- [x] Proxmox snapshot integration before change-tier commands.

## Phase 6 — Windows agent

Approach in ADR-0047: the same agent binary built for Windows, run as a service under a virtual account.

- [x] Windows agent build, service, enrollment and inventory; Windows section of the install guide; CI job on Windows.
- [x] Windows sensors: CPU, memory, disk, port, services, Event Log, PowerShell scripts (can run alongside the previous task).
- [x] Windows Update inventory and installs through update campaigns (ADR-0049).
- [x] MSI package for the agent: binary, service, account, ACLs, event log source (ADR-0050).

## Phase 7 — Packaging

- [x] Production compose for the central server: server and web images, PostgreSQL over TLS, install guide, smoke test in CI (ADR-0051).
- [ ] Agent releases: static Linux binaries and the Windows MSI on GitHub Releases, one-line Linux installer that can be self-hosted, install command on the token page, end-to-end test in CI (ADR-0052).
