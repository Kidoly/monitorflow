# ADR-0182: Major distribution upgrades

Date: 2026-11-23
Status: accepted; Debian 12 to 13 implemented, Ubuntu and RHEL and its derivatives to come; extends ADR-0033 (campaigns), ADR-0094 (local snapshots) and ADR-0108 (root helpers verify signed commands)

## Context

Updates within a release are done (Phase 4). Moving a server to the next release of its distribution is the job people put off for years: it is long, it can leave a server that does not boot, and it is done by hand on each server. The roadmap's design note for Phase 19 asks for it to be an explicit action, with checks before, a snapshot recommended, and waves like a campaign, and for nothing to happen on a server whose administrator did not agree to it.

The agent runs as an unprivileged user and reaches root only through exact sudo lines to helpers of its own binary, which check the command the central server signed (ADR-0032, ADR-0036, ADR-0094, ADR-0108). A major upgrade outlives the agent: the package step restarts services, and the last step reboots the machine. A self-update already survives its agent's restart with a transient systemd unit and a state file root writes and everyone reads (ADR-0053).

## Decision

### Per distribution

**Debian 12 (bookworm) to Debian 13 (trixie)**, the first one, in this order:

1. **Sources.** Every source of the release is pointed at the new codename, in both formats: the one-line entries of `/etc/apt/sources.list` and `sources.list.d/*.list`, and the deb822 stanzas of `sources.list.d/*.sources`. A suite is the release's when it is the codename or the codename followed by `-updates`, `-security`, `-backports` or `-proposed-updates`; `stable` and its suffixes stay as they are, since they already follow the release (and point at trixie once it is stable). Where an entry of the release uses `non-free` and not `non-free-firmware`, `non-free-firmware` is added after it, as Debian 12's release notes ask. The MonitorFlow mirror file `monitorflow.sources` (ADR-0074) is rewritten too: its paths name the catalog's mirrors, so `apt/debian-12…/current/` becomes `apt/debian-13…/current/`. An entry of any other suite is **third-party**: it is switched off (commented out with a `# Disabled by MonitorFlow for the upgrade to Debian 13:` line above it, or `Enabled: no` in its stanza) and left off afterwards, since its packages were built for the old release. The page lists them beforehand and the result says so again.
2. `apt-get update`. A warning or error line fails it, as for the post-check of ADR-0032.
3. `apt-get upgrade --without-new-pkgs`, then `apt-get full-upgrade`, as the release notes say: non-interactive (`DEBIAN_FRONTEND=noninteractive`, `APT_LISTCHANGES_FRONTEND=none`), keeping every changed configuration file (`--force-confdef --force-confold`).
4. A reboot.

**Ubuntu LTS to the next LTS**, not done yet: Ubuntu's own `do-release-upgrade -f DistUpgradeViewNonInteractive`, LTS to the next LTS only, and only once that LTS offers the upgrade (after its first point release, when `do-release-upgrade -c` finds it). Its tool handles sources, third-party entries and the reboot prompt itself; MonitorFlow then runs it the way the Debian steps run, in the same unit, with the same state file.

**RHEL 8 to 9 and 9 to 10**, not done yet: `leapp preupgrade`, whose report blocks the upgrade on any inhibitor and is shown as it is on the server's page, then `leapp upgrade` and the reboot into its upgrade environment. Rocky Linux and AlmaLinux go through the ELevate data for leapp. The checks below apply, with `dnf` in place of apt.

**One release at a time**: bookworm to trixie, never bullseye to trixie in one step. The agent carries the table of upgrades it knows (`common::major_upgrade`); a pair it does not know is refused by the agent and by the server. Other distributions, and Windows feature updates, stay out until asked for.

### Explicit, and agreed to on the server

**Consent on the host, like AI commands (ADR-0098).** `packages.major_upgrades` in `agent.toml`, off by default, is what "Allow major upgrades from the web interface" sets: `MF_MAJOR_UPGRADES_FROM_WEB=1` with the Linux installer (`0` takes it back, and it works on an installed agent), or `monitorflow-agent major-upgrades on|off` as root. The file is `root:monitorflow 0640`, so the agent's user cannot grant it, and the root helper reads it from the file itself, never from the environment or the caller: a compromised central server, or the agent's user, cannot start an upgrade on a server that did not agree. The agent reports the kind `major_upgrade` as accepted or refused in `Hello` (ADR-0076), so the server knows before trying. The Add servers page has the checkbox, off by default.

**Who.** Only global administrators and administrators of the customer, never a site administrator, a rule, an automation or the AI: a major upgrade is only ever a campaign (below), and campaigns are created and started by those people only (ADR-0033). The AI's command catalogue (ADR-0181 and later) never includes it.

**The confirmation** names the source and target releases and the number of servers: "Upgrade 3 servers from Debian 12 (bookworm) to Debian 13 (trixie)?". It is the start of the campaign, audited as every campaign action is (`campaign.create`, `campaign.start`, the runner's `campaign.queue`), and each server's command is signed (ADR-0031) and checked again by the helper (ADR-0108). The kind is classified high impact, like a reboot, in the command list and the audit log.

### Checks before

The agent works them out with each package report, as its own user, and sends them in the report's new `major_upgrade` part, with the next release it knows. The page shows them per server before anything starts; the root helper runs the same code again just before the upgrade and refuses on any failed one.

| Check | Fails when |
| --- | --- |
| `up_to_date` | packages of the current release have updates pending |
| `held_packages` | `apt-mark showhold` lists packages |
| `broken_packages` | `dpkg --audit` reports half-installed or unconfigured packages |
| `free_space` | `/` has under 2 GiB free, `/var` under 4 GiB, `/boot` under 300 MiB; volumes on one filesystem add up |
| `reboot_pending` | `/var/run/reboot-required` exists |
| `sources` | no source of the release was found, a source file cannot be read, or a campaign pinned the mirror sources to a named snapshot |
| `third_party_sources` | never: a warning listing the entries that will be switched off and left off |

The server adds its own, from what it knows:

| Check | Fails when |
| --- | --- |
| `allowed` | the agent does not report `major_upgrade` accepted: no consent, or sudo rules without the helper's line (the page gives the line to run) |
| `agent_version` | the agent sends no `major_upgrade` report or does not know the target: an older agent, which updates by itself |
| `mirror` | the server uses the mirrors of its distribution (ADR-0074) and the target's (`debian-13` and its updates and security mirrors) are not added and published. Mirrors are on-demand (ADR-0142): packages are fetched on the first request and verified against the signed index, so the indexes being there is what makes the target available |

### Snapshot recommended

The campaign takes a snapshot of each server before its upgrade, in the order of ADR-0094: a **Proxmox** snapshot of a mapped guest while `[proxmox]` is on (ADR-0044), else a **local** LVM or ZFS snapshot when the system volumes allow one. Proxmox snapshots, left out of package campaigns because the API call blocks for minutes, are taken by a task the runner starts after its transaction, recorded on the server's row of the campaign (`vm_snapshot_state`), and asked again after 20 minutes without an answer, as a take of the same name answers with the snapshot already there. A failed snapshot fails that server without upgrading it.

**Where none is possible, the screen says so plainly**, "Rollback limited: / is on plain ext4", per server, and the campaign needs `accept_limited_rollback`, the box "I accept that these servers cannot be rolled back by a snapshot". Without it, creating the campaign is refused with the servers concerned, and a server whose snapshot became impossible since then fails rather than being upgraded.

**Until the package step starts, a failure puts the sources back** as they were: the helper keeps a copy of every source file it changes in `/var/lib/monitorflow/major-upgrade/backup/` (root, 0700) before writing, as ADR-0032 does, and restores them, then runs `apt-get update` once more. After `apt-get upgrade` started, nothing is undone by MonitorFlow: only the snapshot goes back, by hand, with the instructions of ADR-0094.

### The helper and the unit

`sudo -n /usr/bin/monitorflow-agent major-upgrade`, one more exact line in `deploy/sudoers/monitorflow-agent`, reads the signed `major_upgrade` command on stdin and checks, in order: the command (ADR-0108), the consent in `agent.toml`, that the payload's `from` is the release `/etc/os-release` names and its `to` the next one the table knows, that no upgrade is running, and the checks above. It then writes the state, `/var/lib/monitorflow/major-upgrade/state.json` (directory root 0755, file root 0644), and starts the work in a transient unit, `systemd-run --unit monitorflow-major-upgrade --collect`, running `monitorflow-agent major-upgrade run`. The unit lives outside the agent's cgroup, so restarting the agent, or apt restarting it, does not stop it. `run` takes no input but the state file, which only root writes, and refuses unless it runs as root.

The state file holds the command ID, the releases, the phase (`starting`, `sources`, `refreshing`, `upgrading`, `full_upgrade`, `rebooting`, then `done` or `failed`), the error, whether the sources were put back, the third-party entries switched off, the boot ID when the reboot was asked, and times. The full output goes to `/var/lib/monitorflow/major-upgrade/log` (root, 0600), for whoever logs in to look.

The agent reads the state file, and works out what holds after a reboot, which the unit does not see: `rebooting` on another boot ID with `/etc/os-release` on the target release is `done`; on the old release, `failed`. A phase other than `rebooting` on another boot ID means the machine restarted during the upgrade: `failed`, "interrupted by a restart while upgrading packages". A fresh state file on a later upgrade replaces the old one.

### The command and the report

`major_upgrade`, payload `MajorUpgrade { from, to }` like `"debian 12"` and `"debian 13"`, the form `Hello` reports. Its handler runs the helper and finishes once the unit is started: succeeded means **started**, with the helper's output; a refused check fails it at once with that check. While a state file says an upgrade is not finished, the agent looks at it every 10 seconds and sends its package report again, with the new phase, whenever it changes, keeping the packages of its last report: apt's lists are not trustworthy halfway through an upgrade.

The server keeps the latest `major_upgrade` part of each server's report in `server_major_upgrades` (migration `20261224090000_major_upgrades`), as it keeps storage reports (ADR-0094).

### Waves: campaigns, extended

A campaign gets a third kind besides package updates and agent updates (ADR-0053): `major_upgrade: {from, to, accept_limited_rollback, parallel}`. Everything else is the campaign of ADR-0033: targets frozen at creation, batches with the first as the pilot, maintenance windows, the failure threshold (default: stop at the first failure), pause, stop, edit and run again (ADR-0166, ADR-0173). Targets must all run `from`, or creation is refused with the servers that do not. Within a batch, **one server at a time** by default: `parallel` (1 to 50) says how many may be upgrading at once, so a batch of 10 is upgraded one after the other unless asked otherwise.

For each server the runner queues, in order: the snapshot (Proxmox or local), then `major_upgrade`, recording the checks that are failing on the server at that moment (its open incidents, by metric and instance) in `checks_before`. **A server is done** when:

- its command succeeded (the upgrade started),
- its report says `done`: it rebooted, and its agent reports the target release,
- `Hello` reports the target release too (`servers.os_version`),
- and its checks are back to what they were before: no incident open on it that was not open before the upgrade, five minutes after it came back.

It **fails** when the command fails, the report says `failed` (with its error and whether the sources were put back), its checks are not back 30 minutes after it came back ("checks not back: disk / (warning)"), or six hours pass after the start without it being done. Its row then carries the error, which counts toward the failure threshold. Commands of a major upgrade campaign cannot be cancelled once sent; pausing or stopping it lets the servers upgrading finish.

### API and screens

- `POST /api/campaigns` and `PUT /api/campaigns/{id}` take `major_upgrade`; campaigns return it. `GET /api/major-upgrades` lists, for the servers the user sees (filters `customer_id`, `site_id`, `server_id`), their release, the target, every check with its state and detail, the rollback they would get, and the progress of their last upgrade. Both are in the OpenAPI document.
- **Campaigns › New** has a fourth kind, "Major upgrade", which picks the pair of releases, shows the checks of every server picked, the rollback of each, the box for a limited rollback when one needs it, and "one server at a time" by default.
- **The campaign's page** shows each server's phase ("Upgrading packages", "Rebooting", "Waiting for its checks") and asks for confirmation before starting a major upgrade, naming the releases and the number of servers.
- **The server's Updates tab** shows its major upgrade: the next release, the checks, the rollback, the last upgrade's progress, and "Upgrade to Debian 13", which opens the campaign form with that server, for its administrators.

### Rejected

- **A separate model for upgrades.** Waves, windows, thresholds, pause and audit are what campaigns already are, and an administrator would have two places to look.
- **Running the upgrade inside the command's handler.** The agent's dispatcher drops a handler after an hour and the server fails a command running for two (ADR-0031), and the reboot ends the agent anyway. The command starts it; the report follows it.
- **Rewriting the sources from the server.** The server would need to read the host's source files and send them back, and the host's own helper is where the backup is.
- **`sed s/bookworm/trixie/` on every file.** It also rewrites third-party entries, which then break `apt-get update`, and comments.
- **Turning third-party sources back on after the upgrade.** Their packages were built for the old release; an administrator decides, once their vendor publishes for the new one.
- **Upgrading without a snapshot silently.** A major upgrade is the change most likely to leave a server that does not boot.

## Consequences

- An administrator moves Debian 12 servers to Debian 13 in waves from the web interface, only on servers whose administrator agreed at install, with each server's checks and rollback shown first.
- Hosts installed before this decision need the installer run again for the new sudo line and the consent, and report `major_upgrade` refused until then.
- A Proxmox guest gets a Proxmox snapshot in a major upgrade campaign, which package campaigns still do not take.
- A failure during the package step leaves a server half upgraded until someone rolls back the snapshot or finishes by hand; the log on the server says where it stopped.
- The checks need an agent of this version; the table of known upgrades grows with agent releases, so a new pair needs an agent update first.
- Ubuntu and RHEL use the same command, state file, unit, report and campaign kind; only the steps and the checks differ, and they are added when implemented.
