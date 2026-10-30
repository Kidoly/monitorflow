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
- [x] Agent releases: static Linux binaries and the Windows MSI on GitHub Releases, one-line Linux installer that can be self-hosted, install command on the token page, end-to-end test in CI (ADR-0052).
- [x] Agent self-update: signed releases, Linux helper with health check and rollback, Windows through the update service, release cache on the server and relays, update action, rollouts and per-site policy in the web interface (ADR-0053).
- [x] Agent install sources: one-line install from GitHub (default), from the central server's release cache over the web interface's HTTPS with checksums pinned in the command, or from a custom URL; server-wide default and per-command switch on the tokens page, offline `agent-release import`, end-to-end test in `central` mode (ADR-0054).
- [x] Server updates from the web interface: published server, web and updater images whose digests the release signs, an updater container that verifies, backs up PostgreSQL, updates and rolls back, check and update in two clicks, offline path, end-to-end test in CI (ADR-0056).
- [x] Automatic updates: on by default with site overrides, a canary per site then batches, halt on failures, version compatibility (server ≥ relay ≥ agent); relay releases, installer and self-update with rollback, relays updated before their agents (ADR-0055).

## Phase 8 — Simplicity

Everything installable in one line, updatable in two clicks, and usable without reading documentation. Findings, journeys and the reasoning behind each task are in `docs/design/simplicity-audit.md`; tasks are in order of impact, and those without a dependency can run in parallel.

- [x] First-run setup in the browser: one-time setup link while no user exists, `/setup/` creates the first administrator, a login page that says so, a default site.
- [ ] One-line server installer (`curl … | sudo sh`): installs Docker if missing, no git clone, generates secrets, guesses and confirms the public name, pulls published images, prints the URL and setup link; `--uninstall` (ADR; needs a release with the ADR-0056 images).
- [x] Server and relay offline incidents: a built-in rule on missed heartbeats, counted in the fleet tiles, can be muted per site or server.
- [x] Notifications: email and webhook channels configured in the interface with a test button, for incidents, approvals and failed updates (ADR revising the product non-goal).
- [x] Reusable install command per site: an install key valid for N hosts or days, hostname as the ID, optional approval of new hosts; single-use tokens under Advanced (ADR).
- [x] Editable server address in install commands: a public address setting defaulted to the first server name, picked or edited on the tokens page (the published ports are already used).
- [ ] Relay onboarding without typing: Relays › Add relay, server names reported by the relay and confirmed instead of typed (ADR), mirroring as a checkbox, "Add servers behind this relay".
- [x] Edit thresholds and add checks in the interface, per server or per site, in plain words.
- [x] Users, customers and roles in the interface: create customers, invite users by one-time link, grant and revoke roles.
- [x] One-click backups: daily backup of the database and `serverdata` with retention, back up now and download in Settings, `--restore` in the server installer; replaces the unpruned update dumps.
- [ ] Update available everywhere: daily background release check with a banner and a notification; release import from the interface for servers without Internet.
- [ ] AI setup in the interface: API key, model, test and enable in Settings; `MF_ALLOW_COMMANDS=1` in the agent installer for the shell opt-in; approvals say when AI is off.
- [ ] Mirrors that work in the production compose: mirror tools sidecar image, add a mirror from a list of distributions with a size estimate.
- [ ] Plain-language pass: rename jargon on screen, certificate expiry warning only when renewal is late or failed, quiet CLI output, an action button in every empty state.
- [ ] Remove a server end to end: revoke, hide from lists, show the uninstall command, re-enroll the same host later.
- [ ] Agent on any Linux: start and monitor on unknown distributions with package features reported as unavailable.
- [ ] Browser certificate without a terminal: ACME when the name is public, otherwise a trust page for the internal CA (ADR, as it is a plaintext listener).
- [ ] Settings page for runtime server settings, and a one-screen "Getting started" at the top of the install guides.
