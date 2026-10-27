# ADR-0050: Windows agent MSI

Date: 2026-10-29
Status: accepted; the update service is installed by the MSI since ADR-0053

## Context

ADR-0047 installs the Windows agent by hand: copy the binary, `sc.exe create` and `sc.exe failure`, then `icacls` on `%ProgramData%\MonitorFlow`. Phase 6 task (d) packages these steps as an MSI, the format Group Policy, Intune, SCCM and `msiexec /qn` deploy. The package must give the same result as the install guide, survive upgrades without losing the identity or the configuration, and accept the upstream and the enrollment token on a silent command line without leaving the token in the MSI log. ADR-0047 also left the Event Log to the package, because a source must be registered at install time.

## Decision

**WiX Toolset v5, as a .NET tool.** The package is written in WiX (`deploy/windows/Package.wxs`) and built with the `wix` .NET tool, pinned in `deploy/windows/.config/dotnet-tools.json` (5.0.2) with the matching `WixToolset.Util.wixext`. `deploy/windows/build-msi.ps1` restores the tool and builds `target\msi\monitorflow-agent-<version>-x64.msi` from a release build; the runner only needs the .NET SDK, which `windows-latest` has. WiX is the standard for MSI authoring and its source is declarative XML that reviews like code. v5 rather than v6, whose binaries come under the Open Source Maintenance Fee EULA. `cargo-wix` was rejected: it wraps WiX v3 and generates a template we would rewrite anyway. NSIS and Inno Setup make executables, not MSIs, and lack the transactional rollback and the deployment tools' support.

**Layout.** Per-machine, 64-bit only. `%ProgramFiles%\MonitorFlow` holds `monitorflow-agent.exe` and `agent.windows.example.toml`. The package creates `%ProgramData%\MonitorFlow`, its `agent` and `scripts.d` with the guide's ACLs, through `MsiLockPermissionsEx` (WiX `PermissionEx` with SDDL), protected from inheritance:

| Folder | SYSTEM | Administrators | `NT SERVICE\MonitorFlowAgent` |
| --- | --- | --- | --- |
| `MonitorFlow` (configuration) | F | F | RX |
| `MonitorFlow\agent` (`data_dir`) | F | F | M |
| `MonitorFlow\scripts.d` | F | F | RX |

Every entry is inherited by files and subfolders (`OICI`). The ACLs name the virtual account by SID (`S-1-5-80-` and the SHA-1 of the upper-case service name), because the account resolves only once the service exists and the folders are created before. `scripts.d` gets its own protected ACL, so ADR-0048's checks hold even if the parent's changes: nobody but SYSTEM and Administrators may write there. ADR-0048's example also lets Users read; the package does not, as only the service runs the scripts.

**Service.** `ServiceInstall` creates `MonitorFlowAgent`, automatic start, as `NT SERVICE\MonitorFlowAgent` without password, command line `"…\monitorflow-agent.exe" --config "%ProgramData%\MonitorFlow\agent.toml" service`. `util:ServiceConfig` sets the guide's failure actions: restart after 60 s three times, counter reset after a day. The service is started by the install only when it has a configuration: `UPSTREAM_URL` was given, or an upgrade replaces an earlier version. Otherwise the administrator writes `agent.toml` and starts it.

**Configuration and enrollment.** Public properties, for `msiexec /i … /qn`:

- `UPSTREAM_URL`: when set, a deferred custom action running as SYSTEM writes `agent.toml` with it, `ENROLL_URL` and `ENROLLMENT_TOKEN` (both optional). It runs the installed agent, `monitorflow-agent.exe --config … init-config --upstream-url … --enroll-url … --enrollment-token …`, which validates the result as the agent would load it and creates the file only if it does not exist: an upgrade, a repair or a second install never replaces an edited configuration. The file inherits the folder's ACL. An invalid URL fails the install, and it rolls back.
- `ENROLLMENT_TOKEN` is `Hidden`, listed in `MsiHiddenProperties`, so the log prints asterisks for its value, and the custom action has `HideTarget`, so its command line is not logged either. Only administrators can read the command line of a process running as SYSTEM. Properties from the command line are not stored in the cached package or the registry, and the package never writes them anywhere else. The token stays in `agent.toml` after enrollment, as with the manual steps; the guide says to remove it. CI checks the verbose log for the token.
- `REMOVE_DATA=1`, on uninstall only (below).

Writing the file from the agent rather than a script: a PowerShell custom action would need the token on its command line too, under an interpreter that logs more, VBScript is being removed from Windows, and a C# or Rust custom action DLL would add a toolchain or another crate of `unsafe` FFI. `init-config` is about forty lines, tested on Linux, and also serves manual installs.

**Event Log.** The package registers the source `MonitorFlowAgent` in the Application log, with .NET Framework 4's `EventLogMessages.dll` as message file: it formats every event ID as its one string, and every supported Windows Server ships with it, so the agent needs no message table of its own. The agent writes one event, ID 1, error, when the service stops on an error, with the error's text. Until now that reason reached only `agent.log`, and nowhere when logging had not started yet (an invalid configuration, an unwritable log); the control manager's 7024 event gives only the exit code. Everything else still goes to `agent.log` (ADR-0047): mirroring the log in the Event Log would double the I/O and flood the Application log at debug level. Writing events uses `RegisterEventSourceW` and `ReportEventW`, one `unsafe` block. Without the source registered, as after a manual install, Windows still records the event, without its formatted message.

**Upgrades.** `MajorUpgrade` with the default schedule: the old version is removed (service stopped and deleted), then the new one installed and its service started. Builds of the same version upgrade each other (`AllowSameVersionUpgrades`), since every build has its own product code; downgrades are refused. Nothing under `%ProgramData%` belongs to the package's components except the three folders, so `agent.toml`, the identity, the sensor configuration and the journal stay as they are, and the virtual account's SID, derived from the name, keeps its access. The version is the agent crate's from `Cargo.toml`, without a pre-release suffix, which MSI versions cannot carry.

**Uninstall.** Removes the binary, the example, the service and the event source. `%ProgramData%\MonitorFlow` stays, so reinstalling keeps the server's identity, unless `REMOVE_DATA=1` is given (`msiexec /x … REMOVE_DATA=1`): `util:RemoveFolderEx` then deletes the folder and everything in it. The removal that an upgrade does never deletes it. Empty folders the package created are removed.

**Extension point.** `deploy/windows/Helpers.wxs` defines the component group `AgentHelpers`, part of the agent feature and empty today. The privileged Windows Update component of ADR-0049 adds its binary, service and ACLs there.

**Signing.** The package and the binary are unsigned for now: SmartScreen warns on a downloaded copy and Windows shows "Unknown publisher". Signing plugs into `build-msi.ps1`: `signtool sign /fd sha256 /tr <timestamp URL>` on `monitorflow-agent.exe` before `wix build`, so the signed binary is packaged, and on the `.msi` after it, with the certificate from a CI secret or a cloud HSM (Azure Trusted Signing), never from the repository. It is a release step, decided with the release process.

**CI.** The `windows` job builds the package after the release binary, then a second one with version 255.0.0, and runs `deploy/windows/smoke-test.ps1` on the runner: silent install with `UPSTREAM_URL` and a well-formed token, then checks of the files, the service account, command line, start mode and failure actions, the three DACLs, `agent.toml` and its inherited ACL, the event source, that the token is absent from the MSI log, and that the service runs and logs its enrollment attempts to an upstream that does not exist. It then upgrades and checks that the configuration and data are unchanged and the service runs again, makes the service fail on an invalid configuration and finds the reason in the Application log, uninstalls and checks what is removed and what is kept, then reinstalls and uninstalls with `REMOVE_DATA=1`. The job uploads the `.msi` as the `monitorflow-agent-windows-x86_64-msi` artifact.

## Consequences

- A Windows server is installed with one command: `msiexec /i monitorflow-agent-<version>-x64.msi /qn UPSTREAM_URL=… ENROLLMENT_TOKEN=…`. The manual steps stay in the guide for hosts where an MSI cannot be used.
- The agent gains the `init-config` subcommand and the Event Log error. Logs otherwise stay in `agent.log`.
- `agent.toml` written by the package holds only the keys given; other keys are at their defaults, described in the example next to the binary.
- Unsigned packages warn when downloaded until the signing step exists.
- 32-bit and ARM64 Windows are not packaged.
