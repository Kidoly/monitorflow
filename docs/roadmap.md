# Roadmap: task breakdown

Each task is sized to be one Claude Code thread on its own branch. Check a task when its PR is merged. Order within a phase matters unless noted.

## Phase 1: Foundations

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

## Phase 2: Monitoring

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

## Phase 3: Relays

- [x] Relay binary: accept agent connections, forward to upstream.
- [x] Store-and-forward buffer on disk with ordering and bounded size.
- [x] Relay chaining and relay enrollment.
- [x] Multi-tenancy: customers, sites, tenancy enforcement in the API.
- [x] Web: administration of relays and sites.

## Phase 4: Packages and updates

- [x] Mirror orchestration: aptly for apt, reposync + createrepo_c for dnf; snapshots.
- [x] Relay mirror sync.
- [x] Agent: rewrite package sources with backup and revert; distribute signing keys.
- [x] Campaigns: model, waves, maintenance windows, stop on failure.
- [x] Agent: apply updates, report packages and reboot-required.
- [x] Web: campaign creation and progress.
- [x] On-demand mirrors (ADR-0142): indexes only, packages fetched on first request and kept under a storage cap, prefetched for campaigns and pending updates, in cascade on relays. Full mirrors stay available.

## Phase 5: AI remediation

- [x] AI provider trait and Claude adapter.
- [x] Context gathering with secret redaction.
- [x] Risk classifier: allow-lists, deny patterns, default to high-impact.
- [x] Incident loop with command and duration limits.
- [x] Approval queue, API and web.
- [x] Append-only audit log.
- [x] Proxmox snapshot integration before change-tier commands.

## Phase 6: Windows agent

Approach in ADR-0047: the same agent binary built for Windows, run as a service under a virtual account.

- [x] Windows agent build, service, enrollment and inventory; Windows section of the install guide; CI job on Windows.
- [x] Windows sensors: CPU, memory, disk, port, services, Event Log, PowerShell scripts (can run alongside the previous task).
- [x] Windows Update inventory and installs through update campaigns (ADR-0049).
- [x] MSI package for the agent: binary, service, account, ACLs, event log source (ADR-0050).

## Phase 7: Packaging

- [x] Production compose for the central server: server and web images, PostgreSQL over TLS, install guide, smoke test in CI (ADR-0051).
- [x] Agent releases: static Linux binaries and the Windows MSI on GitHub Releases, one-line Linux installer that can be self-hosted, install command on the token page, end-to-end test in CI (ADR-0052).
- [x] Agent self-update: signed releases, Linux helper with health check and rollback, Windows through the update service, release cache on the server and relays, update action, rollouts and per-site policy in the web interface (ADR-0053).
- [x] Agent install sources: one-line install from GitHub (default), from the central server's release cache over the web interface's HTTPS with checksums pinned in the command, or from a custom URL; server-wide default and per-command switch on the tokens page, offline `agent-release import`, end-to-end test in `central` mode (ADR-0054).
- [x] Server updates from the web interface: published server, web and updater images whose digests the release signs, an updater container that verifies, backs up PostgreSQL, updates and rolls back, check and update in two clicks, offline path, end-to-end test in CI (ADR-0056).
- [x] Automatic updates: on by default with site overrides, a canary per site then batches, halt on failures, version compatibility (server ≥ relay ≥ agent); relay releases, installer and self-update with rollback, relays updated before their agents (ADR-0055).

## Phase 8: Simplicity

Everything installable in one line, updatable in two clicks, and usable without reading documentation. Findings, journeys and the reasoning behind each task are in `docs/design/simplicity-audit.md`; tasks are in order of impact, and those without a dependency can run in parallel.

- [x] First-run setup in the browser: one-time setup link while no user exists, `/setup/` creates the first administrator, a login page that says so, a default site.
- [x] One-line server installer (`curl … | sudo sh`): installs Docker if missing, no git clone, generates secrets, guesses and confirms the public name, pulls published images, prints the URL and setup link; `--uninstall` (ADR; needs a release with the ADR-0056 images).
- [x] Server and relay offline incidents: a built-in rule on missed heartbeats, counted in the fleet tiles, can be muted per site or server.
- [x] Notifications: email and webhook channels configured in the interface with a test button, for incidents, approvals and failed updates (ADR revising the product non-goal).
- [x] Reusable install command per site: an install key valid for N hosts or days, hostname as the ID, optional approval of new hosts; single-use tokens under Advanced (ADR).
- [x] Editable server address in install commands: a public address setting defaulted to the first server name, picked or edited on the tokens page (the published ports are already used).
- [x] Relay onboarding without typing: Relays › Add relay, server names reported by the relay and confirmed instead of typed (ADR), mirroring as a checkbox, "Add servers behind this relay".
- [x] Edit thresholds and add checks in the interface, per server or per site, in plain words.
- [x] Users, customers and roles in the interface: create customers, invite users by one-time link, grant and revoke roles.
- [x] One-click backups: daily backup of the database and `serverdata` with retention, back up now and download in Settings, `--restore` in the server installer; replaces the unpruned update dumps.
- [x] Update available everywhere: daily background release check with a banner and a notification; release import from the interface for servers without Internet.
- [x] AI setup in the interface: API key, model, test and enable in Settings; `MF_ALLOW_COMMANDS=1` in the agent installer for the shell opt-in; approvals say when AI is off.
- [x] Mirrors that work in the production compose: mirror tools sidecar image, add a mirror from a list of distributions with a size estimate.
- [x] Snapshot retention without a setting, and using a mirror from a server's or a site's page (ADR-0074).
- [x] Plain-language pass: rename jargon on screen, certificate expiry warning only when renewal is late or failed, quiet CLI output, an action button in every empty state.
- [x] Remove a server end to end: revoke, hide from lists, show the uninstall command, re-enroll the same host later.
- [x] Agent on any Linux: start and monitor on unknown distributions with package features reported as unavailable.
- [x] Browser certificate without a terminal: ACME when the name is public, otherwise a trust page for the internal CA (ADR, as it is a plaintext listener).
- [x] Settings page for runtime server settings, and a one-screen "Getting started" at the top of the install guides.

## Phase 9: Everyday gaps

Limits already recorded in the ADRs that get in the way of daily use. What the first test on a real host finds is added here.

- [x] Delete a mirror: from the interface, refused or handled while something still uses it, disk space and relay copies reclaimed.
- [x] Show whether each server accepts commands: reported by the agent, unknown for agents that do not report it, used wherever a command is offered.

## Phase 10: Notifications

Reach people outside the web interface. Email and webhook channels shipped in Phase 8 (ADR-0060).

- [x] Slack, Teams and Discord channels.
- [x] Routing per customer and site.
- [x] Silences.
- [x] Alert grouping.
- [x] Escalation.
- [x] PagerDuty-style on-call.

## Phase 11: Windows parity

Bring Windows to the level of Linux after Phase 6. The Windows updates list already shows on a server's page (ADR-0049).

- [x] AI remediation on Windows, where `shell` commands are refused today (ADR-0047).
- [x] The file log sensor on Windows, following file IDs through rotation (ADR-0048).
- [x] The configured update source in the inventory: WSUS URL or Windows Update (ADR-0049).

## Phase 12: Production hardening

Needed before real customers. Daily backups and restore shipped in Phase 8 (ADR-0058).

- [x] High availability for the server: done in Phase 14, as the Enterprise edition (ADR-0100). Several instances on one database are done (ADR-0089).
- [x] Security audit of the whole code base, its fixes released in 0.3.1: signed CA bundles and root-helper commands, least-privilege database role, stored secrets sealed, gated release key, and smaller fixes.
- [x] CA key rotation.
- [x] TLS between the server and VictoriaMetrics and VictoriaLogs (ADR-0051).
- [x] Load test on about 1,000 agents.

## Phase 13: Monitoring depth

Mostly follow-ups that the ADRs postponed.

- [x] journald and log forwarding.
- [x] Disk I/O.
- [x] Proxmox containers (ADR-0044).
- [x] LVM and ZFS snapshots.
- [x] Automatic discovery of Proxmox servers instead of mapping each one by hand (ADR-0044).
- [x] Custom dashboards.

## Phase 14: Commercial

The open questions in `docs/product.md`.

- [x] Subscription tiers and licensing (ADR-0147): one license key per subscription (Basic, Standard, Premium, Enterprise) with the customer, the managed and central servers it covers and its dates, checked offline; Administration › License says the plan, what it includes and the support contact; subscribers follow tested releases, promoted by the release workflow. Nothing ever stops without one.
- [x] Enterprise edition: licensed high availability (ADR-0100). The Enterprise directory `ee/` and its license; an offline license key, checked with a key pair of its own, on an Administration › License page; `install-server.sh --join` to add a central server, which starts only with a license that has room. A missing or expired license stops adding central servers, never monitoring or the ones running.
- [ ] Billing.
- [x] Final product name: MonitorFlow.
- [x] "How to monitor" guides in English and French for the showcase website, to bring search traffic: about 25 subjects from the check catalogue, in `docs/guides/`.

## Phase 15: Your choice

Let each install pick what suits it instead of a fixed choice.

- [x] Any AI for remediation: Anthropic, OpenAI or any OpenAI-compatible API, and local models (Ollama, llama.cpp, vLLM) so nothing leaves the network. The safety rules stay on the server whatever the model.
- [x] Choose how often each check runs, from seconds to hours, per check and as a default per site. Agents already accept any interval up to 24 hours; the web interface only shows it.
- [x] Turn AI commands on and off from the web interface instead of on each server. Turning off always works. Turning on works only for servers installed with "Allow AI commands to be turned on from the web interface", so a compromised central server cannot give itself a shell on servers that did not agree. Administrators only, audited, and shown plainly on the server's page.
- [x] Show dashboards on a wall screen or TV: full screen without menus, large text, refreshing by itself, cycling through several dashboards, and a display link that needs no sign-in, read-only, tied to one dashboard and revocable. Say plainly on screen when the data is stale.

## Phase 16: Integrations

Fit into the tools people already use, for a single company and for MSPs alike (see "Who it is for" in `docs/product.md`).

- [x] A clean public API: versioned, documented with OpenAPI, API keys with scopes limited to a customer or a site, pagination and filters, stable errors, idempotent writes and rate limits. Everything the web interface does is possible through it.
- [x] Keep the API complete, and prove it: a test that fails when a route the web interface uses is missing from the public API or from its OpenAPI document, with a short, justified list of what stays for people only (signing in, one's own preferences and password, making API keys). Close today's gaps: undocumented routes, actions only a global administrator can do and no key can reach, and anything the web interface does in more than one call that a script should be able to do in one. From then on a feature is done only when the API and its documentation come with it.
- [x] Move from another monitoring tool: read its configuration (an export file or its API) and create the matching servers, checks, thresholds, silences and notification channels, starting with the most common open-source and commercial tools and the usual open formats for alerting rules and scrape targets. A preview first shows what maps exactly, what is approximated and what cannot be moved, and nothing is created until it is confirmed. Servers then only need the agent; the old tool can keep running side by side until the switch. Done for PRTG, Zabbix and Prometheus (ADR-0146, ADR-0163).
- [x] Outgoing events for integrations: signed webhooks for incidents, acknowledgements, servers and updates, retried and replayable.
- [x] PSA connection, HaloPSA first, first version (ADR-0164, ADR-0166): the connection and its test, sites matched to HaloPSA clients and sites, a ticket per incident with the chosen type, category and statuses, closed with a note when the incident is resolved.
- [ ] PSA connection, the rest of ADR-0164: the ticket status kept in step from HaloPSA back to MonitorFlow, servers synced as assets, customers matched as well as sites, a priority map, silences holding tickets, and HaloPSA's rate limits.
- [x] Plugins: add checks, notification channels and integrations without changing MonitorFlow, signed, isolated from the server's secrets, and installable from the web interface.
- [x] MSP features out of sight for a single company: no customer picker or customer wording until a second customer exists.
- [x] Sign in through your own identity provider: OpenID Connect with any standards-compliant provider, self-hosted ones included (authorization code flow with PKCE), and LDAP directories over TLS only (LDAPS, or StartTLS). Roles come from the provider's groups, mapped once in the web interface, per customer and site for MSPs; accounts are created at first sign-in. Multi-factor authentication stays with the provider, and the password form is replaced by a "Sign in with ..." button for those accounts (ADR-0141). One local administrator keeps a password, so a provider outage never locks everyone out. Configured from the web interface, audited, and tested with a "Test connection" button before it is turned on.

## Phase 17: Beyond the agent

What people moving from another monitoring tool cannot do without today (the "cannot be moved" lists of the importer): watch what cannot run the agent, check a website the way a visitor sees it, and silence exactly what is under maintenance. Each item also makes the importer map what it now covers exactly.

- [x] Network checks from a probe (ADR-0148): ping (answer, round-trip time, packet loss) and TCP or UDP ports of any address, run by the central server or by a relay of the site, so a network problem between two places is seen. Addresses without an agent are added as devices, with their own page, alerts and dashboards.
- [x] SNMP devices (ADR-0149): switches, routers, firewalls, printers, UPSs and NAS watched over SNMP v2c and v3 from a relay or the central server, with what the common MIBs say (uptime, interfaces up or down, traffic and errors, processor, memory, storage, temperature and battery where they exist), added from a template per kind of device. Servers' network interface traffic is measured by the agent too.
- [x] Website checks (ADR-0150): the status code, text expected or not expected in the page, response time, redirects and the certificate (valid, matching the name, days left), from the central server, a relay or a server's agent.
- [x] Precise and recurring silences (ADR-0151): silence one check instead of a whole server, and recurring maintenance windows (weekly or monthly, in a time zone) that need no renewing.

## Phase 18: Catching up

What established monitoring tools still do better once Phase 17 is done: alerts that understand causes and trends, finding what to watch without being told, proving availability to customers, and virtual machines on other hypervisors. The first six items can start now; the last four come after them.

- [x] Dependencies (ADR-0152): a server, device or check can depend on another one (a switch, a firewall, a relay, a host). While the parent is down, what depends on it opens no incident of its own and shows as unreachable, and the parent's incident lists what it hides. This generalises the relay rule of ADR-0080, and the importer maps other tools' dependencies exactly.
- [x] Richer alert rules (ADR-0153): a rule on a value over a window (average, minimum, maximum, change, rate), a comparison with the same time last week, a forecast from the trend ("the disk will be full in 3 days"), "no data" per check, and conditions combined with "and". The simple threshold stays the default, and the richer forms are offered in the same editor. The importer maps more alerting rules exactly.
- [x] Network discovery (ADR-0154): scan an address range from the central server or a relay (ping, common ports, SNMP) and propose what it finds, servers to install the agent on and devices with their matching template. Nothing is added until it is confirmed. A scan can run on a schedule and say what is new.
- [x] Availability reports (ADR-0155): availability of servers, devices, checks and business services (a group of checks with its own rule, all of them or at least N) over a month or any period, maintenance windows excluded, against an SLA target per service. Reports are sent by email on a schedule, as PDF and CSV, per customer for MSPs.
- [x] VMware and Hyper-V (ADR-0156): hosts, virtual machines and datastores of vCenter or ESXi through their API, and Hyper-V hosts through the Windows agent, with their state, resources and snapshots, without an agent in each virtual machine. Virtual machines are matched to the servers already monitored.
- [x] Custom SNMP checks (ADR-0157): watch any OID, not only the common MIBs: up to 10 channels per check, each a gauge, a rate or a text with its name, unit, scaling and limits; lookups that name enumerated values and their states; tables whose rows appear by themselves; Test and Browse from the probe, and MIB uploads to name objects. SNMPv1 joins v2c and v3. The importer moves custom SNMP sensors of PRTG, Zabbix SNMP items and Prometheus snmp_exporter modules.
- [x] Ready-made application checks (ADR-0183): PostgreSQL, MySQL and MariaDB, Redis and Docker as built-in check templates instead of scripts, each with its metrics (connections, replication and lag, slow queries, locks, sizes, hit ratios, memory and evictions, containers' state, restarts and health) and default alerts, run by the agent or a probe with a sealed read-only account. The importer maps the equivalent PRTG, Zabbix and Prometheus sensors.
- [ ] SQL Server check (ADR-0183): connections, batch requests, page life expectancy, blocked processes, database and log sizes and Always On replicas, from the Windows agent or a probe, with `VIEW SERVER STATE` only.
- [ ] Java (JMX) check (ADR-0183): heap, non-heap, garbage collection and threads, then Tomcat and Kafka groups, read through Jolokia from the agent or a probe.
- [ ] Kubernetes check (ADR-0183): a cluster as a device read through its API with a read-only token: nodes and their conditions, pods not running or restarting, deployments short of ready replicas, pending volume claims.
- [x] Cloud providers (ADR-0183): state and basic metrics (processor, network, disks, status checks) of AWS EC2 and Azure virtual machines through their APIs with read-only credentials sealed per account, read by the central server and matched to the servers already monitored.
- [ ] Google Cloud virtual machines (ADR-0183): the same for Compute Engine through Cloud Monitoring, with a service account key.
- [x] Traffic flows (ADR-0184): NetFlow v5 and v9, IPFIX and sFlow v5 received by a relay of the site or the central server, added up each minute on the probe (largest addresses, applications, protocols, interfaces and conversations), kept 48 hours by minute and 31 days by hour. A Flows tab on the device's page with the period, a stacked chart and a click on an address to filter, an alert on any address above a rate for some minutes, and PRTG's flow sensors recognized by the importer.
- [x] Network maps: drawn from discovery and dependencies, with live states, editable by hand and usable on a wall screen.
- [x] A mobile web app with push notifications (ADR-0186): installed on the phone's home screen, notifications by Web Push through the phone's own push service, acknowledge and silence from a phone, who is on call, a long session per phone that its person can remove.
- [ ] Native applications in the App Store and Google Play around the same pages, with an optional push relay for native notifications (ADR-0186).

## Phase 19: Keeping servers current

Updates within a release are done (Phase 4). Moving a server to the next release of its distribution is the job people put off for years, because it is long, risky and done by hand on each server.

- [x] Major distribution upgrades, Debian 12 (bookworm) to Debian 13 (trixie): move servers to the next release of their distribution in waves like a campaign. An explicit action with warnings, a snapshot recommended before, and nothing done on a server that did not agree to it (ADR-0182).
- [ ] Major distribution upgrades, Ubuntu LTS to the next LTS, on the same command, unit, checks and campaigns (ADR-0182).
- [ ] Major distribution upgrades, RHEL 8 to 9 and 9 to 10 with its derivatives (Rocky, AlmaLinux) through leapp and ELevate (ADR-0182).

  Design note, now ADR-0182:

  - **Per distribution.** Debian: point every source at the new codename (both the classic `sources.list` lines and the deb822 `.sources` files, adding `non-free-firmware` where `non-free` was used), then `apt-get update`, `apt-get upgrade --without-new-pkgs`, `apt-get full-upgrade`, non-interactive with the configuration files kept as they are, then a reboot. Ubuntu: its own `do-release-upgrade` in non-interactive mode, LTS to the next LTS only, once that LTS offers the upgrade (after its first point release). RHEL and derivatives: `leapp preupgrade`, whose report blocks the upgrade on any inhibitor and is shown as it is, then `leapp upgrade` and the reboot into its upgrade environment; Rocky and AlmaLinux through the ELevate data for leapp. One release at a time: bookworm to trixie, never bullseye to trixie in one step. Other distributions, and Windows feature updates, stay out until asked for.
  - **Explicit.** Global or customer administrators only, never from a rule or the AI. The server must have been installed with "Allow major upgrades from the web interface" (an agent setting, like AI commands, so a compromised central server cannot start one), and the confirmation names the source and target releases and the number of servers. Audited like campaigns, and the commands are signed and classified high impact.
  - **Checks before.** Shown per server before anything starts, and run again on the agent just before: the current release fully updated, no held or broken packages, enough free space on `/`, `/var` and `/boot`, no reboot pending, third-party sources listed (they are switched off during the upgrade, then left off with a warning), an agent version that knows the target release, and the target release available on the mirror when the server uses one (ADR-0142), mirrored ahead like a campaign's packages.
  - **Snapshot recommended.** A Proxmox (ADR-0044) or local (ADR-0094) snapshot is taken first where possible; where none is possible, the screen says so plainly ("Rollback limited: / is on plain ext4") and the administrator ticks that they accept it. Until the package step starts, a failure puts the sources back as they were (ADR-0032); after it, only the snapshot goes back.
  - **Waves.** The campaign model (ADR-0033): a pilot wave first, maintenance windows, stop on the first failure, one server at a time by default. A server is done once it rebooted, its agent reports the new release and its checks are back to what they were before.

## Phase 20: Managing servers, not only watching them

What people otherwise keep a separate remote management tool for. Servers first: workstations come only if asked for. Every action that changes a server follows the rule of AI commands and major upgrades: the server must have agreed to it at install, so a compromised central server cannot give itself control of servers that did not.

- [ ] AI repairs that need root: a short catalogue of exact root actions (clearing the package cache, shrinking the journal, pruning Docker, restarting a service) run by the agent's root helper from signed commands, the safe ones at once and the others after approval, never a free root command. The AI writes its diagnosis in the user's language.
- [x] Remote access: open a terminal (Linux and Windows) on a server from its page, through the agent's existing connection and its relay, with no port to open and no separate tool to install. Allowed per server at install, limited to chosen roles, audited (who, when, how long), and optionally recorded. A message on the server says a session is open (ADR-0190).
- [ ] Remote desktop (Windows): the host's own RDP tunnelled through the agent's connection to a web RDP client, with its own consent at install, off by default, and the same roles, audit and idle time as terminals (plan in ADR-0190).
- [x] Script library and automation rules: scripts kept in MonitorFlow (shell and PowerShell) with their parameters, versions and the role allowed to run them, run on one server, a group or a whole site, now or on a schedule, with the output of each server kept. Automation rules run a script when something happens (an incident opens, a check fails, a server joins), with the same approvals and limits as AI commands. The AI can use library scripts as ready-made fixes.
- [x] Software deployment and Windows and third-party updates, Windows: a software catalogue (winget packages, shared or per customer), installed on and removed from servers by campaigns in batches and windows, only where the server's administrator allowed it at install (`SOFTWARE_FROM_WEB=1`). Windows Update is driven from campaigns: approve updates by classification, hold some back, in maintenance windows, with reboots; third-party apps updated by winget the same way, with held apps (ADR-0188).
- [x] Software deployment, the rest (ADR-0188): Linux packages from the catalogue (apt and dnf) through a root helper with one exact sudo line, packages from the central server's mirrors added for the campaign, campaigns that run again by themselves at each window, and turning software installs off from the server's page (turning them on again stays on the server).
- [ ] MSI packages on Windows (ADR-0188, "Still to come"): an uploaded MSI file in the catalogue with its SHA-256 and ProductCode, served through relays and the central server, checked by the update service before `msiexec`.
- [x] Hardware and software inventory (ADR-0187): what each server is made of (model, serial number, processor, memory, disks, network cards, firmware and BIOS, warranty typed by people since makers give it only through keyed services) and what it runs (installed software and versions, services, Windows features, licences), searchable across the fleet, with changes kept over time and exported. The PSA asset sync is left to the PSA task, with its hook ready (`docs/halopsa.md`).
