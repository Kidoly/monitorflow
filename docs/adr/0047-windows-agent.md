# ADR-0047: Windows agent

Date: 2026-10-29
Status: accepted; Windows Update and its privileged component in ADR-0049

## Context

Phase 6 brings the agent to Windows servers (product.md: sensor support and Windows update management). The agent is Linux only today. It reads `/etc/os-release` and `/proc`, keeps its private key behind Unix modes, stops on SIGTERM, runs privileged steps through `sudo -n monitorflow-agent …` helpers (ADR-0032, ADR-0036), drives apt and dnf, and runs `shell` commands with `/bin/sh -c` (ADR-0043). ADR-0021 kept `crates/sensors` free of transport code so that a Windows agent could reuse it.

Phase 6 has four parts: (a) the agent itself, with its service, enrollment and inventory; (b) Windows sensors; (c) Windows Update inventory and installs; (d) the install guide and an MSI package. This ADR decides (a) and sets the limits the later tasks build on.

## Decision

**One binary, platform code behind `cfg`.** The same `monitorflow-agent` crate builds for `x86_64-pc-windows-msvc`. Unix-only code is gated with `#[cfg(unix)]` and its Windows counterpart with `#[cfg(windows)]`. Everything else is shared unchanged: configuration, enrollment, identity storage and renewal, the stream, sample upload, sensor configuration, the command dispatcher and its journal. A separate `monitorflow-agent-windows` crate was rejected. It would either copy that code or force it into one more library crate, and the platform-specific part is small: inventory, the service wrapper, file ACLs, and which command handlers are registered. MSVC is the target rather than GNU because it is the default Windows toolchain and what the CI runner has. `ring` and `rustls` build on it without other C tooling.

**A Windows service under a virtual account.** On Windows the agent runs as the service `MonitorFlowAgent`, using the `windows-service` crate. The service control manager starts `monitorflow-agent.exe --config <path> service`. The `service` subcommand registers the control handler, reports `Running` and accepts `Stop` and `Shutdown`. Either control fires the same shutdown trigger that SIGTERM fires on Linux (`common::client::shutdown`). The service then reports `StopPending` with a 30 s wait hint, and `Stopped` once the agent returns: exit code 0, or service-specific code 1 after an error, which the control manager writes to the System event log. Started without `service`, from a console, the agent stops on Ctrl+C, Ctrl+Break or when the console closes.

The service runs as its virtual account `NT SERVICE\MonitorFlowAgent`. It has its own SID, no password and no administrator rights, and ACLs can name it, which makes it the equivalent of the `monitorflow` system user. LocalService was rejected because it is one identity shared by many services: an ACL that lets it read the key lets all of them read it. NetworkService was rejected for the same reason. LocalSystem was rejected because it has full control of the host, so any agent bug would hand that control over.

**No elevation yet.** Windows has no sudo, and this task runs nothing privileged. Package and source commands are not supported (below). Windows Update installs, task (c), will need administrator rights. Following ADR-0036, they will go through a separate privileged component that accepts typed requests and builds the actual calls itself, not through a general elevation path. That component is decided with Windows Update (planned as ADR-0049).

**Paths.**

| What | Linux | Windows |
| --- | --- | --- |
| Binary | `/usr/bin/monitorflow-agent` | `%ProgramFiles%\MonitorFlow\monitorflow-agent.exe` |
| Configuration | `/etc/monitorflow/agent.toml` | `%ProgramData%\MonitorFlow\agent.toml` |
| `data_dir` (identity in `tls\`, sensor configuration, command journal) | `/var/lib/monitorflow/agent` | `%ProgramData%\MonitorFlow\agent` |
| `sensors.script_dir` | `/etc/monitorflow/scripts.d` | `%ProgramData%\MonitorFlow\scripts.d` |
| `sensors.log_dirs` | `["/var/log"]` | empty (log sensor off) |
| Logs | stdout (journald) | `<data_dir>\logs\agent.log` |

`%ProgramData%` is read from the environment, with `C:\ProgramData` as the fallback. `%ProgramFiles%` lets only administrators write by default, like a root-owned `/usr/bin`. Windows has no log directory comparable to `/var/log`, so the log sensor stays off until the administrator lists directories. The Event Log is a sensor of task (b).

**ACLs instead of modes.** By default `%ProgramData%` lets every user read what is created under it, so the agent cannot rely on inheritance alone. `common::winacl` sets protected DACLs:

- A private file (callers of `write_atomic` that pass a mode without group or other bits: the private key and the command journal) gets `D:P(A;;FA;;;SY)(A;;FA;;;BA)(A;;FA;;;OW)` before anything is written to it. That is full control for `SYSTEM`, `Administrators` and the file's owner, and nothing inherited.
- A directory that `write_atomic` creates gets the same entries, inheritable (`OICI`), in place of mode 0700. Public files (certificate, CA) inherit from it.
- At load, a key whose DACL is not of that shape is logged as a warning, as a group- or other-readable key is on Unix.

The owner entry (Owner Rights, `OW`) names whoever created the file. Under the service, that is the virtual account, so the agent never has to look up its own SID. Administrators keep access, as root does on Linux. The install guide also sets ACLs by hand: the configuration is readable by the service and writable by administrators only, and `data_dir` is modifiable by the service only. The MSI package, task (d), will set them in the same way. Windows cannot sync a directory, and NTFS journals renames, so atomic writes skip the directory sync there. These are the only `unsafe` blocks: three Win32 security calls, each with a comment explaining why it is sound.

**Logs.** A service has no console and Windows has no journald. As a service, the agent logs to `<data_dir>\logs\agent.log` (text or JSON, per `[log]`). When the file would grow past 16 MiB, it replaces `agent.log.1` and the agent starts a new file, so logs take at most 32 MiB. The Event Log was rejected for now because it needs a registered event source and a message file, an install step that fits the MSI better. From a console, the agent logs to stdout as on Linux.

**Inventory.** `Hello.os_family` is `windows`. The server accepts it, and a new migration widens the `servers.os_family` CHECK. `os_version` comes from `HKLM\SOFTWARE\Microsoft\Windows NT\CurrentVersion`: `ProductName`, `DisplayVersion`, `CurrentBuildNumber` and `UBR`, for example `Windows Server 2022 Datacenter 21H2 build 20348.2849`, cut to the server's 128 bytes. `ProductName` still says "Windows 10" on Windows 11, so builds from 22000 on are renamed. `RtlGetVersion` was rejected: it gives numbers without the edition, and calling it needs `unsafe`. Any user can read these registry values, through the safe `windows-registry` crate. The host name is the DNS host name from `Tcpip\Parameters`, else `COMPUTERNAME`.

**Commands.** Signature checks, the journal (ADR-0031, file-based) and the dispatcher are unchanged. On Windows, `apply_updates`, `configure_sources`, `revert_sources` and `shell` are registered with an `Unsupported` handler. It answers `REJECTED` with a reason such as "package updates are not supported on Windows yet", which the commands API shows, rather than the dispatcher's "unknown kind". No `PackageReport` is sent. Windows Update reports and installs come with task (c).

`shell` stays unsupported on Windows even with `commands.shell = true`, and the agent logs a warning when the option is set. ADR-0043 runs commands with `/bin/sh` because ADR-0041's risk rules assume its semantics. PowerShell parses the same line differently: `;` and `&`, `$(…)` and `$env:` expansion, backtick escapes, and aliases such as `sc` (Set-Content in Windows PowerShell 5.1) or `curl` (Invoke-WebRequest). A line the rules classify as read-only under `sh` could therefore change something under PowerShell. `cmd.exe` has the same problem. Remediation on Windows needs classifier rules of its own and its own ADR. Until then, the incident loop escalates a Windows server's first command with the agent's refusal.

There is no journald reader to port: the log sensor reads files on Linux too. Windows logs come from the Event Log sensor of task (b).

**Sensors.** `registry::KINDS` stays the same on every platform, so the server's validation does not change. On Windows, a kind without a Windows implementation is refused with `BuildError::Refused` ("not available on this platform"). The agent reports it in the configuration acknowledgement, like any sensor it refuses. Today `port` works on Windows. `crates/sensors` gates its Linux modules and registry arms with `cfg(unix)` and nothing else, so task (b) can add the Windows implementations there (ADR-0021). Task (b) did so in ADR-0048, which also replaced this refusal by `BuildError::Unsupported` ("not supported on this platform") for the Unix-only kinds.

**CI.** A `windows` job on `windows-latest` runs clippy with `-D warnings` and the tests of `monitorflow-agent`, `monitorflow-sensors` and `monitorflow-common`, then uploads the release build of the agent as an artifact. The server and relay stay Unix only. On Windows, the agent's integration tests (which run a real server) and its Linux helper tests are compiled out, and so are the dispatcher's unit tests, which use the server's CA.

## Consequences

- Linux behaviour is unchanged. The only visible difference is that the signal handlers are installed before the configuration is read.
- A Windows server enrolls, appears in the fleet with its version, renews its certificate and keeps its stream like a Linux one. Until task (b), the sensors the server configures by default are refused, except `port`.
- Update campaigns that pin mirrors refuse Windows targets, because no mirror family covers them. A campaign without pinned mirrors still queues `apply_updates` for them, and the agent's refusal counts as a failure in the wave. Leave Windows servers out of campaigns until task (c).
- Installing is manual (`sc.exe` and `icacls`) until the MSI. An administrator who starts the agent from an elevated console creates files owned by `Administrators`, which the service then cannot read. The guide says to enroll through the service.
- The web interface shows the family before the version, as it does for Linux: `windows Windows Server 2022 …`. A label for the family is left to the interface.
