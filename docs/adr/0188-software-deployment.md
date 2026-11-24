# ADR-0188: Software deployment and update approvals

Date: 2026-11-24
Status: accepted; Windows (Windows Update approvals and winget) delivered, Linux and MSI packages described below and still to come

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

**On Linux** the agent registers `software` as unsupported for now: "software installs are not supported on Linux yet".

### Consent on the server

Installing and removing software needs the host's consent, on the model of ADR-0098: **`commands.software`** in `agent.toml`, off by default. Only the host's administrator sets it: the MSI property `SOFTWARE_FROM_WEB=1` at install or upgrade, or `monitorflow-agent.exe software on|off` later. The agent's account cannot write `agent.toml` (ADR-0050), and no command, signed or not, can set it.

It is checked twice: the agent reports `software` as accepted or off in `Hello` (ADR-0076) and refuses the command when off; and the update service, which does the install as LocalSystem, reads `commands.software` from its own copy of `agent.toml` at each `software` step and refuses without it. The second check is the one that holds against the agent's account (ADR-0108), and it makes `software off` effective at once, before the agent restarts.

The server reads the report: a campaign gives a server that has not allowed it no command and an error naming the line to run, an agent that predates this decision an error saying it updates by itself, and a single command is refused with 409.

**Default: off, and the Add servers checkbox unticked.** The AI commands checkbox is ticked by default (ADR-0175) because AI commands run as the agent's own unprivileged account and still go through risk rules and approvals. Installing software runs an installer of the publisher's choosing as LocalSystem: full control of the host. With the consent, a central server that is taken over could install any package of the server's winget sources; without it, it can only update what is installed, as before. That trade is the host administrator's to make, knowingly, per server. Existing installs never agreed and keep refusing until their administrator runs `software on`. The checkbox "Allow installing and removing software from the web interface" adds `SOFTWARE_FROM_WEB=1` to the Windows command; the Linux command will get `MF_SOFTWARE_FROM_WEB=1` with the Linux part.

There is no switch on the server's page to turn software installs off from the web interface, unlike AI commands (ADR-0098): the consent is the only setting, and it lives on the host. A switch that only takes the power away may come later.

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

## Still to come

The roadmap line stays open for these, in this order:

1. **Linux packages.** Catalogue entries of manager `apt` or `dnf`, names in the distributions' grammar (`common::command::valid_package_name`). A root helper `monitorflow-agent packages software`, one more exact sudo line in `deploy/sudoers/monitorflow-agent` (`monitorflow ALL=(root) NOPASSWD: /usr/bin/monitorflow-agent packages software`), reads the signed `software` command on stdin, checks it against its own anchor (ADR-0108) and its consent in `agent.toml`, and runs fixed command lines: `apt-get -q -y install <names>` and `apt-get -q -y remove <names>` with `DEBIAN_FRONTEND=noninteractive` and the options of ADR-0036, `dnf -q -y install|remove <names>`. Never `purge`, `autoremove` or a file path. Consent: `MF_SOFTWARE_FROM_WEB=1` with the installer, as a systemd drop-in like ADR-0098's.
2. **Linux repositories.** A catalogue entry may need a repository: it names a mirror of the central server (ADR-0142), and a software campaign configures the server's sources first with `configure_sources` (ADR-0035) in `add` mode, as pinned campaigns do (ADR-0038), then restores them. No free repository URL reaches a server: mirrors are where keys and URLs are reviewed.
3. **MSI packages on Windows.** A catalogue entry of manager `msi` holds an uploaded file stored by the central server, its SHA-256 and its ProductCode, and optional public properties from a checked grammar. The signed `software` command carries the URL and the SHA-256; the update service downloads it through the agent's upstream (relay mirror listener or central server), checks the digest, and runs `msiexec /i <file> /qn /norestart` with only those properties, or `msiexec /x {ProductCode} /qn /norestart` to remove it. MSI files carry no GPG signature, so the digest in the signed command is what protects them on plain HTTP listeners.
4. **Recurring campaigns**: a campaign that runs again by itself at each window, for Windows updates and third-party apps kept up to date without anyone pressing "Run again" (ADR-0173). Today a campaign is one pass, run again by hand or through the API.
5. **Turning software installs off from the server's page**, and the server's page showing whether software is allowed.

## Manual test

On a Windows Server 2025 virtual machine with the agent installed by the MSI with `SOFTWARE_FROM_WEB=1`, PowerShell 7 and the winget module (ADR-0139):

1. `agent.toml` has `software = true` under `[commands]`; the server reports `software` as accepted.
2. Add 7-Zip (`7zip.7zip`, source `winget`) to the catalogue. A campaign "Install software" of it, one batch, reboot allowed: the server's command output has `7zip.7zip succeeded: 7-Zip`, the next report lists the app, and `update.log` shows the step `software`.
3. Run the campaign again: `7zip.7zip is installed already`, the command succeeds.
4. A campaign removing it: `succeeded`, the app is gone from the report; again: `is not installed`.
5. `software off` as administrator, no restart: a new campaign's command fails with the update service's refusal; after `Restart-Service MonitorFlowAgent`, the campaign skips the server with "software installs are not allowed on this server".
6. An update campaign approving `Definition Updates` only and holding one pending KB: only definition updates install, and the output says the KB is held.

## Consequences

- One campaign screen and one API deploy software, update Windows with approvals and holds, update third-party apps and agents.
- A server installs software only where its administrator allowed it; existing servers do not until someone runs `software on` there. A compromised central server can install packages only on those servers, and only what their winget sources offer.
- Approvals and holds reach only agents of this version or later; older Windows agents in a campaign that uses them are skipped until they update themselves, rather than install what is held.
- Linux servers are not targets of software campaigns yet. A Linux agent refuses approvals and holds it cannot apply rather than ignore them; the server never sends it any.
- `servers.command_kinds` gains `software`. The catalogue is one more table to back up with the database.
