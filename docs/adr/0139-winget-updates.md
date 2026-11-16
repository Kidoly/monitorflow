# ADR-0139: Third-party updates on Windows through winget

Date: 2026-11-16
Status: accepted; the Windows side of ADR-0138

## Context

ADR-0138 adds third-party apps to package reports and `apply_updates`, with one manager on Windows: winget, the Windows Package Manager. It leaves to this decision which interface to winget the agent uses, where it runs, which sources and installs it touches, and what it reports when winget cannot be used.

Constraints:

- The agent runs as `NT SERVICE\MonitorFlowAgent` without administrator rights (ADR-0047). Machine-wide installs need administrator rights, so they go through the update service `MonitorFlowUpdate`, which runs as LocalSystem, takes typed requests on its pipe and checks the signed command itself (ADR-0049, ADR-0108). No general elevation path.
- winget is an app package (App Installer) registered per user. LocalSystem never has packages registered, and a virtual service account has no profile that would get them either.
- winget's documentation says, in its troubleshooting page, section "System Context": "As packages can be registered for any user except NT AUTHORITY\SYSTEM (aka LocalSystem, aka System), the WinGet CLI is not supported in the system context", and "The Microsoft.WinGet.Client PowerShell module can be used in the system context with applications that are installed machine wide."
- Platforms, from winget's documentation and its repository: winget ships with Windows 10 (1809 and later), Windows 11 and Windows Server 2025, where App Installer is updated through Windows Update. Windows Server 2019 and 2022 can run it only experimentally, without support. It cannot run on Server Core.

## Decision

### Interface: the winget PowerShell module, in PowerShell 7, as LocalSystem

The update service runs winget through the `Microsoft.WinGet.Client` module, the PowerShell front of winget's COM API (`Microsoft.Management.Deployment`). It is the only interface winget documents for the system context. Its source shows why: when the process runs as LocalSystem, the module creates the COM objects in process, from the winget engine it carries in its own folder (`SharedDependencies`), instead of activating the out-of-process server of the App Installer registered for the user. The source also says this in-process mode does not work in Windows PowerShell 5.1, and the module asks for PowerShell 7: the service runs `pwsh.exe`.

- **PowerShell 7** from `%ProgramFiles%\PowerShell\7\pwsh.exe`, the location of its machine-wide installer, never one found on `PATH`. `%ProgramFiles%` comes from the service's own environment, which the agent's account cannot change.
- **The module** from the machine-wide module folder of PowerShell 7, `%ProgramFiles%\PowerShell\Modules`, where `Install-Module -Scope AllUsers` puts it. The service sets `PSModulePath` to that folder alone; PowerShell 7 then adds its own `$PSHOME\Modules` and LocalSystem's user folder, all three writable by administrators only. The newest version present is imported.
- **The process** is started as the script sensor and AI commands start PowerShell (ADR-0048, ADR-0084, `monitorflow_sensors::powershell`): system variables only, `-NoLogo -NoProfile -NonInteractive -ExecutionPolicy Bypass`, no window, stdin closed, in a job object killed at the timeout and after every run. The script is a constant of the binary, passed with `-EncodedCommand`. The only variable parts are a package ID and a source name, passed as environment variables (`MONITORFLOW_WINGET_ID`, `MONITORFLOW_WINGET_SOURCE`, `MONITORFLOW_WINGET_EXCLUDE`) that the script reads as values, after the service checked them against ADR-0138's grammar. Nothing taken from a request or from a catalog is ever parsed as PowerShell.
- **Output** is one JSON object written by the script (`ConvertTo-Json -Compress`), UTF-8 without a byte order mark: no table to parse, no localized headers, no truncated columns.

MonitorFlow never installs PowerShell 7 or the module. Both are an administrator's choice, as WSUS is for Windows Update, and the install guide gives the two commands.

Rejected:

- **The `winget` command line run as LocalSystem**, from the App Installer folder under `%ProgramFiles%\WindowsApps`. It often works and many scripts do it, but winget's documentation says it is not supported in the system context. Its output is a table meant for a console: column headers are localized, names and IDs are cut with an ellipsis to fit the width, and only `winget export` writes JSON, without available versions. The versioned folder must be found by a wildcard on a protected directory, and it changes with every App Installer update.
- **winget's COM API from the agent's own code**, through Rust bindings generated from `Microsoft.Management.Deployment.winmd`. The out-of-process server cannot be activated by LocalSystem; the in-process engine would have to be shipped with the agent, a native DLL of another project in the MSI, with thousands of lines of generated bindings and their `unsafe`, and kept current with winget. The module is that same engine, maintained and updated by winget's own project.
- **Running winget as the agent's account.** It is a virtual service account: no App Installer is registered for it, and it cannot install machine-wide anyway.
- **Windows PowerShell 5.1**, which every server has: the module's in-process mode does not support it.

### When winget is not there

The service reports one of the states of ADR-0138's `PackageManagerStatus`, with its reason in `error`:

| Situation | State | `error` |
| --- | --- | --- |
| Group Policy disables App Installer or winget's command line interfaces (below) | `UNUSABLE` | "winget is turned off by Group Policy" |
| `pwsh.exe` is not at its location | `ABSENT` | "PowerShell 7 is not installed" |
| The module is not in the machine-wide module folder | `ABSENT` | "the Microsoft.WinGet.Client module is not installed for all users" |
| The script fails, times out, or its output does not parse | `UNUSABLE` | its first line |
| It lists the apps | `AVAILABLE` | empty; `version` is the module's version |

`ABSENT` covers every server where nobody set winget up, which is most Windows Server 2016, 2019 and 2022 machines and every Server Core one. The service checks the files and the policy before starting PowerShell, so a server without winget costs nothing at each report. The service does not check the Windows version: the module works where it works, and a server where it does not, such as Server Core, ends up `UNUSABLE` with the module's own error. An agent whose update service is too old to know the request reports `UNUSABLE` with "the update service is too old for winget; it updates with the agent".

### Group Policy and sources

The service reads, as LocalSystem, the App Installer policy under `HKLM\SOFTWARE\Policies\Microsoft\Windows\AppInstaller`. A value of 0 turns a feature off; a missing value leaves it on, as winget does:

- `EnableAppInstaller` or `EnableWindowsPackageManagerCommandLineInterfaces` at 0: winget is off. Nothing runs, and the state is `UNUSABLE` as above.
- `EnableDefaultSource` at 0: the community source `winget` is not used.
- `EnableMicrosoftStoreSource`: not read, since the store source `msstore` is never used (ADR-0138 leaves it out). Its apps are per-user app packages updated by the store itself, and its agreements are made for an interactive user.

Every other source winget has configured for LocalSystem is used: the community source, and sources that an administrator added or that the `EnableAdditionalSources` policy adds, such as an internal REST source. `EnableAllowedSources` restricts what may be added, and MonitorFlow never adds, removes, resets or updates the configuration of a source. Inventory queries each source in turn (`Get-WinGetPackage -Source <name>`), never the store source, so a store source that wants its agreements accepted never stops a scan.

### Scope

LocalSystem's view of installed apps holds the machine-wide installs (the uninstall entries under `HKLM`, and machine-wide app packages) and LocalSystem's own per-user installs, which nobody makes by hand. Apps installed for a user are under that user's registry hive and are not seen at all: they are neither reported nor updated, which ADR-0138's out-of-scope list already states. Every app reported is therefore `MACHINE`, and an update installs with the scope winget recorded for it. The service passes no `-Scope`.

### What is reported per app

Each app winget matches to a used source becomes an ADR-0138 `ThirdPartyPackage`:

| Field | Value |
| --- | --- |
| `manager` | `WINGET` |
| `id` | the package identifier (`PSInstalledCatalogPackage.Id`) |
| `name` | its display name, cut to 256 characters, control characters removed |
| `version` | `InstalledVersion`, as winget writes it, `Unknown` included; at most 128 characters |
| `available_version` | when `IsUpdateAvailable`, the first of `AvailableVersions`, taken to be the newest as `winget show --versions` lists them (step 3 of the manual test checks it); else empty |
| `scope` | `MACHINE` |
| `origin` | the source's name, like `winget` |

Installed software that winget does not match to a source (its ID then starts with `ARP\` or `MSIX\`) is left out: nothing could update it. An app whose ID falls outside ADR-0138's grammar is left out too, and the count is logged, since it could never be named in a command. Pending apps come first and the list stops at 5,000, as ADR-0138 sets.

### Installs

The signed `apply_updates` reaches the service as before, with a new step, `{"op": "signed", "command": …, "step": "winget"}`, after the step `install` when the command keeps the OS. The service checks the command as for every step (ADR-0108), then builds the work from the signed payload alone: the `ThirdPartySelection` whose manager is `WINGET`. The step fails when the command has none. Each step of a command runs once, so the agent's account cannot replay it.

1. The service scans as above, so the selection is matched against what winget offers now.
2. It selects the pending apps, all of them for an empty selection, else those named (IDs compared without case, as winget does). A named ID that is not installed or has no update is listed as "not pending" and is not an error (ADR-0049, ADR-0138).
3. For each app in turn it runs `Update-WinGetPackage -Id <id> -Source <source> -MatchOption Equals -Mode Silent`: the exact ID from the exact source, with the installer's silent mode. `-Force`, `-IncludeUnknown`, `-AllowHashMismatch`, `-Override`, `-Custom` and `-Location` are never passed: a hash mismatch, an unknown installed version and a pinned app stay refusals.
4. One app's failure does not stop the others; the step fails when any app failed, and its error names them, as for Windows updates.

**Agreements.** A publisher's package agreement may be accepted for an unattended update, as the service accepts the license terms of Windows updates (ADR-0049): the administrator who queued the update chose the app. `Update-WinGetPackage` has no switch for agreements and the service passes none; the module runs without a prompt in the system context. Source agreements are never accepted by MonitorFlow: whoever adds a source that has them accepts them. An update the module refuses for an agreement fails with winget's code, `0x8A150041` (package) or `0x8A150046` (source), and its line says so; nothing waits for an answer.

**Results.** Each app gives one line, like Windows updates do: `<id> succeeded: <name>`, or `<id> failed (0x8A150101): <name>: <message>` with the extended error (an HRESULT, such as `0x8A150101` for an app in use or `0x8A150109` for a restart needed to finish), else the installer's own exit code, written in hexadecimal too (1603 is `0x00000643`). `NoApplicableUpgrade` counts as not pending.

**Timeouts.** A scan has 10 minutes: the first one downloads the community source's index. Each app has 30 minutes, the job object is killed after that, and the app fails with "timed out". At most 100 apps are updated by one command; the rest are listed as not started ("not attempted, over 100 apps") and the command fails, so a campaign wave cannot hold the service for days.

**Reboots.** An app's result says when it needs a restart (`RebootRequired`). The service adds it to the reply's `reboot_required`, and the agent then follows `allow_reboot` exactly as for Windows updates: the signed `reboot` step, a restart in one minute. Installers that start a restart on their own are prevented by the silent mode as far as the installer honours it.

**One at a time.** The service handles one pipe request at a time (ADR-0049), so winget never runs alongside a Windows Update install, and an install another process runs shows as winget's `0x8A150102`.

### Pipe protocol

Additive, as ADR-0049's:

| Request | Reply |
| --- | --- |
| `{"op": "winget_scan"}` | `{"apps": {"state": "available" \| "absent" \| "unusable", "version", "error", "apps": [App]}}` |
| `{"op": "signed", "command": …, "step": "winget"}` | `{"installed": {"results": [Result], "not_pending": [id], "reboot_required": bool}}`, ADR-0049's reply |

`App` has `id`, `name`, `source`, `version` and `available`. `winget_scan` changes nothing and is not signed, like `scan`. The service turns a `winget` step into `{"op": "winget_upgrade", "ids": [id]}` for itself, and refuses that request when it comes from the pipe, as it refuses `install`. An older service answers both new requests with an error, which the agent reports as above.

## Manual test

On a Windows Server 2025 virtual machine, with a MonitorFlow agent and its update service installed by the MSI:

1. Before anything, the server's Updates tab shows winget as not installed, "PowerShell 7 is not installed".
2. As an administrator: install PowerShell 7 machine-wide with its MSI, then in `pwsh`, `Install-Module Microsoft.WinGet.Client -Scope AllUsers`. After the next report, the tab says "the module is not installed" before the second step and lists apps after it.
3. Install an older version of two test apps machine-wide from the community source (`winget install --id <id> --version <old> --scope machine`), and one with `--scope user`. Wait for a report: the two machine-wide apps are pending with their available version, which is the newest `winget show --id <id> --versions` lists, and the user-scope one is absent.
4. Queue an update of one of them by ID: the command's output has one line for it, the next report shows it current, and `update.log` shows the step. Queue an update of all third-party apps: the other one is updated. Queue an ID that is not installed: "not pending", the command succeeds.
5. Set `EnableDefaultSource` to 0 under the policy key: the next report has no apps from `winget`. Set `EnableAppInstaller` to 0: the tab says winget is turned off by Group Policy.
6. Start one test app and queue its update: the line shows `0x8A150101` (in use) or the installer's code.
7. Install an app whose manifest has a package agreement and queue its update: it is either updated or fails with `0x8A150041`, never hangs. Record which in this ADR.

## Consequences

- Third-party updates on Windows need PowerShell 7 and the winget module, installed once by an administrator; servers without them report winget as absent and keep their Windows updates.
- A compromised agent account can make LocalSystem list apps, and update machine-wide apps that a signed command named to what their source offers; it cannot choose a source, pass an option or name a version.
- The module runs inside the LocalSystem service's child process, so its code is trusted as much as the module folder's access list. That folder is writable by administrators only.
- A source that publishes a bad version reaches every server whose campaign selects winget; ADR-0138's OS-only default keeps that an explicit choice.
- Follow-ups: per-app reboot behaviour in the report, should winget's API expose it; the module's in-process mode on Windows PowerShell 5.1, if winget adds it, would drop the PowerShell 7 prerequisite.
