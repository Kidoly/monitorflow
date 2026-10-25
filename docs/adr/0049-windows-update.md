# ADR-0049: Windows Update

Date: 2026-10-30
Status: accepted

## Context

ADR-0047 brought the agent to Windows and refused `apply_updates`, `configure_sources` and `revert_sources` there. It sent no `PackageReport`, and update campaigns failed a wave on a Windows target. Phase 6 asks for Windows Update inventory and installs through the campaigns that Linux servers already use (architecture §7, ADR-0031, ADR-0033).

Constraints:

- The agent runs as `NT SERVICE\MonitorFlowAgent`, a virtual account without administrator rights (ADR-0047). Installing updates needs administrator rights. Following ADR-0036, privileged work goes through a separate component that accepts typed requests and makes the calls itself. A general elevation path is ruled out.
- Windows servers get their updates from Windows Update, Microsoft Update or WSUS, as Group Policy or Windows Update for Business configured them. MonitorFlow's mirrors serve apt and dnf only.
- Protocol changes are additive. Campaigns already express "all", "security only" and "these packages", with an optional reboot (`ApplyUpdates`).
- A concurrent task builds the MSI. This ADR names what it must install.

## Decision

### Update API

The Windows Update Agent COM API (`IUpdateSession`, `IUpdateSearcher`, `IUpdateDownloader`, `IUpdateInstaller`, `ISystemInformation`), called through the `windows` crate. It is the supported interface behind the Settings page, `UsoClient` and WSUS clients. It reports KB numbers, classifications, MSRC severity, sizes and reboot behaviour per update, and installs a chosen set. Rejected alternatives:

- **The PSWindowsUpdate module**: a third-party PowerShell module to install, and its text output would have to be parsed.
- **`UsoClient` or `wuauclt`**: undocumented, and neither can install a chosen update or report a result.
- **`Get-HotFix`/WMI `Win32_QuickFixEngineering`**: lists only some installed updates and installs nothing.

Searches use the default server selection, so they follow the machine's configuration: the WSUS server named by policy, otherwise Windows Update, or Microsoft Update when it is registered. MonitorFlow never changes that configuration. WSUS approvals, and the deferrals of Windows Update for Business as far as the scan applies them, decide what is offered, and MonitorFlow installs only offered updates. Automatic Updates keep running on their own schedule. On servers managed by campaigns, administrators should set them to notify or download only (`AUOptions` 2 or 3), so installs happen in campaign windows.

### The privileged component

**Same binary, second service.** `monitorflow-agent.exe update-service` runs as the Windows service `MonitorFlowUpdate` under LocalSystem, the only built-in account that may install updates. It serves one named pipe and makes every Windows Update call itself, searches included. The agent makes no COM call at all. As with ADR-0036, one binary means one file to ship and to protect. `%ProgramFiles%\MonitorFlow` lets only administrators write, as `/usr/bin` lets only root write. Rejected alternatives:

- **A scheduled task running as SYSTEM**: requests and results would have to pass through files, there is no reply channel, and starting the task needs a right that the agent's account would also need for every other task.
- **Granting the agent's account the rights**: no right narrower than administrator covers installing updates, so that gives the agent full control of the host, which ADR-0047 refused.
- **Searching from the agent**: searches work for standard users in a desktop session, but nothing documents them for a virtual service account. An online scan also writes the Windows Update data store through the system service anyway. With every call in one process, the agent needs no COM code and no knowledge of the API.

For the MSI and the install guide:

| What | Value |
| --- | --- |
| Binary | `%ProgramFiles%\MonitorFlow\monitorflow-agent.exe` (the agent's) |
| Service name, display name | `MonitorFlowUpdate`, "MonitorFlow update service" |
| Command line | `"…\monitorflow-agent.exe" update-service` (no configuration file) |
| Account | `LocalSystem` |
| Start type | automatic, with the same restart actions as the agent's service |
| Pipe | `\\.\pipe\MonitorFlowUpdate` |
| Log | `%ProgramData%\MonitorFlow\update\update.log`, 16 MiB then `update.log.1`, like the agent's |

**Pipe.** The service creates the pipe with `FILE_FLAG_FIRST_PIPE_INSTANCE` (if another process already holds the name, the service stops with an error rather than sharing it) and `PIPE_REJECT_REMOTE_CLIENTS`. Its DACL is protected and, in SDDL:

```
D:P(A;;GA;;;SY)(A;;GRGW;;;S-1-5-80-<agent service SID>)
```

SYSTEM has full access, and the agent's service SID may read and write. Administrators, users and the pipe's other clients are not listed, so Windows denies them at open time. The service SID of a virtual account is derived from the service name alone: `S-1-5-80-` followed by the SHA-1 of the upper-case name in UTF-16LE, read as five little-endian 32-bit numbers. The service computes it, so it works before the agent's service exists, and a unit test checks the derivation against `NT SERVICE\TrustedInstaller`'s well-known SID. The agent opens the pipe with `SECURITY_IDENTIFICATION`, tokio's default, so a process that took the name first could not impersonate the agent at a useful level. The worst it could do is lie about updates.

**Protocol.** One request per connection. Each message is a 4-byte big-endian length followed by JSON. Requests are limited to 64 KiB and replies to 16 MiB. Unknown fields are refused, and requests are handled one at a time. The service checks every request again, as it trusts nothing on the pipe.

| Request | Reply |
| --- | --- |
| `{"op": "scan"}` | `{"updates": {"updates": [Update], "reboot_required": bool}}` |
| `{"op": "status"}` | `{"status": {"reboot_required": bool}}` |
| `{"op": "install", "kind": "all" \| "security" \| "named", "updates": [name]}` | `{"installed": {"results": [Result], "reboot_required": bool}}` |
| `{"op": "reboot"}` | `{"rebooting": null}` |
| any, on failure | `{"error": "…"}` |

`Update` has `name`, `title`, `classification`, `severity`, `size_bytes`, `revision`, `installed` and `reboot_required`. `Result` has `name`, `title`, `result` (`succeeded`, `succeeded_with_errors`, `failed`, `aborted`, `not_started`) and `hresult`.

### Inventory

A scan is one search: `IsInstalled=0 and IsHidden=0 and Type='Software' or IsInstalled=1 and Type='Software'`. It returns the pending updates, plus the installed ones that the source still lists, at no extra cost. Drivers are left out: they are not what a patch campaign is for, and WSUS rarely approves them.

Every update becomes a `Package`:

| Field | Value |
| --- | --- |
| `name` | `KB` + its first KB article number, like `KB5034441`; its update ID (a GUID) when it has none |
| `version` | its revision number when installed, else empty |
| `upgradable_version` | its revision number when pending, else empty |
| `arch` | empty |
| `title` (5) | its title |
| `classification` (6) | the English name of its classification (`Security Updates`, `Critical Updates`, `Definition Updates`, `Update Rollups`, `Updates`, `Feature Packs`, `Service Packs`, `Tools`, `Upgrades`), mapped from the classification's well-known GUID because names are localized; else the name Windows gives |
| `severity` (7) | MSRC severity (`Critical`, `Important`, `Moderate`, `Low`), empty when none |
| `size_bytes` (8) | the largest download size |
| `reboot_required` (9) | pending: installing it may need a restart (its reboot behaviour is not "never reboots"); installed: it waits for one |

The new fields are additive and stay empty on Linux. Several updates can share a KB number (one per .NET version, for example). They become one entry: pending wins over installed, sizes add up, and the first title is kept. A name then installs all of them. `PackageReport.reboot_required` comes from `ISystemInformation.RebootRequired`.

On the server, migration `20261030090000_windows_updates` adds nullable `title`, `classification`, `severity` and `size_bytes` columns and a `reboot_required` column (false by default) to `server_packages`. `GET /api/servers/{id}/packages` returns them. Revision numbers are versions in Windows Update's sense, so "upgradable" keeps its meaning: a pending update has an `upgradable_version`, an installed one only a `version`.

The agent collects reports as on Linux: at start, after every `apply_updates`, and every `packages.report_interval`. A collection that fails, as when the update service is not installed or not running yet, is now retried after 5 minutes on both platforms instead of after the full interval.

### Installs

`apply_updates` keeps its payload. On Windows, the agent maps it as ADR-0036 does on Linux: `security_only` gives `security`, a non-empty `packages` gives `named`, and anything else gives `all`. The agent checks the names, and the service checks them again.

- **Names** are `KB` followed by 1 to 10 digits, or an update ID (a GUID), without case. `common::command::valid_windows_update_name` checks them. They are matched against the KB numbers and update IDs of pending updates. A name that matches nothing is listed in the output as "not pending" and is not an error, as when apt skips a package that is not installed.
- **all**: every pending software update except the `Upgrades` classification. Those are feature updates to a new Windows version, which a patch campaign must not start without naming them.
- **security**: pending updates of the `Security Updates` classification (by GUID), among the named ones if any. Critical and definition updates are not security updates in this sense, just as apt's `-security` selection excludes other suites.
- **named**: the pending updates named, `Upgrades` included.

The service accepts the license terms of the selected updates, as unattended installs must, then downloads and installs them with source prompts off. Updates that need user input fail rather than wait. The command's output has one line per update, like `KB5034441 succeeded: 2024-01 Cumulative Update for …`. The command fails when any update did not succeed, and its error names them with their HRESULTs. The agent then sends a report.

**Reboots.** The agent never reboots on its own. A pending reboot is reported in `PackageReport.reboot_required`, and the web interface shows it. When `allow_reboot` is set (the campaign option of ADR-0033, also available on single commands) and Windows then needs a restart, the agent sends `reboot`. The service runs `%SystemRoot%\System32\shutdown.exe /r /t 60 /d p:2:17 /c "MonitorFlow: updates installed"`, a planned restart for a hotfix after one minute, which leaves time for the result and the report to get out. This is the Linux `shutdown -r +1`.

### Campaigns and sources

- Windows servers take part in campaigns like Linux ones. `apply_updates` gets Windows Update semantics on them, and their results count in their wave.
- **Pinned snapshots do not apply to Windows servers.** A campaign that pins mirrors no longer refuses Windows targets for lack of a mirror family. The runner queues their update without `configure_sources` before it or a restore after it. Linux targets still need a mirror of their family.
- **Named updates must fit every target.** A campaign or command naming packages is refused (400) when it targets Windows servers and a name is not a KB number or update ID, or when it targets Linux servers and a name is one. Otherwise one platform's names would fail every server of the other platform.
- `configure_sources` and `revert_sources` stay unsupported on Windows. The API refuses them for Windows servers with 400 "package sources are not supported on windows servers", and the agent still answers them with `REJECTED` if one arrives.

## Consequences

- A Windows server needs the second service for updates and reports. Without it, the agent runs as before and logs every 5 minutes that the update service is unreachable. `apply_updates` fails with that reason. The guide installs the service by hand until the MSI does.
- A compromised agent account can make the host install any update its configured source offers and reboot it, which matches what ADR-0036 allows on Linux. It cannot choose the source, pass options or install anything that is not offered.
- Inventories of Windows servers list installed updates that the source still knows, not the full history. An update whose installation is superseded drops out of the list.
- The service runs searches online. A scan that cannot reach WSUS or Windows Update fails, and no report is sent.
- Killing the agent while an install runs does not stop the service. The install finishes, and the journal reports the command as interrupted (ADR-0031).
- Follow-ups: report the configured update source (WSUS URL or Windows Update) in the inventory, driver updates as an opt-in, and a per-update install timeout.
