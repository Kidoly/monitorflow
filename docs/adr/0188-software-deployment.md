# ADR-0188: Software deployment and update approvals

Date: 2026-11-24
Status: accepted; Windows (Windows Update approvals and winget) delivered first, then Linux packages, packages from mirrors, recurring campaigns and turning software installs off from the server's page (amended 2026-11-25), then MSI packages (amended 2026-11-27)

## Context

Phase 20 of the roadmap asks for "software deployment and Windows and third-party updates": install, update and remove software on servers from a catalogue (winget and MSI on Windows, packages and repositories on Linux), in waves like campaigns; Windows Update driven from MonitorFlow (approve or hold updates by category, maintenance windows, reboots); common third-party applications kept up to date the same way. Its introduction sets the rule for every action that changes a server: the server must have agreed to it at install, so a central server that is taken over cannot give itself control of servers that did not.

Much of it exists already:

- Campaigns (ADR-0033, ADR-0038, ADR-0179, ADR-0167, ADR-0173) send a signed command per server, in batches, inside weekly, monthly or one-off windows, with a failure threshold, editing, runs again and an optional reboot.
- Windows Update installs through the update service `MonitorFlowUpdate`, which runs as LocalSystem and checks every signed command itself (ADR-0049, ADR-0108): all updates but feature upgrades, security updates only, or named KB numbers.
- winget updates the third-party apps a server has (ADR-0138, ADR-0139), in the same service, with PowerShell 7 and the `Microsoft.WinGet.Client` module.
- On Linux, root helpers behind exact sudo lines update packages (ADR-0036), configure package sources (ADR-0032, ADR-0035), and campaigns pin snapshots of on-demand mirrors (ADR-0038, ADR-0142).

What is missing: choosing which Windows updates a campaign installs beyond "all" or "security", holding some back, and installing or removing software at all, which no command does. Installing software is a larger power than updating what is there: a package of the configured winget sources, installed as LocalSystem, can be anything.

The work is too large for one change. This decision covers the whole design and delivers Windows first, end to end: Windows Update approvals and holds, held winget apps, and installing and removing winget packages from a catalogue. Linux and MSI packages follow the same model and are described under "Still to come".

## Decision

### One campaign model

A campaign stays the one way to reach many servers in batches and windows. It does one of three things:

- **Update** (`apply_updates`, ADR-0031), with the approvals and holds below;
- **Update the agents** (`self_update`, ADR-0053);
- **Install or remove software** (`software`, below): new.

Batches, windows (weekly, monthly, yearly, one-off), the failure threshold, pausing, editing, running again and the history of runs apply to all three unchanged. A second model for deployments was rejected: it would duplicate every one of those, and administrators would learn two screens for "do this to these servers, in batches, tonight". "Maintenance windows" of the roadmap line are campaign windows: a command's time to live is capped at the window's end, so nothing starts outside it, and a reboot follows the install by one minute, inside the window.

### Windows Update approvals and holds

`ApplyUpdates` gains two fields (additive, `packages.proto`):

- `classifications` (6): the Windows Update classifications approved, by the English names the agent reports (ADR-0049): `Security Updates`, `Critical Updates`, `Definition Updates`, `Update Rollups`, `Updates`, `Feature Packs`, `Service Packs`, `Tools`, `Upgrades` (`common::software::WINDOWS_CLASSIFICATIONS`). Empty keeps ADR-0049's meaning: every classification but `Upgrades`. They narrow `all` and `security`; `Upgrades`, feature updates to a new Windows version, are installed only when approved by name here or named by KB number, as before. Classifications with named updates (not security only) are refused: the names already say what to install.
- `hold` (7): updates never installed by this command, as KB numbers or update IDs, whatever else selects them, named ones included. A name both named and held is refused.

`ThirdPartySelection` gains `hold` (3): app IDs never updated, compared without case as winget does. winget only for now; the API refuses held Flatpak or Snap apps.

These are the "approve or hold by category" of the roadmap, per campaign. WSUS approvals and Windows Update for Business deferrals still decide what is offered, and MonitorFlow still installs only offered updates (ADR-0049): a WSUS administrator keeps that control. Approving per campaign rather than per server or per site keeps one place that says what a given night installs; a standing policy per site is a follow-up.

**Where the selection happens.** The update service selects among the pending updates it found (`windows_update::select`), from the signed payload only (ADR-0108). Held updates the selection would have taken are listed in the output, `KB5034441 is held`, as not-pending names are. Held apps are left out of winget's selection the same way.

**Older agents and services.** An agent before this decision ignores the new fields and would install what is held. The server sends approvals and holds only to agents that report the `software` command kind in `Hello` (ADR-0076), accepted or not, since both come with this decision; a campaign gives an older Windows agent no command and the error "agent does not know approvals and held updates yet; it updates by itself", and a single command is refused with 400. The update service and the agent are the same binary, but the service runs the old one for about a minute after a self-update (ADR-0108). So an install with approvals or holds is a new step on the pipe, `install_approved`, and a winget update with holds `winget_approved`: an older service refuses an unknown step rather than install everything. Linux servers have no classifications or KB numbers: the server leaves both out of their commands, and a Linux agent refuses an `ApplyUpdates` that still carries any (`SpecError::Approvals`), rather than ignore it.

**Reboots** stay ADR-0049's: the agent never reboots on its own; with `allow_reboot` and a pending restart after the install and the report, the update service runs `shutdown.exe /r /t 60`. Administrators who want MonitorFlow alone to install keep Automatic Updates on notify or download only (`AUOptions` 2 or 3), as ADR-0049 advises.

### The software catalogue

`software_catalog` (migration `20261222090000_software_deployment`) lists what servers may be given: a name, the manager (`winget`), the package ID, the source it installs from (`winget` by default, never the store's `msstore`, ADR-0138) and a description. An entry is **shared** by every customer (`customer_id` null), which only global administrators add, change or remove, or **a customer's own**, which that customer's administrators manage, the people who run campaigns there (ADR-0033). Anyone who sees a customer sees the shared entries and that customer's. A package is listed once per customer and once among shared entries. Changes are audited as `software.create`, `software.update` and `software.delete`.

A campaign or command names entries by ID (`{"action": "install" | "remove", "catalog_ids": [...]}`), shared or of its customer, and **resolves them when it is saved**: the campaign stores the packages (`software` column: action, and per package its catalogue ID, name, manager, ID and source) and keeps them if the entry later changes or goes. A run must install what was reviewed when it was saved, not what someone changed in the catalogue since. Editing a campaign resolves again.

The catalogue rather than free package IDs: an administrator reviews a source and an ID once, and every campaign reuses them; it is also where MSI files and Linux packages will be listed.

### The `software` command

A new command kind, `software`, payload `SoftwareChange { action, manager, packages: [{id, source}], allow_reboot }`. It travels and is checked like every command (ADR-0031): signed for one agent, journaled, refused once expired. `common::software::check_change` is its check, made by the API, by the agent and by the update service: a known action, winget, 1 to 100 packages each once, IDs in winget's grammar (ADR-0138), and for an install a source name of 1 to 64 letters, digits and `._-`, not the store's.

**On Windows**, the agent sends the signed command to the update service as the step `software`; the service checks it against its own trust anchor and runs it once (ADR-0108), from the signed payload alone:

1. It scans winget as for third-party updates (ADR-0139): policy, PowerShell 7 and the module first.
2. An install skips a package winget lists as installed already, from any source (`7zip.7zip is installed already`), and refuses a source Group Policy turns off; a removal skips an app that is not installed. Neither is an error.
3. For each package in turn, one of two fixed scripts runs in PowerShell 7 as LocalSystem, the ID and the source passed as environment variables and read as values, never as PowerShell:
   - install: `Install-WinGetPackage -Id $env:MONITORFLOW_WINGET_ID -Source $env:MONITORFLOW_WINGET_SOURCE -MatchOption Equals -Mode Silent -Scope SystemOrUnknown`: machine-wide where the package says, or as the package installs when it does not say;
   - remove: `Uninstall-WinGetPackage` with the same options but the scope, from the source winget matched the app to.

   `-Force`, `-Override`, `-Custom`, `-Location`, `-Version` and `-AllowHashMismatch` are never passed. Package agreements are accepted as for updates; source agreements never (ADR-0139).
4. Each package gives one line, like updates do: `7zip.7zip succeeded: 7-Zip`, or `failed (0x8A150011): …` with winget's code or the installer's. One failure does not stop the others; the command fails when any failed. 30 minutes per package, as for an update.
5. The agent sends a package report, then asks for the signed restart when the command allows it and a package needs one.

`allow_reboot` of a campaign applies to its software commands. The update service accepts a `reboot` step of a `software` command only when its payload allows it.

**On Linux**, see "Linux packages" below.

### Consent on the server

Installing and removing software needs the host's consent, on the model of ADR-0098: **`commands.software`** in `agent.toml`, off by default. Only the host's administrator sets it: the MSI property `SOFTWARE_FROM_WEB=1` at install or upgrade, or `monitorflow-agent.exe software on|off` later. The agent's account cannot write `agent.toml` (ADR-0050), and no command, signed or not, can set it.

It is checked twice: the agent reports `software` as accepted or off in `Hello` (ADR-0076) and refuses the command when off; and the update service, which does the install as LocalSystem, reads `commands.software` from its own copy of `agent.toml` at each `software` step and refuses without it. The second check is the one that holds against the agent's account (ADR-0108), and it makes `software off` effective at once, before the agent restarts.

The server reads the report: a campaign gives a server that has not allowed it no command and an error naming the line to run, an agent that predates this decision an error saying it updates by itself, and a single command is refused with 409.

**Default: off, and the Add servers checkbox unticked.** The AI commands checkbox is ticked by default (ADR-0175) because AI commands run as the agent's own unprivileged account and still go through risk rules and approvals. Installing software runs an installer of the publisher's choosing as LocalSystem: full control of the host. With the consent, a central server that is taken over could install any package of the server's winget sources; without it, it can only update what is installed, as before. That trade is the host administrator's to make, knowingly, per server. Existing installs never agreed and keep refusing until their administrator runs `software on`. The checkbox "Allow installing and removing software from the web interface" adds `SOFTWARE_FROM_WEB=1` to the Windows command and `MF_SOFTWARE_FROM_WEB=1` to the Linux one.

The server's page can only take that power away (below, "Turning software installs off").

### API

Everything the web interface does is in the public API (`/api/v1`, area `updates`) and its OpenAPI document:

| Method | Path | What |
| --- | --- | --- |
| GET | `/api/software` | The catalogue a user sees; `?customer_id=` keeps the shared entries and that customer's |
| POST | `/api/software` | Add an entry; 409 when the package is listed already |
| PUT, DELETE | `/api/software/{id}` | Edit (its customer stays) or remove an entry |
| POST, PUT | `/api/campaigns`, `/api/campaigns/{id}` | Gain `classifications`, `hold`, `third_party[].hold` and `software` |
| POST | `/api/servers/{id}/commands` | Gains `classifications` and `hold` for `apply_updates`, and the kind `software` |

Command parameters show `classifications`, `hold` and held apps of `apply_updates`, and `{action, manager, packages, allow_reboot}` of `software`.

### Rejected

- **Running winget's command line, or `msiexec`, through AI commands or a script library.** They run as the agent's account, not as LocalSystem, and an AI's output is untrusted text: installs need a typed, signed command and a privileged helper that checks it.
- **Approvals per site or per server, stored once.** Useful as a standing policy, but every campaign would then depend on settings elsewhere that change under it. Campaigns carry their approvals; a policy that pre-fills them is a follow-up.
- **Free package IDs in campaigns.** The catalogue is where an ID and a source are reviewed once.
- **Consent from the central server, or ticked by default.** See above.

## Linux packages

Delivered with the amendment of 2026-11-25.

**Catalogue.** An entry's manager is `winget`, `apt` or `dnf` (migration `20270105090000_software_deployment_rest`). apt and dnf entries name a package of the distribution in its grammar (`common::command::valid_package_name`: letters, digits and `.+_:-`, starting with a letter or digit and ending with one before any trailing `+`), so none is an option, a path, a glob, a group (`@core`) or a removal marker (`nginx-`), and have no source. The same package may be listed for apt and for dnf.

**One campaign, each server its manager.** `PackageManager` gains `APT` (4) and `DNF` (5), the distributions' own managers, never accepted in a third-party selection (`third_party::is_third_party`). `software::manager_for` maps a platform to its manager: winget on Windows, apt on the Debian family, dnf on the RHEL family. A software campaign targets the servers of the families its entries are for, and each server's `software` command carries only the packages of its manager; a server none of them is for gets the error "none of this software is for this server's system". `check_change` refuses a change whose manager is not the platform's, on the API, the agent and the helper alike. So one campaign can install `nginx` on Debian and RHEL servers and `Git.Git` on Windows ones. As before a package update, a campaign takes a local snapshot of a Linux server's system volumes first where they allow one and no Proxmox snapshot applies (ADR-0094).

**The helper.** `sudo -n /usr/bin/monitorflow-agent packages software`, one more exact line in `deploy/sudoers/monitorflow-agent`, reads the signed command on stdin. As root, before anything runs, it checks in order: `commands.software` read from `agent.toml` alone, without the environment, which only root writes; the web interface's "off" (below); the command, with its own trust anchor, for this machine, of kind `software`, unexpired and never run before (ADR-0108); and the change for its own distribution. It then runs fixed lines in the system's environment as sudo resets it:

- apt: `apt-get -q update`, then `apt-get -q -y -o Dpkg::Options::=--force-confdef -o Dpkg::Options::=--force-confold --no-upgrade install <names>`, or the same options with `remove <names>`, with `DEBIAN_FRONTEND=noninteractive`. `--no-upgrade` leaves an installed package at its version: updating it is an update campaign's job.
- dnf: `dnf -q -y install <names>` or `dnf -q -y remove <names>`.

Never `purge`, `autoremove`, a file or a URL. What is installed already (`dpkg-query`, `rpm -qa`) is skipped for an install, and what is not installed for a removal, each with a line, `nginx is installed already`, as on Windows; neither is an error. The packages left go in one transaction, so their dependencies resolve together; a failure fails the command with the tool's code. The agent then sends a package report and, with `allow_reboot`, restarts the host one minute later when it needs it.

**The agent** reports `software` as `accepted`, `off` without the consent or once turned off, or `unsupported` when `sudo -n -l` says the sudo rules predate the helper, as for AI root actions (ADR-0181). Existing servers get the sudo line by running the install command again; until then campaigns skip them with that reason.

**Consent.** `commands.software` in `agent.toml`, as on Windows, written by `sudo monitorflow-agent software on|off` (now on both systems), which the installer runs for `MF_SOFTWARE_FROM_WEB=1` or `0`. This replaces the systemd drop-in this decision first described: the helper reads the file and never the environment, as ADR-0181 and ADR-0182 do, so a drop-in would not have reached it.

## Packages from mirrors

An apt or dnf entry may name a mirror of the central server of its family (`mirror_id`, cleared if the mirror is removed), for software outside the distributions' repositories. Its URL and keys were reviewed when the mirror was added (ADR-0142); no free repository URL reaches a server.

A campaign whose software names mirrors, without `sources`, gets them as `sources` in `add` mode, each pinned to its newest published snapshot when the campaign is saved; editing keeps those pins while the same mirrors are named. The runner then does what pinned update campaigns do (ADR-0038): `configure_sources` adds the mirrors, the `software` command follows once it succeeded, and the server's sources are put back afterwards. A software campaign may give `sources` itself, in `add` mode only: replacing a server's sources to install one package would cut it off from its distribution. As for pinned updates, every Linux server of the campaign must be of a family a pinned mirror covers. A single `software` command naming such an entry is refused: it cannot add the mirror first.

Pinning the snapshot when the campaign is saved, rather than `current`, keeps what was reviewed: a recurring campaign installs from the same snapshot until it is edited. Retention and relays keep the snapshots a completed recurring campaign pins, as they keep those of unfinished campaigns (ADR-0074).

## Recurring campaigns

`campaigns.recurring` (`recurring` in the API): once a run completes, the runner runs the campaign again by itself at the first window that opens after that, exactly as "Run again" with every server does (ADR-0173): the run moves to the history, the targets are planned again, and it is `scheduled`. It is audited as `campaign.relaunch` by `campaign-runner`. "The next window" is one whose end differs from the end of the window open when the run completed (`campaign_window_end`), so a run that completes inside a window does not start again in it. A campaign with commands still in flight or sources to restore waits for the next step.

A recurring campaign needs a weekly, monthly or yearly window; a major upgrade never recurs. A failed or stopped run ends the recurrence until someone runs the campaign again or edits it: a failure is for a person to look at. So does a run that can no longer be planned, every server removed for instance: the runner clears `recurring` and sets the campaign's error, "stopped recurring: …", audited as `campaign.update`. The flag can change at any time, even during a run.

## Turning software installs off

A new command kind, `software_off`, empty payload `SoftwareOff`. `POST /api/servers/{id}/software` with `{"on": false}` queues it for administrators of the server's site, its customer or everything, audited as `server.software_off`; it waits up to seven days for an offline server, and a newer request cancels one still queued. `{"on": true}` is always refused with 409 and the line to run on the server. `GET /api/servers/{id}` gains `software_off_change`, the last such command. The server's page shows whether software installs are allowed and offers "Turn off".

The agent writes `<data_dir>/software-off` and reports `software` as `off` at once, reconnecting so the new `Hello` carries it (ADR-0098). The agent, the Linux helper and the Windows update service each refuse software changes while the file exists, whatever it holds or when it cannot be read. Nothing the central server sends removes it: `monitorflow-agent software on|off` on the host does, and the installer and the MSI property through it. The file is in the agent's own data directory, which its account can write: deleting it gives back exactly what `agent.toml` allows, never more, so the central server still cannot widen a server's rights. The agent reports `software_off` as accepted, so the server knows which agents obey it; older ones get 409.

Unlike AI commands, there is no consent that lets the web interface turn software installs back on: installing software is full control of the host, and a central server that is taken over must not regain it by an "on" after an administrator turned it off.

## MSI packages

Delivered with the amendment of 2026-11-27, in two parts: the agent and its update service first, then the central server, relays and the web interface.

**The catalogue.** An entry of manager `msi` names a Windows Installer file uploaded to the central server. `POST /api/software/files` takes the file itself (`application/octet-stream`), at most 2 GiB, Windows Installer's own limit, reads its `Property` table and answers with its SHA-256, size, ProductCode, ProductName, ProductVersion and Manufacturer. The entry (`POST /api/software`, `manager` `msi`, `file_sha256`) takes the file's ProductCode as its `package_id` unless one is given, and up to 32 public properties (`properties`, `NAME=value`) passed to every install. A file is stored once by its SHA-256 under `<mirror_dir>/software/`, whoever uploads it, and the central server keeps at most 20 GiB of them: an upload beyond is refused with 409. An entry may name a file the user uploaded, or one an entry they see names already, never one they only know the digest of. A file stays while an entry or a campaign names it, and goes 8 days after the last one stopped, longer than a command's longest time to live (7 days), as does an upload nobody added to the catalogue.

**The command.** In the `software` command of a Windows server, an MSI package is a `SoftwarePackage` whose `msi` is set (`packages.proto`): its `id` is the ProductCode, `{XXXXXXXX-XXXX-XXXX-XXXX-XXXXXXXXXXXX}` in upper case, and `msi` holds the file's SHA-256 and size, the URLs to download it from, the properties and the entry's name. A removal carries the ProductCode alone. One command carries winget and MSI packages alike. `common::software::check_change` checks them on the API, the agent and the update service: a ProductCode, no source, a digest of 64 lower-case hex digits, 1 byte to 2 GiB, 1 to 4 http or https URLs, and properties whose names are public (an upper-case letter, then upper-case letters, digits and `_`, up to 64), each once, never one that changes what `msiexec` does (`ACTION`, `ADVERTISE`, `MSIPATCHREMOVE`, `PATCH`, `REBOOT`, `REBOOTPROMPT`, `REINSTALL`, `REINSTALLMODE`, `REMOVE`, `TRANSFORMS`), with values of up to 256 printable ASCII characters but `"` and `;`.

The URLs are `/software/<sha256>.msi` on the mirror listener of the server's relay, when it reported one, then on the central server's (`mirror_public_url`, ADR-0069). A server that has neither gets no command, with the error "MSI packages download from a mirror listener; set the mirror URL of the central server". Agents that know MSI packages report the kind `software_msi`, with the acceptance of `software`; it is never sent as a command. A server whose agent does not gets no command either: "this server's agent is too old to install MSI packages; it updates by itself". An older agent would refuse the command anyway, since a ProductCode is no winget ID.

**Serving the files.** The central server's mirror listener serves `/software/<sha256>.msi` from `<mirror_dir>/software/`, GET and HEAD only, no listing, like the rest of it (ADR-0030). A relay's listener, on a miss, fetches the same path from its upstream's listener, keeps the file in its on-demand store (ADR-0142) only once its SHA-256 matches the name, and serves it; files are evicted least recently used first under `mirror.cache_max_bytes`. These listeners are plain HTTP, and MSI files carry no GPG signature: the digest in the signed command is what protects them. They are not secret either: anyone who reaches a mirror listener and knows a file's SHA-256 can download it, and the URL crosses the network in the clear. The catalogue says so where files are uploaded: an MSI file must hold nothing that the hosts' network may not read, no license key or password in the package or its properties. Properties are in the signed command, not on the listeners.

**The update service.** The agent sends the signed command as the step `software_msi`, after the step `software` when it also has winget packages; a service that predates MSI packages refuses the unknown step. The service checks the command as for winget (ADR-0108), and the consent in its own `agent.toml` and the web interface's "off", then for each package in turn:

1. Asks Windows Installer whether the ProductCode is installed (`MsiQueryProductStateW`): an install of an installed product, or a removal of a missing one, gives the line `{ProductCode} is installed already` or `is not installed`, which is no error.
2. For an install, downloads the file from each URL in turn into `%ProgramData%\MonitorFlow\update\msi\`, which only SYSTEM and Administrators can write (ADR-0050), refusing more bytes than the command says and a redirect, and keeps the first file whose size and SHA-256 match the command's.
3. Opens it without write or delete sharing, so nobody can change it until `msiexec` is done, checks it again through that handle, then runs `%SystemRoot%\System32\msiexec.exe /i "<file>" /qn /norestart /l*v "<log>" NAME="value" ...`, or `msiexec.exe /x {ProductCode} /qn /norestart /l*v "<log>"`, with a log per product next to the files, 30 minutes at most. The file is deleted afterwards.
4. Exit codes 0, 3010 and 1641 succeed, the last two needing a restart; any other fails the package with its code and meaning, like `fatal error during installation (msiexec exit code 1603)`. An install that succeeds without the ProductCode then installed fails ("the file installs another one"): the catalogue's ProductCode is what detection and removal rely on. One failure does not stop the others.

The agent then reports and, with `allow_reboot`, asks for the signed restart, as for winget.

**Rejected.** Free URLs or file shares in the catalogue: the central server would then make every host fetch from wherever an administrator typed, and the digest would be the only review. Serving files through the agent's gRPC stream: it would carry hundreds of megabytes through every relay's command path, where the mirror listeners already carry packages and agent releases. Detecting installs by display name or by the `Uninstall` registry key: the ProductCode is what Windows Installer itself keys products by.

## Manual test

On a Windows Server 2025 virtual machine with the agent installed by the MSI with `SOFTWARE_FROM_WEB=1`, PowerShell 7 and the winget module (ADR-0139):

1. `agent.toml` has `software = true` under `[commands]`; the server reports `software` as accepted.
2. Add 7-Zip (`7zip.7zip`, source `winget`) to the catalogue. A campaign "Install software" of it, one batch, reboot allowed: the server's command output has `7zip.7zip succeeded: 7-Zip`, the next report lists the app, and `update.log` shows the step `software`.
3. Run the campaign again: `7zip.7zip is installed already`, the command succeeds.
4. A campaign removing it: `succeeded`, the app is gone from the report; again: `is not installed`.
5. `software off` as administrator, no restart: a new campaign's command fails with the update service's refusal; after `Restart-Service MonitorFlowAgent`, the campaign skips the server with "software installs are not allowed on this server".
6. An update campaign approving `Definition Updates` only and holding one pending KB: only definition updates install, and the output says the KB is held.

An MSI test, on the same machine, the central server with `mirror_public_url` set:

1. Upload a small MSI to the catalogue: its ProductCode and ProductName are read from the file. Add a property its installer knows.
2. A campaign installing it: the output has `{ProductCode} succeeded: <name>`, the product is in Programs and Features with the property applied, and `update\msi\` holds its log but no file. Again: `is installed already`.
3. Behind a relay with a mirror URL, the relay's access log shows the file fetched once, then served from its store.
4. A campaign removing it: `succeeded`, the product is gone; again: `is not installed`.

A Linux test, on Debian 13 and Rocky Linux 9 with the agent installed with `MF_SOFTWARE_FROM_WEB=1`:

1. `agent.toml` has `software = true`; `sudo -u monitorflow sudo -n -l /usr/bin/monitorflow-agent packages software` succeeds; the server reports `software` as accepted.
2. Entries `nginx` for apt and for dnf; one campaign over both servers installs it on each with its manager; a second run says `nginx is installed already`.
3. "Turn off" on the server's page: the next campaign skips the server; `software-off` is in the data directory; `sudo monitorflow-agent software on` and a restart allow it again.
4. A recurring campaign with a daily window runs again the next day by itself.

## Consequences

- One campaign screen and one API deploy software, update Windows with approvals and holds, update third-party apps and agents.
- A server installs software only where its administrator allowed it; existing servers do not until someone runs `software on` there. A compromised central server can install packages only on those servers, and only what their winget sources offer.
- Approvals and holds reach only agents of this version or later; older Windows agents in a campaign that uses them are skipped until they update themselves, rather than install what is held.
- Linux servers take software of the catalogue with apt or dnf where their administrator allowed it, from their own sources or a reviewed mirror. A compromised central server can install there only packages those sources offer. A Linux agent refuses approvals and holds it cannot apply rather than ignore them; the server never sends it any.
- Administrators can take the right away from the server's page; giving it back always needs the host.
- Campaigns can keep software installed and servers updated without anyone pressing "Run again".
- `servers.command_kinds` gains `software_off`; `deploy/sudoers/monitorflow-agent` gains one line, which existing servers get by running the install command again.
- `servers.command_kinds` gains `software`. The catalogue is one more table to back up with the database.
- MSI packages install only where `commands.software` allows software, from files whose digest the signed command fixes; a compromised central server can install there any MSI it uploads, as it could any winget package. `servers.command_kinds` gains `software_msi`. `<mirror_dir>/software/` is one more directory to back up, at most 20 GiB, and the files on it are readable by anyone who reaches a mirror listener and knows their digest.
