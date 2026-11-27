# ADR-0182: Major distribution upgrades

Date: 2026-11-23
Status: accepted; Debian 12 to 13, Ubuntu LTS to the next LTS, RHEL 8 to 9 and 9 to 10, Rocky Linux 8 to 9 and AlmaLinux 8 to 9 and 9 to 10 implemented, with Ubuntu and RHEL servers on the mirrors and `leapp preupgrade` alone; extends ADR-0033 (campaigns), ADR-0094 (local snapshots) and ADR-0108 (root helpers verify signed commands)

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

**Ubuntu LTS to the next LTS** (22.04 to 24.04, 24.04 to 26.04): Ubuntu's own `do-release-upgrade -f DistUpgradeViewNonInteractive`, in the same unit, with the same state file and environment as apt (`DEBIAN_FRONTEND=noninteractive`, configuration files kept). LTS to the next LTS only, and only once that LTS offers the upgrade (after its first point release): the check `release_offered` needs `Prompt=lts` in `/etc/update-manager/release-upgrades`, so the tool can never pick an interim release, and `do-release-upgrade -c` to offer the target. The tool rewrites the sources and switches third-party entries off itself; MonitorFlow reads the same sources beforehand to list those entries and to make sure the archive's are there, and writes nothing. Its phases are `upgrading`, then `rebooting`: the non-interactive mode does not reboot, so the unit does. It exits 0 when it found nothing to upgrade to, so the unit reads `/etc/os-release` before the reboot and fails if the server is not on the target.

**RHEL 8 to 9 and 9 to 10, Rocky Linux 8 to 9, AlmaLinux 8 to 9 and 9 to 10**: `leapp preupgrade` (phase `preupgrade`), then `leapp upgrade` (`upgrading`), then the reboot into its upgrade environment (`rebooting`), which upgrades the packages and boots again into the new release; the agent then sees another boot ID and the target in `/etc/os-release`, as for Debian. After each leapp run the unit keeps `/var/log/leapp/leapp-report.txt` in the state file as it is (cut at 64 KiB), and any entry of `leapp-report.json` flagged or grouped `inhibitor` fails the upgrade with their titles; a failed exit fails it too. Neither run changes the installed system, so the error says the server still runs the old release. The report goes up with the progress (`MajorUpgradeProgress.report`) and the server's Updates tab shows it under "leapp's report". Rocky Linux and AlmaLinux go through the ELevate data for leapp (`leapp-data-rocky`, `leapp-data-almalinux`). **Rocky Linux 9 to 10 is left out**: ELevate publishes no `leapp-data-rocky` for el9 (checked 2026-11-24, and again 2026-11-27 in its el9 repositories, stable and testing: only `leapp-data-almalinux` and `leapp-data-centos`), so the agent's table does not have it and both sides refuse it until it does.

**`leapp preupgrade` alone**, before any campaign, from the server's Updates tab: `POST /api/servers/{id}/major-upgrade/preupgrade`, for the same people as a campaign (global administrators and administrators of the customer). It is a command kind of its own, `leapp_preupgrade`, with the `MajorUpgrade` payload of the next release: an agent that predates it refuses the kind, where a flag in `major_upgrade` would have been read as a full upgrade by one. The same helper and sudo line take it (the helper reads the kind from the signed command and accepts these two only), with the same consent; it checks the releases, that no upgrade runs and the `leapp` check alone, since leapp reports the rest itself. It writes a state with `preupgrade_only` and starts the same unit, which runs `leapp preupgrade` and ends `done` (no inhibitor) or `failed` (the inhibitors' titles), keeping the report, and never upgrades or reboots. The report goes up as any progress, with `preupgrade_only` (`MajorUpgradeProgress.preupgrade_only`), and the tab shows it under "Last leapp preupgrade". A campaign never takes it for its own upgrade's progress. The agent reports `leapp_preupgrade` accepted on the RHEL family where it accepts `major_upgrade`, unsupported elsewhere, and the server lists `can_preupgrade` per server. Not high impact: nothing on the server changes.

**One release at a time**: bookworm to trixie, never bullseye to trixie in one step. The agent carries the table of upgrades it knows (`common::major_upgrade`); a pair it does not know is refused by the agent and by the server. Other distributions, and Windows feature updates, stay out until asked for.

### Explicit, and agreed to on the server

**Consent on the host, like AI commands (ADR-0098).** `packages.major_upgrades` in `agent.toml`, off by default, is what "Allow major upgrades from the web interface" sets: `MF_MAJOR_UPGRADES_FROM_WEB=1` with the Linux installer (`0` takes it back, and it works on an installed agent), or `monitorflow-agent major-upgrades on|off` as root. The file is `root:monitorflow 0640`, so the agent's user cannot grant it, and the root helper reads it from the file itself, never from the environment or the caller: a compromised central server, or the agent's user, cannot start an upgrade on a server that did not agree. The agent reports the kind `major_upgrade` as accepted or refused in `Hello` (ADR-0076), so the server knows before trying. The Add servers page has the checkbox, off by default.

**Who.** Only global administrators and administrators of the customer, never a site administrator, a rule, an automation or the AI: a major upgrade is only ever a campaign (below), and campaigns are created and started by those people only (ADR-0033). The AI's command catalogue (ADR-0181 and later) never includes it.

**The confirmation** names the source and target releases and the number of servers: "Upgrade 3 servers from Debian 12 (bookworm) to Debian 13 (trixie)?". It is the start of the campaign, audited as every campaign action is (`campaign.create`, `campaign.start`, the runner's `campaign.queue`), and each server's command is signed (ADR-0031) and checked again by the helper (ADR-0108). The kind is classified high impact, like a reboot, in the command list and the audit log.

### Checks before

The agent works them out with each package report, as its own user, and sends them in the report's new `major_upgrade` part, with the next release it knows. The page shows them per server before anything starts; the root helper runs the same code again just before the upgrade, after `apt-get update` or `dnf check-update --refresh`, and refuses on any failed one. `monitorflow-agent major-upgrade checks` prints them on the server, as any user. The names are the same on every distribution, in the order of `major_upgrade::checks(method)`; what each reads differs:

| Check | Fails when |
| --- | --- |
| `up_to_date` | packages of the current release have updates pending |
| `held_packages` | `apt-mark showhold` lists packages |
| `broken_packages` | `dpkg --audit` reports half-installed or unconfigured packages |
| `free_space` | `/` has under 2 GiB free, `/var` under 4 GiB, `/boot` under 300 MiB; volumes on one filesystem add up |
| `reboot_pending` | `/var/run/reboot-required` exists |
| `sources` | no source of the release was found, a source file cannot be read, or a campaign pinned the mirror sources to a named snapshot |
| `third_party_sources` | never: a warning listing the entries that will be switched off and left off |

On Ubuntu, the same as Debian with the Ubuntu archive (`ubuntu.com` hosts, or a path ending in `ubuntu` or `ubuntu-ports`, but never a PPA on `launchpad.net` or `launchpadcontent.net`) and its components, and:

| Check | Fails when |
| --- | --- |
| `sources` | as on Debian. On the MonitorFlow mirrors, a warning: `do-release-upgrade` would switch `monitorflow.sources` off as third-party and find no archive, so the campaign puts the server's own sources back first (below) |
| `release_offered` | `Prompt` is not `lts`, `do-release-upgrade` is missing, `do-release-upgrade -c` offers nothing (before the first point release) or another release |

On the RHEL family:

| Check | Fails when |
| --- | --- |
| `up_to_date` | `dnf check-update` lists updates |
| `held_packages` | `/etc/dnf/plugins/versionlock.list` has entries |
| `broken_packages` | `dnf check` reports problems |
| `reboot_pending` | `dnf needs-restarting -r` says a reboot is needed |
| `sources` | never fails; a warning on an enabled `monitorflow-*` repository: leapp takes the target's repositories from its own data and the mirror files would stay on the old release, so the campaign puts the server's own repositories back first (below) |
| `leapp` | `leapp-upgrade-el<from>toel<to>` is not installed, or on a derivative its ELevate data; the detail gives the commands |
| `third_party_sources` | never: a warning listing the enabled repositories that are not the distribution's (by host, `redhat.repo` and ELevate's own counting as the distribution's), which leapp does not use |

The server adds its own, from what it knows:

| Check | Fails when |
| --- | --- |
| `allowed` | the agent does not report `major_upgrade` accepted: no consent, or sudo rules without the helper's line (the page gives the line to run) |
| `agent_version` | the agent sends no `major_upgrade` report or does not know the target: an older agent, which updates by itself |
| `mirror` | Debian: the server uses the mirrors of its distribution (ADR-0074) and the target's (`debian-13` and its updates and security mirrors) are not added and published. Mirrors are on-demand (ADR-0142): packages are fetched on the first request and verified against the signed index, so the indexes being there is what makes the target available. Ubuntu and the RHEL family: never fails; a warning when the server uses the mirrors and they do not carry the target, as it then keeps its own sources after the upgrade |

**Ubuntu and RHEL servers on the mirrors** go back to their own sources for the upgrade, as their tools take the next release from the distribution. The campaign does it, not the helper: the helper cannot rebuild the mirror spec of the new release, which the central server signs (ADR-0035). A server of such a campaign whose sources history says mirrors (the last source command that succeeded is a `configure_sources`) gets, before its snapshot, `revert_sources`, the same command as "Revert package sources" on its page; once its part ended, the mirrors back: after an upgrade done, the `configure_sources` that "Use the mirrors" would send for the release it runs now; when it did not upgrade (failed, or the campaign stopped or paused before its upgrade), its previous mirror sources again, the last `configure_sources` that succeeded outside campaigns, as pinned campaigns restore them (ADR-0038). A pause then a resume sends the revert again. The two commands sit in the columns of pinned campaigns (`sources_command_id`, `restore_command_id`), which a major upgrade campaign never has, so the batch waits for the mirrors back, their failure counts as the server's, nothing cancels them, and the server's sources page says a campaign holds them meanwhile. Where the mirrors do not carry the new release, the server keeps its own sources: its revert is recorded as its restore and the audit log says why (`campaign.sources_kept_own`). The helper still refuses to start while a mirror file is enabled, so a server is never upgraded by these tools on the mirrors. A server that reaches only the mirrors (a site without Internet) cannot use them, and fails at the tool's first download with the old mirror sources put back.

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

- `POST /api/campaigns` and `PUT /api/campaigns/{id}` take `major_upgrade`; campaigns return it. `GET /api/major-upgrades` lists, for the servers the user sees (filters `customer_id`, `site_id`, `server_id`), their release, the target, every check with its state and detail, the rollback they would get, the progress of their last upgrade, and `can_preupgrade`. `POST /api/servers/{id}/major-upgrade/preupgrade` queues `leapp_preupgrade` (201 with the command, 409 saying why not). All are in the OpenAPI document.
- **Campaigns › New** has a fourth kind, "Major upgrade", which picks the pair of releases, shows the checks of every server picked, the rollback of each, the box for a limited rollback when one needs it, and "one server at a time" by default.
- **The campaign's page** shows each server's phase ("Upgrading packages", "Rebooting", "Waiting for its checks") and asks for confirmation before starting a major upgrade, naming the releases and the number of servers.
- **The server's Updates tab** shows its major upgrade: the next release, the checks, the rollback, the last upgrade's progress, and "Upgrade to Debian 13", which opens the campaign form with that server, for its administrators. On the RHEL family, "Run leapp preupgrade" too, after a confirmation, hidden while an upgrade or a preupgrade runs; its report shows open under "Last leapp preupgrade".

### Rejected

- **A separate model for upgrades.** Waves, windows, thresholds, pause and audit are what campaigns already are, and an administrator would have two places to look.
- **Running the upgrade inside the command's handler.** The agent's dispatcher drops a handler after an hour and the server fails a command running for two (ADR-0031), and the reboot ends the agent anyway. The command starts it; the report follows it.
- **Rewriting the sources from the server.** The server would need to read the host's source files and send them back, and the host's own helper is where the backup is.
- **`sed s/bookworm/trixie/` on every file.** It also rewrites third-party entries, which then break `apt-get update`, and comments.
- **Turning third-party sources back on after the upgrade.** Their packages were built for the old release; an administrator decides, once their vendor publishes for the new one.
- **Installing leapp and the ELevate data from the helper.** It would add a repository and packages from outside the distribution as root on the strength of the upgrade's consent alone; the check gives the two commands instead, and an administrator runs them once.
- **Running `leapp preupgrade` with each package report.** It needs root and minutes; the agent's checks read only what an unprivileged user can, and the report comes from the unit's first phase, or from `leapp_preupgrade` when someone asks for it.
- **A `preupgrade_only` flag in the `major_upgrade` payload.** An agent that predates it ignores the field and would run the whole upgrade.
- **The helper putting the mirrors back after the upgrade.** It would need the new release's signed spec, which only the central server builds; and on the RHEL family the upgrade ends in leapp's environment, after the unit is gone.
- **Rewriting the Ubuntu sources ourselves, as for Debian.** `do-release-upgrade` also handles the packages that change name, the quirks of each release and its own rollback of the sources; doing it twice would fight it.
- **Upgrading without a snapshot silently.** A major upgrade is the change most likely to leave a server that does not boot.

## Consequences

- An administrator moves Debian 12 servers to Debian 13 in waves from the web interface, only on servers whose administrator agreed at install, with each server's checks and rollback shown first.
- Hosts installed before this decision need the installer run again for the new sudo line and the consent, and report `major_upgrade` refused until then.
- A Proxmox guest gets a Proxmox snapshot in a major upgrade campaign, which package campaigns still do not take.
- A failure during the package step leaves a server half upgraded until someone rolls back the snapshot or finishes by hand; the log on the server says where it stopped.
- The checks need an agent of this version; the table of known upgrades grows with agent releases, so a new pair needs an agent update first.
- Ubuntu and the RHEL family use the same command, sudo line, consent, state file, unit, report and campaign kind as Debian; only the steps and the checks differ. No new privilege: the helper still runs only the steps of the pair the signed command names.
- Ubuntu and RHEL servers on the MonitorFlow mirrors go back to their own sources for a major upgrade, put back by the campaign, and to the mirrors of the new release after it where the mirrors carry it. They must reach their distribution's own repositories meanwhile.
- `leapp preupgrade` runs alone from the server's page, so an administrator reads leapp's report and fixes its inhibitors before planning the campaign.
- leapp and the ELevate data are installed by hand once per server before its first upgrade.
