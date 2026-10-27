# Simplicity audit

Status: 2026-10-27. Audit and plan only; nothing here changes the product.

**Goal.** A person who has never heard of mTLS, Docker volumes or TOML should be able to do everything with MonitorFlow without reading a document:

- install the central server, a relay or an agent with one line each;
- update the server in two clicks;
- let agents and relays update themselves;
- learn the rest from the interface.

This document walks every journey from zero the way such a person would. It counts what the journeys cost today, ranks what blocks them, and proposes Phase 8, "Simplicity", as roadmap tasks of one thread each.

**Method.**

- Read `main` at `572fe68`: docs, compose file, installer, web interface, server CLI.
- Read the two in-flight stacks as designed:
  - ADR-0055, automatic agent and relay updates and the relay installer: `phase7/relay-self-update`, `phase7/relay-installer`, `phase7/relay-auto-update`.
  - ADR-0056, two-click server update: `phase7/server-update-*`.
- Installed the server exactly as `docs/deploy-server.md` says, in a fresh copy of the repository. Separate project name and ports (18443, 17443, 17445).
- Clicked through every page of the web interface in a browser and took screenshots.
- Enrolled an agent (the published 0.1.0 binary), then stopped it.

The screenshots are in [`simplicity-audit/`](simplicity-audit/).

## Summary

The product is careful and secure. It is still an expert's tool:

- **Installing the server** takes about 12 steps across two terminals and a browser. Along the way the user makes three configuration decisions and needs two documentation lookups.
- **The first login is a dead end.** The login page cannot be used until someone runs a CLI command that only the docs mention.
- **One agent install takes about 9 interactions.** It starts with a detour through Sites, and every host needs its own single-use token.
- **Many tasks have no screen at all:**
  - tuning a threshold;
  - adding a check;
  - creating a user or a customer;
  - adding a mirror;
  - enabling AI;
  - restoring a backup.

  Each of these needs the CLI, the REST API or a hand-edited file.
- **Monitoring can fail silently.** A server that goes down opens no incident, and nothing notifies anyone. A novice will believe they are monitored when they are not.

The in-flight stacks fix the server update (two clicks after the first install) and make agent and relay upgrades automatic. They leave four things for Phase 8:

- the first install, which still means `git clone` plus a local build;
- the relay install command in the interface;
- release imports when the server cannot reach GitHub;
- restoring after a failed update.

## Findings ranked by impact

**Blocker**: a novice cannot finish the journey without documentation or a terminal on the server. **Major**: they finish, but slowly, confused, or with a wrong result. **Minor**: polish.

### Blockers

**B1. Installing the server is a developer workflow.**

`docs/deploy-server.md` requires all of the following:

- Docker Engine with the compose and buildx plugins;
- `git clone`;
- `cp .env.example .env` and `chmod 600`;
- editing three values in `.env` (`DOMAIN`, `SERVER_NAMES`, `POSTGRES_PASSWORD` generated with `openssl rand -hex 24`);
- `docker compose … up -d --build --wait`, which compiles Rust and Next.js for several minutes.

On the audit machine Docker had compose but not buildx. The documented command then failed after 2 seconds with:

> `the --mount option requires BuildKit. Refer to https://docs.docker.com/go/buildkit/`

That message gives a novice no clue that a plugin is missing. With buildx installed and a warm cache, the build took 2.5 minutes.

`SERVER_NAMES` (the names agents use, which go into a certificate) is jargon that a novice cannot answer.

ADR-0056 does not change any of this for the first install. Only updates switch to published images.

**B2. No way to create the first administrator from the browser.**

After `up`, the URL shows a sign-in form. Every attempt answers "Wrong username or password. Check both and try again." (`02-login-no-admin.png`). Nothing says that no account exists yet.

The only path is `docker compose -f deploy/docker-compose.yml exec server monitorflow-server user add --username admin --role admin`. Its useful line ("created user admin") comes after three lines of JSON logs.

**B3. Certificate warning on first visit.**

With an IP address or an internal name in `DOMAIN`, which is the normal case for a homelab or a LAN, the first page a novice sees is Chrome's full-page "Your connection is not private" (`00-cert-warning.png`). The documented fix is to:

1. copy Caddy's root certificate out of the container with `docker compose exec web cat …`;
2. import it into every browser.

**B4. A server that goes down raises nothing.**

The audit stopped the agent. The fleet then showed "Online 0/1" next to "Critical incidents 0", and the Incidents page said "No incidents. Thresholds have not fired yet." (`36-fleet-offline.png`, `37-incidents-after-offline.png`).

ADR-0017 leaves "host down" as a follow-up. `docs/product.md` lists notification channels (email, webhook, chat) as a non-goal of the first version. So an incident, when there is one, only shows if someone has the page open. For a monitoring product this is the expectation a novice takes for granted.

**B5. Thresholds and checks cannot be changed in the interface.**

The sensor page shows the threshold, duration and hysteresis as read-only facts (`35-sensor-page.png`). There is no edit button, and no "add a check" (port, service, log pattern, script) anywhere. The API has every route (`/api/servers/{id}/thresholds`, `/api/sites/{id}/sensor_configs`, …), but the web client calls none of them.

The defaults (CPU, memory, disk) are good, so monitoring works out of the box. The first "warn me at 70 % instead" fails, though.

**B6. Users, customers and roles have no screen.**

Admin › Users and roles says: "Create users and grant roles on the server with `monitorflow-server user add` and `monitorflow-server user grant`" (`14-users.png`). Customers exist only through `monitorflow-server customer add`. An MSP cannot onboard a customer or a colleague from the browser.

**B7. Mirrors cannot work in the production compose.**

The Mirrors page says "Add one on the server host with `monitorflow-server mirror add`" (`18-mirrors.png`). `deploy-server.md` § Limits adds that the server image has no `aptly`, `reposync` or `createrepo_c`. A novice who follows the interface hits a feature that cannot work on the supported install.

**B8. AI remediation is configured in files on two kinds of hosts.**

Enabling it means three things:

- write an API key file;
- mount it with a custom `server.toml` through a `docker-compose.override.yml`;
- set `[commands] shell = true` in `/etc/monitorflow/agent.toml` on every host that may run commands, by hand.

Risk rules exist only in the API. The Approvals page never says whether AI is on (`19-approvals.png` shows "Nothing waiting for approval" whether it is on or off). Proxmox snapshots need another TOML section and secret files.

**B9. Relays have no install path on `main`, and no command in the interface on the branch.**

On `main`:

- release 0.1.0 publishes no relay binary;
- there is no relay doc, no relay unit and no relay installer;
- a relay token shows only a `relay.toml` snippet.

ADR-0055's `install.sh` adds `MF_ROLE=relay`, which is good. But its doc still creates the token on the CLI, and the tokens page still prints no relay command. The user must also work out three things alone:

- `--server-name`, the names agents use to reach the relay, which go into its certificate;
- `MF_MIRROR_URL`;
- `MF_ENROLL` for the relay's agents.

### Major

**M1. Adding the first agent starts with a detour.**

1. Fleet › "Enroll agent" lands on the tokens page.
2. A fresh install has no site, so the page says "No site to enroll into yet. Create one under Sites first." That sentence is plain text, not a link (`11-tokens.png`).
3. Sites warns that a site name is permanent and labels every metric: "Choose names that will still make sense in two years." That is a hard decision for a first-time user.
4. Back on the tokens page, the form needs a "Peer ID", a site, an expiry and a role, then a source for the download.

The command then appears below a TOML snippet and a "This token will not be shown again" warning (`23-token-created.png`).

**M2. One token per host, typed name, single use, at most 7 days.**

Installing 30 servers means creating 30 tokens and typing 30 names. The name typed as "Peer ID" (`audit-laptop`) is not the name the fleet shows (the hostname, `THE-ONE`). It appears again as "Certificate ID" on the server page, so one machine has two names.

The Ansible recipe in `deploy-agent.md` works around this with `docker compose exec` on the server host. That is an expert's escape hatch, not a fix.

**M3. Install commands ignore the published ports.**

The tokens page builds `MF_SERVER=localhost:7443` from the server's internal listener (`web/src/lib/install.ts`, `crates/server/src/api/tokens.rs`). The audit stack published gRPC on 17443 (`GRPC_PORT` in `.env`), so the copied command would not connect. The same happens behind NAT or a port mapping. Nothing in the interface lets the user correct the address.

**M4. A healthy new server shows a warning.**

Agent certificates last 7 days and renew themselves (by design). The fleet nevertheless shows "Warning · Certificate expires in 7 days" on a server enrolled seconds earlier (`30-fleet-one.png`), while the Warnings tile says 0. A novice will try to "fix" it.

**M5. The agent refuses to start on an unsupported distribution.**

On a host outside the Debian and RHEL families it exits with "unsupported distribution `arch` … the agent supports the debian and rhel families" (`crates/agent/src/os.rs`), instead of monitoring with package features off. Examples are Arch, Alpine, SUSE, or anything without an `ID_LIKE` the agent knows. A novice's first test machine is often such a host.

**M6. Backups and restore are manual.**

The doc says "Back up `serverdata`", without saying how to back up a Docker volume, and gives a one-off `pg_dump` command. No restore procedure exists on `main`. Nothing schedules, rotates or tests backups.

On the ADR-0056 branch:

- each update makes a dump, but dumps are never pruned;
- restoring after a failed migration takes five `docker compose exec` commands.

**M7. Agent releases for offline sites are CLI-only.**

The tokens page offers "This server" as a download source and then says "import one with `monitorflow-server agent-release import --dir DIR`". That needs every release asset downloaded by hand and copied into the container. ADR-0056 imports the release's agents during a server update, which covers connected servers. Offline servers still need the CLI, plus `docker save`/`docker load` for the images.

**M8. No update check runs in the background.**

ADR-0056 updates when an administrator clicks "Check for updates". ADR-0055 notes that a background release check is not built. Nobody learns that a security release exists unless they look.

**M9. Jargon on everyday screens.**

The words a novice meets on the first day:

| Where | Words |
| --- | --- |
| Tokens page | Peer ID, enrollment, upstream_url, enroll_url, Server names |
| Server page | Revision 0 in sync, Certificate ID |
| Sensor page | Instance, Hysteresis, Origin: Built-in default · revision 0 |
| Fleet | "Debian debian 12" |
| Docs | mTLS, gRPC, SANs, ADR numbers in almost every paragraph |

**M10. Removing a server is "Revoke", and it is final.**

The dialog says: "This cannot be undone: enrolling it again needs a new ID" (`38-revoke.png`). Problems with it:

- Revoked servers stay in the fleet list.
- There is no "Remove" that also hides the host.
- Neither the dialog nor the page shows the command that uninstalls the agent (`MF_UNINSTALL=1`, `msiexec /x`). Those live only in the docs.

**M11. Server-side settings live in files.**

Changing any of these means editing `.env` or mounting a `server.toml` with a compose override:

- retention;
- the mirror public URL;
- the agent download URL;
- the AI model;
- the Proxmox clusters;
- session lifetimes.

Only the default install source and per-site agent updates have a screen.

### Minor

- **m1.** `http://<host>:<HTTP_PORT>/` redirects to `https://<host>/` and drops a non-default HTTPS port.
- **m2.** CLI subcommands (`user add`, `token create`) print the server's full JSON startup logs before their one line of output.
- **m3.** The Updates tab of a server says "Updates for THE-ONE are applied through update campaigns for now." It does not say whether updates are pending.
- **m4.** Empty states that state a fact without a next step:
  - Campaigns: "A campaign updates a site…" (there is a button in the header, but not in the empty state);
  - Incidents: "Thresholds have not fired yet.";
  - Approvals: does not say whether AI is enabled.
- **m5.** The Windows command downloads an unsigned MSI ("unknown publisher" prompt in interactive installs).

## Journeys, step by step

A **step** is one command to run, one form field to fill, one decision to make, or one switch between server terminal, host terminal and browser. A **doc** is a place the user must read documentation to succeed. "Target" is the count after Phase 8.

### a. Install the central server

| # | Today (`main`) | Doc? |
| --- | --- | --- |
| 1 | Install Docker Engine, compose plugin and buildx plugin | yes, requirements |
| 2 | `git clone` and `cd` | yes |
| 3 | `cp deploy/.env.example deploy/.env && chmod 600` | yes |
| 4 | Decide `DOMAIN` (DNS name or IP; ACME needs ports 80 and 443 open to the internet) | yes |
| 5 | Decide `SERVER_NAMES` (names agents use, in a certificate) | yes |
| 6 | Generate `POSTGRES_PASSWORD` with `openssl rand -hex 24` | yes |
| 7 | `docker compose -f deploy/docker-compose.yml up -d --build --wait`, minutes of compiling | yes |
| 8 | `docker compose … exec server monitorflow-server user add …`, password twice | yes |
| 9 | Open `https://<DOMAIN>/` | |
| 10 | Get past the certificate warning: export Caddy's root and import it in each browser | yes |
| 11 | Sign in | |

That is 11 steps, 3 decisions and 8 doc lookups. The ADR-0056 branch keeps all of them.

**Target: 2 steps.**

1. `curl -fsSL https://get.monitorflow.dev | sudo sh`. Once a domain is chosen, the URL is a placeholder; until then the server installer is a GitHub release asset.
2. Open the printed link and choose the admin password.

### b. First login and orientation

**Today: 2 dead ends.**

- Before step 8 above, the login page cannot be used and does not say why.
- After it, the empty fleet's "Enroll agent" leads to "Create one under Sites first", which is not a link.

**Target: 0 dead ends.**

- A first-run screen creates the admin.
- A checklist (add a server, set up alerts, invite a colleague) stays on the fleet page until done.
- A default site exists from the start.

### c. Add the first agent

| # | Today | Doc? |
| --- | --- | --- |
| 1 | Fleet › Enroll agent | |
| 2 | Tokens page: no site. Go to Sites | |
| 3 | Choose a permanent site name, create it | |
| 4 | Back to Enrollment tokens | |
| 5 | Choose Agent and type a Peer ID | |
| 6 | Choose site and expiry, Create token | |
| 7 | Choose the download source (GitHub, This server, Custom) and maybe a relay | |
| 8 | Copy the Linux or Windows command | |
| 9 | Run it on the host as root, or in an elevated PowerShell | |

That is 9 steps per host, and steps 5–9 repeat for every host. There are 0 doc lookups, unless:

- the host is behind a relay (then `MF_ENROLL`);
- the host is offline (then a release import);
- the host is not Debian or RHEL (refused, M5).

**Target: 2 steps.**

1. "Add servers" shows one command for the site, valid for N hosts or N days. It can be revoked.
2. Paste it on each host. The host appears live on the same screen.

### d. Add a relay and the agents behind it

**Today, on `main`: not possible without building from source.** There is no relay binary, unit or doc.

**On the ADR-0055 branch: 6 steps plus 3 decisions.**

1. Create a relay token, on the CLI per the doc, or in the interface with "Server names".
2. Read `deploy-relay.md` to build the command.
3. Decide `MF_SERVER`, the server names and `MF_MIRROR_URL`.
4. Run the command.
5. For each agent, enter the relay address on the tokens page.
6. Copy the agent command, which contains `MF_ENROLL`.

**Target: 3 steps.**

1. Relays › Add relay shows a one-line command. It guesses the names from the relay host, and mirroring is a checkbox.
2. Paste it on the relay host.
3. The relay's page shows "Add servers behind this relay" with the site command already pointed at the relay.

### e. See metrics, set a threshold, understand an incident

- **Graphs.** Seeing graphs takes 2 clicks (fleet › server › sensor). Good.
- **Thresholds.** Changing a threshold is impossible in the interface. Today it takes an API call: find the server's UUID, POST JSON with the CSRF header, about 5 steps and the API docs.
- **Server down.** A server that goes down shows "Offline" in the list only. There is no incident and no notification.

**Target:**

- An "Edit" button on every threshold, for this server or for the whole site.
- An "Add a check" button on the server page.
- "Server offline" as a default incident.
- Email or webhook notification, set up once with a "Send test" button.

### f. Package updates, campaigns, mirrors

- **Campaigns** have a full form. It is usable, but its vocabulary is dense: waves, failure threshold, maintenance windows.
- **Mirrors** cannot be used with the production compose (B7).
- **Agent self-update:** ADR-0055 makes it automatic and on by default (good).

**Target:**

- Mirrors that work in the compose stack, added from a list of distributions.
- On each server's Updates tab, the pending updates and an "Update now" button that creates a one-server campaign behind the scenes.

### g. AI remediation and approvals

**Today: about 8 steps and 3 doc lookups.**

1. Get an API key.
2. Write it to a file.
3. Write `server.toml` with `[ai] enabled`.
4. Write a `docker-compose.override.yml` mounting both.
5. Recreate the stack.
6. Edit `agent.toml` on every host (`[commands] shell = true`).
7. Restart those agents.
8. Optionally, Proxmox: one more TOML section and secret files.

**Target: 3 steps.**

1. Settings › AI: paste the key, then Test.
2. Choose which servers may run commands: the agent's local opt-in stays, as a flag of the install command or a choice in the installer.
3. Approvals explains what will wait for the user and why.

### h. Customers, sites, users and permissions (MSP)

| Task | Today | Target |
| --- | --- | --- |
| Create a customer | CLI | Form |
| Create a site | Form (good), permanent name | Form; site names become labels that can be renamed (ADR needed) or carry a clearer warning |
| Invite a colleague | CLI `user add`, which needs the password on the server terminal | "Invite" makes a one-time link |
| Grant a role per customer or site | CLI `user grant` | Role picker in the user list |

### i. Maintain

| Task | Today (`main`) | With ADR-0055/0056 | Target |
| --- | --- | --- | --- |
| Update the server | `git pull` and rebuild, 2 commands and minutes of compiling | 2 clicks, manual check | 2 clicks, with an "update available" banner from a daily check |
| Upgrade agents | Re-run the installer per host, or a per-site switch (off by default) | Automatic, on by default | Same |
| Upgrade relays | Not possible | Automatic | Same |
| Back up | `pg_dump` plus "back up `serverdata`" (how?) | A dump before each update, never pruned | Daily automatic backup with retention, and "Download backup" |
| Restore | Not documented | 5 commands | `install.sh --restore <file>` |
| Certificates | Agent and relay certificates renew automatically; browser certificate imported by hand; false warning (M4) | Same | Warning only when renewal fails; a trust page for internal CAs |
| Revoke a host | 2 clicks, final, host stays listed | Same | "Remove server" revokes, hides, and shows the uninstall command |

### j. Uninstall

| What | Today | Target |
| --- | --- | --- |
| Agent | `MF_UNINSTALL=1` (and `MF_PURGE=1`) on Linux, `msiexec /x` on Windows. Documented only, not shown in the interface | Shown when removing the server |
| Relay (branch) | Same variables | Same as agents |
| Server | Not documented: `docker compose down -v` deletes the data, `down` keeps it | `install.sh --uninstall`, which asks whether to keep the data |

### Totals

| Journey | Steps today | Doc lookups today | Target steps |
| --- | --- | --- | --- |
| a. Server install | 11 | 8 | 2 |
| b. First orientation | 2 dead ends | 1 | 0 |
| c. First agent | 9, then 5 per extra host | 0 to 2 | 2, then 1 per extra host |
| d. Relay and its agents | impossible (`main`), 6 (branch) | 2 | 3 |
| e. Threshold and alert | API only, no alert on host down | 2 | 2 clicks |
| f. Mirrors | impossible in compose | 2 | 3 clicks |
| g. AI | about 8 | 3 | 3 |
| h. Customer, user, role | CLI, 3 commands | 1 | 3 forms |
| i. Server update | 2 commands (2 clicks after ADR-0056) | 1 | 2 clicks |
| i. Backup and restore | manual, undocumented restore | 2 | automatic, and 1 command to restore |
| j. Uninstall server | undocumented | 1 | 1 command |

## Proposal: Phase 8 — Simplicity

**Principles.** Each task applies them to the screen it touches:

1. **One line or one click.** Every install is one command copied from the interface or a web page. Every routine change is one form.
2. **Guess, don't ask.** Derive the public name, ports and site from what the machine already knows. Ask only when the guess cannot be checked, and show the guess as the default.
3. **Every empty screen names the next action as a button.** No dead ends, no CLI in empty states.
4. **Plain words first, detail on demand.** Jargon moves behind a "Details" disclosure. The docs become reference, not prerequisites.
5. **Safe by default stays.** mTLS, signatures, the agent-side opt-in for shell commands and the append-only audit are kept. Simplicity comes from automating them, not from removing them.

**Ordering.** The tasks are ordered by impact. Each fits one thread and one PR of about 500 lines or less. Tasks without a dependency can run in parallel.

| # | Task | Fixes | Depends on |
| --- | --- | --- | --- |
| 1 | **First-run setup in the browser.** The server prints a one-time setup link while no user exists. `/setup/` creates the first administrator. The login page says "No account yet: open the setup link printed by the installer" instead of "Wrong password". A default site `main` is created with the default customer. | B2, b | none |
| 2 | **One-line server installer.** A `install-server.sh` release asset (`curl … \| sudo sh`) that installs Docker and compose if missing, downloads the compose bundle (no `git clone`), generates every secret, guesses the public name from the hostname and IP and confirms it when run in a terminal, writes `.env`, pulls the published images, starts the stack, and prints the URL and the setup link. Re-running it repairs or upgrades. `--uninstall` removes the stack, keeping or deleting data on request. Self-hostable, like the agent installer. ADR. | B1, j | ADR-0056 stack (published GHCR images); task 1 |
| 3 | **Server offline incidents.** A built-in rule opens a critical incident when a server or relay misses heartbeats for N minutes, and resolves it on reconnect. It shows in the fleet tiles. Can be muted per site or server. | B4 | none |
| 4 | **Notifications.** Email (SMTP) and webhook channels, set up in Settings with "Send test". Notify on incident open, resolve and escalation, approvals waiting, failed updates and halted rollouts. Secrets stored server-side, never shown again. ADR, which revises the product non-goal. | B4, M8 | task 3 |
| 5 | **Reusable install command per site.** An install key per site, valid for a number of hosts or days and revocable. The host's hostname becomes its ID (deduplicated). New hosts appear at once and can optionally wait for approval. The tokens page shows it first, and single-use tokens move under "Advanced". ADR on the trade-off. | M1, M2, c | none |
| 6 | **Correct and guessable addresses in commands.** Public gRPC and enrollment addresses as settings, defaulted from the published ports (`GRPC_PORT`, `ENROLL_PORT`) and the name of the web interface. Editable on the tokens page. Used by every command. | M3 | none |
| 7 | **Relay one-liner in the interface.** Relays › Add relay shows the `MF_ROLE=relay` command, with server names guessed from the relay host (the installer reports its names and addresses) and mirroring as a checkbox. Each relay row has "Add servers behind this relay". | B9, d | ADR-0055 stack (relay installer); tasks 5 and 6 |
| 8 | **Edit thresholds and add checks.** An "Edit" button on the sensor page, scoped to this server or the whole site, in plain words ("Warn when memory available is below 10 % for 5 minutes"). "Add a check" on the server page for port, service, log pattern and script, with the agent-side limits explained in place. | B5, e | none |
| 9 | **Users, customers and roles in the interface.** Create a customer. Invite a user by a one-time link (they choose their password). Grant and revoke roles per customer or site. Disable a user. Audited. | B6, h | none |
| 10 | **One-click backups.** A daily backup (database dump plus `serverdata`, encrypted at rest) into the backups volume, with retention. "Back up now" and "Download" in Settings. `install-server.sh --restore <file>` restores onto a new host. Replaces ADR-0056's unpruned dumps and its five-command restore. | M6, i | ADR-0056 stack (backups volume); task 2 |
| 11 | **Update available, everywhere.** A daily background release check (server, agents, relays) with a banner for administrators, sent through notifications. Release import from the interface for servers without Internet: upload the release files or point to a URL. | M7, M8 | ADR-0056 and ADR-0055 stacks; task 4 for notifications |
| 12 | **AI setup in the interface.** Settings › AI: paste the key (stored in `serverdata`, redacted everywhere), choose the model, Test, then Enable. The installer flag `MF_ALLOW_COMMANDS=1` sets the agent's `commands.shell` opt-in, and each server shows whether it accepts commands. Approvals and incidents say when AI is off and how to turn it on. Proxmox clusters get the same form treatment in a follow-up. | B8, g | none |
| 13 | **Mirrors that work in the stack.** A mirror sidecar image with aptly, reposync and createrepo_c in the production compose. "Add mirror" from a list of distributions and versions, with the disk size estimate shown before syncing. | B7, f | ADR-0056 images, for publishing the sidecar |
| 14 | **Plain-language pass.** Rename on screen only: Peer ID → Name, Certificate ID → hidden under Details, revision → "configuration up to date", hysteresis and origin → Details. Fix "Debian debian 12". Show certificate expiry only when renewal is late (under 2 days) or failed. Make CLI subcommands log at warn to stderr, so the result line is all the user sees. Rewrite empty states with a button for the next step. | M4, M9, m2, m4 | none |
| 15 | **Remove a server, end to end.** "Remove" revokes, hides the server from lists (kept in the audit and history), and shows the uninstall command for its OS. Re-enrolling the same host with a new token works without choosing a new ID. | M10, j | task 5 for re-enrollment by hostname |
| 16 | **Agent on any Linux.** On an unknown distribution the agent starts, monitors, and reports package features as unavailable, instead of exiting. The server page says why updates are not offered. | M5 | none |
| 17 | **Browser certificate without a terminal.** The installer offers ACME when the name resolves publicly, and Caddy's internal CA otherwise. For the internal CA, a plain-HTTP `/trust/` page on port 80 serves the root certificate and per-OS instructions, and the setup link goes through it. Needs an ADR, because it is another plaintext listener (read-only, public certificate only), next to the ADR-0030/0034 exception. Alternative: print the certificate fingerprint and download path from the installer, if the ADR is refused. | B3 | task 2 |
| 18 | **Settings page and docs diet.** Server settings that are safe to change at runtime (retention, mirror URL, download URL, session lifetimes) get a Settings page, with `.env` still able to lock them. `deploy-server.md` and `deploy-agent.md` start with a one-screen "Getting started" (the two one-liners and the first-run screen). Everything else moves to a reference section. ADR references move to footnotes. | M11, M9 | tasks 2, 5, 7 |

### Dependencies on the in-flight stacks

- **ADR-0056** (#157, #161, #163, #167, #168, #169) must merge first for:
  - **task 2:** the installer pulls published images and needs no build;
  - **task 10:** the backups volume and the updater's dump step;
  - **task 11:** the release check and the agent-release import during updates.
- **ADR-0055** (#158, #159, #162, #166, #170) must merge first for:
  - **task 7:** `MF_ROLE=relay`;
  - **task 11:** automatic agent and relay upgrades, which task 11 only surfaces.

  The relay stack is based on a `main` that predates the ADR-0054 merges and will need a rebase (`deploy/install.sh` differs by about 140 lines).
- **Gaps in those stacks** to keep in mind when reviewing them:
  - no background update check (task 11);
  - no relay command in the interface (task 7);
  - no pruning of update dumps (task 10);
  - automatic updates without maintenance windows. Sites that need windows must turn them off and use campaigns. A follow-up could reuse campaign windows.

### What not to simplify

- **The agent-side opt-in for shell commands** (`commands.shell`) stays a decision made on the host. Task 12 moves it to the installer and does not remove it.
- **Single-use tokens** stay available for strict environments. Task 5 adds reusable keys beside them.
- **No plaintext listener** is added without an ADR (task 17).
- **The audit log** stays append-only. Removed servers stay in it (task 15).
