# MonitorFlow — Architecture

Status: draft, 2026-10-18. This document is the technical reference for every contributor and every Claude Code thread. Product scope, roadmap and business model live in [product.md](product.md). Changes to a decision below go through an ADR in [adr/](adr/).

## 1. Overview

MonitorFlow is an open source monitoring and fleet management platform written in Rust, in the spirit of Zabbix or PRTG. Target scale for the first version: about 1,000 Linux servers (Debian/Ubuntu and RHEL/Rocky for package updates, any distribution for monitoring, ADR-0063).

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
- **Who may speak for an agent.** A peer whose certificate role is `agent` may only send a `Hello.agent_id` equal to its own certificate ID. A peer whose role is `relay` forwards for other agents: the central server accepts an `agent_id` from a relay only if that agent is assigned to the relay's site in the database. Relays can be chained: the server then checks the whole path a call took, each hop naming only the peer it verified, against the parent relay the administrator chose for each relay, so a relay speaks only for its own subtree (ADR-0028). Consequence: a compromised relay can forge upstream data (metrics, logs, command results) only for agents of its own site, or of its subtree when chained. It still cannot inject commands downstream, because every command is signed by the central server (below).
- **Built-in certificate authority** on the central server. Enrollment: an administrator creates a one-time token, or a reusable install key per site for agents, which then take their host name as ID (ADR-0058); a new agent or relay presents it and receives a short-lived client certificate; the peer renews it over mTLS before expiry (ADR-0010).
- **Revocation.** Removing an agent or relay in the web interface revokes its certificate, which is rejected at the next connection (the server keeps a revocation list checked on handshake). It leaves the lists, keeps its history and can enroll again in the same site (ADR-0062).
- **Signed commands.** Every command sent to an agent is signed by the central server's command key. The agent verifies the signature before execution, so a compromised relay cannot inject commands.
- **Signed releases.** Release files are signed with an offline Ed25519 key whose public half is compiled into every binary; agents and the installer refuse files it did not sign, whoever serves them, and agents refuse downgrades unless told explicitly (ADR-0053).
- **Least privilege.** The agent runs as a dedicated system user. Privileged operations go through sudo rules the administrator can review, generated by the package installer.
- **Storage links.** The server reaches PostgreSQL over TLS verified by name. In the production compose, VictoriaMetrics and VictoriaLogs are reached over plain HTTP on an internal Docker network that is never published, until their clients accept a private CA (ADR-0051).
- **Web API.** Browsers reach the REST API through a TLS reverse proxy; the API listener itself is plain HTTP on loopback (ADR-0006). Sessions are server-side, cookie `HttpOnly`, `SameSite=Strict`, `Secure`.
- **Secrets never leave the site in clear.** AI context is redacted (see §8) before it is sent to an external API.

## 5. Monitoring

### Sensors

| Sensor | What it measures | Typical threshold |
| --- | --- | --- |
| CPU | Usage per core, load average (Linux) | Load above N for M minutes |
| Memory | Used, available, swap (page files on Windows) | Available below a percentage |
| Disk | Space and inodes per mount, or per fixed drive on Windows; I/O later | Free space below a percentage |
| Service | systemd unit state; Windows service state (`windows_service`) | Unit not active |
| Port | TCP/UDP reachability, local or remote | Closed or timeout |
| Log | Pattern matches in files under the agent's log directories (journald later); events of allowed Windows Event Log channels (`eventlog`) | Match count above N in a window |
| Custom script | A script installed on the host by its administrator (PowerShell or a program on Windows): Nagios exit code and `name=value` perfdata | Status WARNING or CRITICAL |

Each sensor has a collection interval and one or more thresholds (warning, critical) with a duration, to avoid alerting on short spikes. Sensors implement one trait in `crates/sensors` and produce `Sample` values; the agent schedules them and ships samples upstream in batches. Sensors configured by the server only touch what the agent's own configuration allows (scripts it installed, log directories, Event Log channels), see ADR-0024.

The same crate serves Windows agents (ADR-0048): `cpu`, `memory`, `disk`, `port` and `script` keep their kind and metrics with Win32 implementations; `windows_service` and `eventlog` are Windows kinds, `systemd` and `log` Unix kinds. An agent refuses a kind its platform does not run as unsupported, while the server validates every kind's parameters whatever the agent's platform.

### Threshold engine

Runs on the central server, not on the agent, so rules can change without touching the fleet. It evaluates incoming samples against the sensor's thresholds, tracks state per (server, sensor), and opens or closes incidents. Alert state is persisted in PostgreSQL so a server restart does not lose or duplicate incidents.

### Notifications

Database triggers on incident events, approvals and failed updates write an outbox, with failed backups copied from the updater's status; a server task delivers it to the email and webhook channels administrators set up in the web interface, each for every customer or one (ADR-0060).

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

Other distributions, and hosts of these families without their tools, are monitored the same way but take no updates or sources: the agent says so in its `Hello`, the server stores it as `servers.package_updates`, campaigns leave them out and the server page says why (ADR-0063).

Mirroring reuses proven tools, orchestrated from Rust, rather than reimplementing repository formats. Pulp is an alternative that handles both families in one tool, at the cost of a heavier deployment; it is not used in the first version.

### Mirrors on the server

Mirrors are global, managed by global administrators (ADR-0030). Each has a family, an upstream URL, for apt a suite and components, optional architectures and a sync interval or manual syncs. A sync runs `aptly mirror update` (one aptly mirror per component) or `reposync` then `createrepo_c`; by default it ends with a snapshot: an aptly publish for apt, a hardlinked dated directory for dnf. One job runs per mirror at a time, under a PostgreSQL advisory lock; every sync and snapshot is a run row with its status and tool output.

A plain HTTP listener (`mirror_listen_addr`, default `0.0.0.0:7480`) serves `mirror_dir` (default `<data_dir>/mirrors`) with a fixed layout that relays and agents rely on:

| URL | Content |
| --- | --- |
| `/apt/<mirror>/<snapshot>/`, `/apt/<mirror>/current/` | aptly publish of a snapshot; `current` is the newest |
| `/dnf/<mirror>/<snapshot>/`, `/dnf/<mirror>/current/` | dnf repository of a snapshot; `current` is the newest |
| `/keys/monitorflow-archive.asc` | public key signing apt `Release` files and dnf `repomd.xml` |

Integrity comes from the signatures, not the transport.

In the production compose the tools run in the `mirror` sidecar, which shares `mirror_dir` with the server and takes commands over a Unix socket in it (ADR-0067). Administrators add a distribution from a list in the web interface, which shows its size and the free space and refuses what does not fit; the signing key and the upstream archive keys need no step. On a host without the sidecar, or for a mirror outside the list:

1. Install `aptly`, `dnf-plugins-core` (for `reposync`), `createrepo_c` and `gpg`, or set their paths under `[mirror]`.
2. Create the signing key with `monitorflow-server mirror key generate`, or import an existing secret key with `monitorflow-server mirror key import --file key.asc`. It lives in `<mirror_dir>/gnupg` without a passphrase; the public half is written to `<mirror_dir>/keys/monitorflow-archive.asc`.
3. For apt, add the upstream archive keys to the keyring aptly verifies with: `gpg --no-default-keyring --keyring <mirror_dir>/upstream-keyring.gpg --import /usr/share/keyrings/debian-archive-keyring.gpg`.
4. `monitorflow-server mirror add …`, then `mirror sync --name <mirror>`.

### Mirrors on relays

Each relay keeps a copy of the upstream's published mirrors and serves it with the same layout on its own plain HTTP listener (`mirror_listen_addr`, default `0.0.0.0:7480`, over `mirror_dir`, default `<data_dir>/mirrors`), so agents behind it point their sources at the relay (ADR-0034). Every mirror listener also serves `/index.json`, built from its tree: each mirror, its family, the snapshot `current` names and, for apt, its distributions. A relay fetches the index and the snapshots over plain HTTP from `mirror.upstream_url` (by default port 7480 on the host of `upstream_url`, so a chained relay syncs from its parent), every `mirror.sync_interval` (15 minutes). It checks every file against the checksums of the signed metadata (`Release` then `Packages`, `repomd.xml` then `primary`), builds the snapshot in a hidden staging directory, reuses unchanged files of earlier snapshots through hardlinks, resumes an interrupted sync file by file, and swaps `current` atomically once the snapshot is complete. It keeps `mirror.keep_snapshots` (3) snapshots per mirror. Signatures are checked by the agents, not the relay.

A mirroring relay with `mirror.public_url` set reports that URL and the snapshots it holds on its own stream, after `Hello` and on every change. Source configuration for a server whose agent connects through that relay uses the relay's URL when the relay holds every snapshot named, and the server's `mirror_public_url` otherwise (ADR-0037).

### Source configuration

- The agent rewrites `/etc/apt/sources.list.d/` or `/etc/yum.repos.d/` to point at its assigned mirror.
- Original files are backed up before any change, so a server can be reverted.
- Repository signing keys are distributed by the central server.
- The spec (`ConfigureSources`) carries the mirror URL, repositories and armored keys; the agent checks every key's fingerprint before installing it. `replace` mode sets the other sources aside (apt) or disables them (dnf); `add` keeps them.
- The manifest of the backup (path, mode, owner, SHA-256) and the copies are on disk before the first change; a failure midway or of the post-check (`apt-get update`, `dnf makecache`) reverts to the originals.
- The agent writes through a root helper, `sudo -n monitorflow-agent sources apply|revert`, allowed by exact sudoers rules (ADR-0032).

### Update campaigns

1. An administrator creates a campaign: target group, repository snapshot, maintenance window.
2. The campaign runs in waves (for example a pilot group first), with automatic stop on failure.
3. Agents apply updates and report installed packages and whether a reboot is required.
4. Results appear per server in the web interface and in the audit log.

Updates travel as `apply_updates` commands on the agent's stream, signed with the CA key, one in flight per server; the agent keeps a journal so a command redelivered after a reconnect never runs twice, runs apt or dnf through a root helper, `sudo -n monitorflow-agent packages update` with the kind of update and the package names on stdin (ADR-0036), and sends a `PackageReport` after each update and every 6 hours (ADR-0031).

Campaigns belong to one customer and are created and driven by its administrators (ADR-0033):

- Targets (servers and/or a site) are frozen at creation into ordered waves, given as explicit servers, sizes or percentages; the rest forms a last wave.
- Maintenance windows are weekly (weekdays, local start and end, IANA time zone) or one-off ranges, evaluated by PostgreSQL; commands are queued only inside one, with a TTL capped at its end.
- A runner task advances each campaign under its row lock every 10 s: it queues the current wave's commands, waits for their results, then moves on. A wave whose failures exceed the threshold (a count or a percentage of the wave) fails the campaign; queued commands are cancelled, sent ones finish.
- States `draft`, `scheduled`, `running`, `paused`, `stopped`, `completed`, `failed`; start, pause, resume and stop are explicit, audited actions. All state is in the database, so a restart resumes where it was.
- A campaign may pin published mirror snapshots (ADR-0038): each server gets `configure_sources` for them, then its update, then its previous sources back (its last configuration made outside campaigns, or a revert), even when the campaign pauses, stops or fails. The mirror index lists the snapshots of unfinished campaigns as `pinned`, so relays copy and keep them.

### Windows Update

Windows servers take part in the same campaigns with Windows Update semantics (ADR-0049). Their source stays whatever the machine is configured for: Windows Update, Microsoft Update or WSUS, with its approvals. Mirrors, pinned snapshots and `configure_sources`/`revert_sources` do not apply to them, so a pinned campaign sends a Windows server its update alone.

- The agent's account cannot install updates. A second service, `monitorflow-agent.exe update-service` running as `MonitorFlowUpdate` under LocalSystem, makes every Windows Update Agent call (search, download, install) and the reboot. It takes typed JSON requests on the named pipe `\\.\pipe\MonitorFlowUpdate`, whose DACL admits only SYSTEM and the agent's service SID.
- Reports list pending and installed software updates as packages named by KB number, with title, classification, MSRC severity, size and whether a restart is involved.
- `apply_updates` installs every pending update except feature upgrades (`all`), the `Security Updates` classification (`security`), or the KB numbers or update IDs named. Names must fit every target's platform. A restart is reported and only happens when the command allows it, one minute after the result.

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

- **Approval:** pending commands appear in the web interface with the AI's reasoning; an approver accepts, edits or rejects each one. Approvers and administrators decide low-impact commands, only administrators high-impact ones; an edited command is classified again, and a rejection needs a reason, which goes back to the AI (ADR-0045).
- **Audit:** each incident records the context sent to the AI, every proposal, who approved it, the exact command, its output and exit code, in a hash-chained history (ADR-0042). The web interface's remediation audit log reads these histories across incidents and verifies one incident's chain on demand (ADR-0045).
- **Rollback:** an arbitrary shell command cannot be undone reliably. Before any change-tier command, the server takes a snapshot when the platform supports it (Proxmox VM snapshot first; LVM, Btrfs, ZFS later). Config files touched by a command are also backed up. Without snapshot support, the interface shows that rollback is limited. On a server mapped to a Proxmox VM, a failed snapshot escalates the incident without running the command (ADR-0046).

### Threats addressed

- **Prompt injection.** Logs and command outputs may contain text crafted to steer the AI. Context is passed as clearly delimited data, and risk tiers apply whatever the AI says.
- **Data leaving the network.** Secrets are redacted before sending. In MSP mode one AI configuration serves all customers, and each request carries data from a single customer only.
- **Runaway loops.** Each incident has a limit on commands and duration before it escalates to a human.

## 9. Web interface

Next.js (React, TypeScript) built as a static export and served by the central server. It talks to the `axum` REST/JSON API only; it never speaks gRPC.

Screens for the first version: fleet overview, graphs, alerts and incidents, approval queue, update campaigns, audit log, administration (agents, relays, tokens, certificates, users, roles).

The first administrator is created in the browser with a one-time setup link the server prints while no user exists (ADR-0057). Administrators of everything then invite users, grant roles and reset passwords under Administration › Users and roles, with one-time password links (ADR-0059).

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
| Packaging | Static agent binaries, MSI and a one-line installer on GitHub Releases, or self-hosted (ADR-0052), signed with an offline Ed25519 key (ADR-0053); `.deb` and `.rpm` for agent and relay later; containers for the server |
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
| Notifications | Email (SMTP) and signed JSON webhooks set up in the interface with a test button; an outbox filled by triggers on incident events, pending approvals and failed updates, delivered at most once by a polling task; secrets never returned (ADR-0060) |
| Threshold evaluation | Server side |
| Time series | VictoriaMetrics, not a custom store |
| Peer identity | Role and ID in a `monitorflow://<role>/<id>` URI SAN; relays speak only for agents of their site |
| Web API transport | Plain HTTP on loopback behind a TLS reverse proxy; server-side sessions (ADR-0006) |
| Sensor scheduling | One task per sensor, jittered first tick, timeout within the interval, missed ticks skipped, full channel drops new samples (ADR-0012) |
| Windows packaging | WiX v5 MSI: per-machine, service under its virtual account with restart actions, protected ACLs by SID on `%ProgramData%\MonitorFlow`; `agent.toml` written from `UPSTREAM_URL` and a hidden `ENROLLMENT_TOKEN` by `monitorflow-agent init-config` only when absent; data kept on upgrade and uninstall unless `REMOVE_DATA=1`; an Event Log source for the service's fatal error; `AgentHelpers` component group for ADR-0049 (ADR-0050) |
| Windows sensors | Same kinds where the measure is the same; `windows_service` and `eventlog` as Windows kinds; agents refuse other platforms' kinds (ADR-0048) |
| Windows Update | WUA COM API in a LocalSystem service (`update-service` of the agent binary) behind a named pipe for the agent's service SID only; machine's own source (WU or WSUS) respected; updates reported as packages by KB number with classification and severity; `apply_updates` kinds map to all but feature upgrades, the Security Updates classification, or named KBs; reboot only with `allow_reboot`; Windows campaign targets skip pinned sources (ADR-0049) |
| Sensor configuration | Built-in defaults, then site rows, then server rows in PostgreSQL; pushed after Hello and on revision change, polled every 10 s; agents acknowledge and persist it (ADR-0016) |
| Threshold engine | Thresholds per metric with site and server layers over built-in defaults; per-severity timers on sample time with hysteresis; one open incident per series by partial unique index; state in `alert_states` reloaded at start (ADR-0017) |
| Offline incidents | Built-in rule, no configuration: a server or relay without a stream for 5 minutes (since its last message and the server's start) opens a critical incident with the reserved metric `offline`, a relay's with `relay_id` instead of `server_id`; resolved on reconnect, mute or revocation; servers behind an offline relay open none; stateless 30 s task over the database and the registry; muted per site, server or relay through `offline_alerts`, audited (ADR-0057) |
| Relay forwarding | The relay terminates agent mTLS (CA, CRL fetched from upstream, Hello ID = certificate ID) and opens one upstream stream per agent stream with its own certificate, forwarding payloads unchanged; relay certificates carry admin-chosen server names (ADR-0025) |
| Renewal through relays | The relay serves `CertificateService::Renew` and forwards it as `RenewForwarded` with the agent's verified certificate; the server checks it against the CA, the relay policy and ADR-0010's rules (ADR-0028) |
| Relay chaining | Each relay has an admin-chosen parent; every hop adds the peer it verified to a `monitorflow-via` path, and the server accepts only paths along parent links down from the connecting relay; relays serve the CRL downstream (ADR-0028) |
| Peer administration | Enrollment tokens and revocation of agents and relays from the API by customer administrators; tokens returned once; revocation updates rows, certificates and the CRL and closes live streams (ADR-0029) |
| Install keys | One reusable key per site enrolls agents under their host names, made unique and never reusing an ID; 30 days and any number of hosts by default, optional host limit and approval of new hosts, 30 enrollments a minute per key, revocable; recorded as used tokens so inventory code is unchanged; single-use tokens under Advanced (ADR-0058) |
| Relay store-and-forward | `SampleBatch` envelopes buffered on disk per origin (CRC-checked segments, size and age caps, oldest dropped first) while the upstream is down, replayed oldest first before live data with the stored Hello and `monitorflow-via` path; at-least-once for buffered data until server acks exist (ADR-0027) |
| Package mirrors | Global, admin-managed; aptly and reposync + createrepo_c run behind a runner trait, one job per mirror under an advisory lock; snapshots are aptly publishes or hardlinked directories; served over plain HTTP at a fixed layout, integrity from the server's GPG signatures (ADR-0030) |
| Package source configuration | Signed spec with inline keys checked by fingerprint; deb822 or `.repo` files under `/etc`; one active backup with a manifest synced before any change, revert byte for byte, automatic revert on a failed post-check; root helper via exact sudoers rules (ADR-0032) |
| Update campaigns | One customer each; targets frozen into waves at creation; weekly or one-off windows evaluated by PostgreSQL, command TTL capped at the window's end; a polling runner advances waves under the campaign's row lock and fails the campaign when a wave exceeds its count or percentage threshold (ADR-0033) |
| Relay mirror sync | Relays copy `current` of every mirror listed in the upstream's `/index.json` over plain HTTP, checking each file against the signed metadata's checksums, staged then swapped atomically, unchanged files hardlinked from earlier snapshots, bounded retention; served with the server's layout from shared code (ADR-0034) |
| Source configuration commands | `configure_sources` and `revert_sources` commands queued by customer administrators; spec built from the mirrors table, the archive key and `mirror_public_url`, chosen per site by one function a relay mirror URL will override; the agent runs the root helper through `sudo -n`, rules shipped in `deploy/sudoers/` (ADR-0035) |
| Package update helper | Every update (all, security only, named packages) runs as root in `monitorflow-agent packages update`, which reads a JSON spec on stdin, checks package names against the distributions' grammar, detects the family and builds the apt or dnf command lines itself; apt security keeps the `-security` origin selection from a simulation; one exact sudoers line (ADR-0036) |
| Relay mirror URLs | Relays report `mirror.public_url` and their complete snapshots on their own stream (`RelayMirrors`); the server stores them per relay and uses the relay's URL for servers attached to it when it holds every snapshot named, else `mirror_public_url` (ADR-0037) |
| Campaign snapshots | Optional published snapshots per campaign and mirror; per server `configure_sources`, update, then restore of the last non-campaign configuration (or revert), restores never cancelled; `/index.json` lists pinned snapshots from `.pinned` files so relays copy and keep them beside `current` (ADR-0038) |
| AI provider | `monitorflow-ai` crate: `AiProvider::next_step` returns a diagnosis plus one command with its reasoning or a stop (resolved, escalate), no risk field; Claude adapter on the Messages API with the context and earlier steps as `document` blocks, two strict tools parsed strictly, refusal fallback, retries on 408/409/429/5xx honoring `retry-after`; `[ai]` server config, off by default, key read from `api_key_file` (ADR-0039) |
| Remediation audit | AI remediation incidents in their own tables, not `audit_log` or the threshold engine's; events numbered per incident under the incident's row lock, append-only by trigger, hash-chained (SHA-256 of the previous hash and PostgreSQL's canonical `jsonb` text) with the head on the incident; verified in Rust on every read; read-only API under `/api/remediation/incidents` (ADR-0042) |
| Incident context | Bounded sections (breach, inventory, series history, server log stream, commands and campaigns) read concurrently with per-section timeouts and truncation markers; every free-text field redacted by rules into `[REDACTED:kind]` with counts per kind; asserted to hold one customer's data; rendered as hash-tagged data blocks (ADR-0040) |
| Risk classifier | Rules only: a conservative lexer refuses command lists, substitutions, redirections, subshells, environment prefixes, non-ASCII and globs on writes (high impact); pipes only into read-only filters; built-in read-only and low-impact allow-lists, high-impact categories, unknown commands high; deny patterns (built-in and per customer or site) matched leniently everywhere and always win; administrator allow rules only extend unknown commands; per-site `low_impact_auto` policy (ADR-0041) |
| Proxmox snapshots | `Snapshotter` trait in `remediation::snapshot`, Proxmox VE first: HTTPS REST with an API token read from `token_secret_file`, certificates always verified (optional per-cluster `ca_file`), task polling with a timeout; servers mapped to `(cluster, node, vmid)` by customer administrators through `/api/servers/{id}/proxmox`, audited; names `mf-<incident>-<step>`; snapshots MonitorFlow took recorded, only those pruned (keep N per VM) or rolled back to; `[proxmox]` off by default (ADR-0044) |
| Incident loop | A polling supervisor opens one remediation per new alert while AI is on, in `[ai]` or in the web interface, which needs no restart (ADR-0064); each pass derives its next action from the incident's audit events (so restarts and approvals resume from the trail), under a renewed per-incident lease; read-only or auto low-impact commands run as signed `shell` commands (agent opt-in `commands.shell`), others park with a pending row, forbidden escalate; outputs redacted; limits on commands, duration (approval waits included) and per command escalate; resolved when the alert resolves; `ChangeHook` before change-tier commands takes a Proxmox snapshot on mapped servers, a failure escalating without running the command, and records that rollback is limited elsewhere (ADR-0043, ADR-0046) |
| Approval queue | `/api/remediation/approvals` lists pending commands with the AI's text, the rules' tier, the deadline, the user's rights and snapshot support; approving low impact needs an approver role on the site, high impact an administrator role, rejecting (with a reason the AI gets back) any approver; edits are classified again and need the higher tier's role; one transaction under the incident's lock appends the decision and `state_change` by the user, marks the pending row and writes `audit_log`, 409 when already decided, closed or past the deadline; `/api/remediation/events` reads events across incidents newest first, without large outputs (ADR-0045) |
| Production compose | `deploy/docker-compose.yml` runs the server, the web interface behind Caddy, PostgreSQL, VictoriaMetrics and VictoriaLogs on one host; PostgreSQL over TLS verified by name with a CA `db-certs` creates on first start, the two stores over plain HTTP on an `internal` network never published; only Caddy and the agent listeners are published; checked by `deploy/smoke-test.sh` in CI (ADR-0051) |
| Agent releases | A `v*` tag matching `Cargo.toml` publishes static musl Linux binaries (x86_64, aarch64, built with cargo-zigbuild), the Windows exe and MSI, `install.sh`, the unit, the sudo rules and `SHA256SUMS` under fixed names; the POSIX installer checks checksums before any change, writes the manual layout, hands the token to the first start through a root-only `EnvironmentFile` it deletes after enrollment, upgrades in place and can use a self-hosted copy (ADR-0052) |
| Agent self-update | Release `SHA256SUMS` signed with an offline Ed25519 key compiled into the binaries, `VERSION` listed in it, downgrades only when allowed; Linux agents download, then a root `self-update` helper behind an exact sudo rule verifies again, swaps the binary atomically and hands the restart to a transient systemd unit that rolls back when the new agent does not connect in time; Windows through the update service; releases cached by the server and relays on their mirror listeners (ADR-0053) |
