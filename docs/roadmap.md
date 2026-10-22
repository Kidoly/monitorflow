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
- [ ] Web: campaign creation and progress.

## Phase 5 — AI remediation

- [ ] AI provider trait and Claude adapter.
- [ ] Context gathering with secret redaction.
- [ ] Risk classifier: allow-lists, deny patterns, default to high-impact.
- [ ] Incident loop with command and duration limits.
- [ ] Approval queue, API and web.
- [ ] Append-only audit log.
- [ ] Proxmox snapshot integration before change-tier commands.

## Phase 6 — Windows agent

- [ ] To be planned.
