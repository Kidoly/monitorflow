# ADR-0048: Windows sensors

Date: 2026-10-29
Status: accepted

## Context

Phase 6 brings the agent to Windows (ADR-0047). The sensors of ADR-0015, ADR-0021 and ADR-0024 read `/proc`, `statvfs`, `systemctl` and log files under `/var/log`, and run scripts in a Unix process group: none of this exists on Windows. `crates/sensors` was kept free of transport code so a Windows agent could reuse it (ADR-0021); what remains is the platform code. Sensor configurations are stored once on the server and pushed to every agent of a site or server (ADR-0016), and the server, which runs on Linux, validates rows with `registry::build` (ADR-0015), so it must know the Windows kinds too.

## Decision

### Kinds and platforms

A kind keeps its name when it measures the same thing on both platforms with the same metrics: `cpu`, `memory`, `disk`, `port` and `script`. Thresholds, dashboards and the built-in defaults (cpu, memory, disk) then work on a Windows server unchanged. Where the source and its metrics differ, Windows gets kinds of its own: `windows_service` (the analogue of `systemd`) and `eventlog` (the analogue of `log`). Reusing `systemd` for services would have given its metrics (`loaded`, `restarts`, `state_age_seconds`) a meaning Windows cannot fill and a unit-name syntax services do not have; reusing `log` would have made `path` mean a channel.

`registry::supported(kind)` says what this platform runs: `systemd` and `log` need Unix, `eventlog` and `windows_service` Windows. `registry::KINDS` lists every kind on every platform. The agent's `build_local` refuses the others with the new `BuildError::Unsupported` ("sensor `eventlog` is not supported on this platform (linux)"), before looking at the parameters. The agent reports it as an invalid spec, not retried (ADR-0024's amendment), since the platform will not change. `registry::build`, which the server uses, still checks the parameters of the Windows kinds on Linux and returns a sensor whose collections fail permanently; on Windows it refuses `systemd` and `log` as unsupported, as their code is Unix only and not compiled there. Nothing panics on either side.

The Win32 code lives in `cfg(windows)` submodules (`cpu/windows.rs`, `disk/windows.rs`, …) next to the Linux code, which keeps its behaviour; parameter parsing and pure logic stay portable so they are tested on Linux as well. Calls go through `windows-sys` (the version tokio already uses): direct Win32 calls for a handful of values each, rather than `sysinfo`, which would bring its own polling model and many more APIs than we read.

### cpu

The total comes from `GetSystemTimes`, per core from `NtQuerySystemInformation(SystemProcessorPerformanceInformation)`, a documented `winternl.h` call. Kernel time includes idle time, so busy is kernel minus idle plus user. Usage is computed as on Linux, from deltas, with the same instances (`total`, `0`, `1`, …). The per-core query returns the cores of the agent's processor group, at most 64; hosts with more groups report their first group per core, and the total covers all of them. Windows has no load average: `load1`, `load5` and `load15` are not reported, and a threshold on them never fires on Windows.

### memory

`GlobalMemoryStatusEx`. `total_bytes`, `available_bytes`, `used_bytes` and `available_ratio` have their Linux meaning. Windows has no swap partition: `swap_total_bytes` is the page files' size (commit limit minus physical memory) and `swap_free_bytes` the commit still available beyond the available physical memory, capped by the total. Both are estimates, close enough for "page file nearly full".

### disk

Fixed drives with a letter (`GetLogicalDrives`, `GetDriveTypeW` is `DRIVE_FIXED`), with `GetDiskFreeSpaceExW`: instance `C:\`, label `fstype` the file system in lower case (`ntfs`, `refs`), label `device` the volume GUID path (`\\?\Volume{…}\`). Removable, optical, network and RAM drives are skipped (mapped network drives belong to a logon session anyway), and so are volumes mounted only in a folder. `available_bytes` honours the quotas of the agent's account, like `f_bavail`. NTFS has no inode limit, so no inode metrics. Volume queries run on the blocking pool and stat calls reuse the hung-mount handling of ADR-0015. `fstypes_include`/`fstypes_exclude` apply to the lower-case type; `mounts_exclude` now also accepts drives (`D:` or `D:\`, compared without case), so one site configuration can exclude mounts on both platforms.

### port

Already portable (ADR-0021), unchanged. A refused connect takes about two seconds on Windows, which retries the SYN after a reset; the check reports down either way, at most at its deadline.

### windows_service

Parameter `services`: a comma-separated list of at most 64 service key names (`W32Time`, `MSSQL$SQLEXPRESS`, not display names), of `[A-Za-z0-9 _.$@{}-]`, at most 256 bytes, not starting with `-`; a name listed twice, ignoring case as Windows does, is refused. One connection to the Service Control Manager per collection (`OpenSCManagerW` with `SC_MANAGER_CONNECT`, then `OpenServiceW` with `SERVICE_QUERY_STATUS` and `QueryServiceStatusEx` per service), on the blocking pool, bounded by the deadline. Per service, with the name as configured as instance:

| Metric | Value |
| --- | --- |
| `installed` | 1 when the service exists |
| `active` | 1 when it is running |
| `failed` | 1 when it is stopped with an exit code other than 0 and `ERROR_SERVICE_NEVER_STARTED`; for `ERROR_SERVICE_SPECIFIC_ERROR`, when the service's own code is not 0 |
| `state` | The SCM state: 1 stopped, 2 start pending, 3 stop pending, 4 running, 5 continue pending, 6 pause pending, 7 paused; 0 when not installed |

As with systemd, a missing service is data (`installed 0`, `active 0`), so `active < 1` alerts on it; a paused service is not active. Other SCM errors are transient. Example: `kind = "windows_service"`, `name = "services"`, `params = {"services": "W32Time,Spooler"}`; threshold `monitorflow_services_active < 1`.

### eventlog

**What the server can name.** Parameter `channel` (`System`, `Microsoft-Windows-TaskScheduler/Operational`), read only if it is one of the agent's `sensors.event_channels` (default `Application` and `System`, compared without case), the counterpart of `log_dirs`: a pattern reveals whether matching events exist, and `Security` holds logons, so an administrator adds it deliberately. An empty list disables the sensor. Filters, all optional, all of which must hold: `providers` (at most 16 names), `levels` (`critical`, `error`, `warning`, `information`, `verbose`; `information` is level 4 or 0, as providers use both), `event_ids` (at most 16 numbers up to 65535), `pattern` and `exclude` (regular expressions of ADR-0024, now in `crates/sensors/src/pattern.rs`, matched on the event's message). Channel and provider names are limited to `[A-Za-z0-9 ._-]` (and `/` in channels), because providers, levels and IDs become an XPath query: no quote or bracket can reach it.

**Reading.** With the `Evt*` API of `wevtapi.dll`, on the blocking pool, at most one read in flight per sensor. Each collection finds the channel's newest record ID (a reverse query, one event), then queries `*[System[EventRecordID>last and EventRecordID<=newest and (providers) and (levels) and (ids)]]` forward. Events are counted as they come; with `pattern` or `exclude`, the message is formatted with `EvtFormatMessage` and the provider's metadata (kept open, up to 256 providers), or the event's XML is matched when the provider has no message for it; the first 16 KiB are matched. The first collection only records the newest ID: history is not counted, as for log files. A channel that was empty then is read from its start, and so is a cleared channel (its newest ID went below the last one read). At most 10 000 events are examined per collection, and the read stops after three quarters of the time left so what it saw is still reported; the rest waits for the next one.

**Metrics.** `matches`, events that matched since the previous collection, and `lag_events`, records of the channel after the last one examined, with label `channel` and the configured instance; thresholds as for `log` (`monitorflow_app_errors_matches > 0`).

**Failure semantics.** A channel that does not exist, access denied and a query the Event Log refuses are permanent; other errors are transient. Events are not forwarded, only counted, for the reasons of ADR-0024.

Example: `kind = "eventlog"`, `name = "service_crashes"`, `params = {"channel": "System", "providers": "Service Control Manager", "event_ids": "7031,7034"}`.

### script

Same parameters, output contract (Nagios status, `name=value` perfdata) and failure semantics as ADR-0024; on Windows, PowerShell replaces `sh`.

**What runs.** A file in `sensors.script_dir` (default `C:\ProgramData\MonitorFlow\scripts.d`) ending in `.ps1`, run by Windows PowerShell 5.1 from the system directory (found with `GetSystemDirectoryW`, not `PATH` or `%SystemRoot%`) with `-NoLogo -NoProfile -NonInteractive -ExecutionPolicy Bypass -File <script> <args>`, or in `.exe`, starting with `MZ`, run directly. The checks below are what makes a script trusted, so the execution policy is bypassed rather than requiring signed scripts; a machine policy set by Group Policy still wins. Batch files (`.bat`, `.cmd`) are refused: `cmd.exe` parses their command line again, which is the argument injection class of CVE-2024-24576. A PowerShell script that throws exits with 1, WARNING; scripts should catch errors and `exit 3`.

**Checks, at build and before every run.** The script directory, the file (symlinks resolved) and the file's own directory must be owned by LocalSystem, BUILTIN\Administrators, TrustedInstaller or the agent's own account, and their DACL may grant write rights (write or append data, delete, delete child, change permissions or owner, generic write or all) to these only. Inherit-only entries do not apply to the object and are skipped, deny entries are ignored, a missing DACL is refused, and an allow entry of another type (callback, object) granting write is refused rather than interpreted. `C:\ProgramData` lets users create files in new subfolders by inheritance, so the directory must be created with an explicit ACL:

```
New-Item -ItemType Directory C:\ProgramData\MonitorFlow\scripts.d
icacls C:\ProgramData\MonitorFlow\scripts.d /inheritance:r /grant:r "*S-1-5-18:(OI)(CI)F" "*S-1-5-32-544:(OI)(CI)F" "*S-1-5-32-545:(OI)(CI)RX"
```

**How it runs.** With an environment of the system variables only (`SystemRoot`, `ProgramData`, `TEMP`, `USERPROFILE` and a few more taken from the agent's environment) and `PATH` set to the system directories, in the root of the system drive, without a window, stdin and stderr closed, and inside a job object with `JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE`. The job is terminated at the deadline, when the scheduler cancels the collection and after every run, so nothing the script started outlives it, like the process group on Unix. The process is put in the job right after it starts, not created inside it: Rust's `Command` does not expose `PROC_THREAD_ATTRIBUTE_JOB_LIST`, and PowerShell takes far longer to start than the gap.

## Consequences

- Built-in defaults (cpu, memory, disk) run on Windows agents without change; a CPU load threshold has no data there.
- A site mixing Linux and Windows servers can hold `systemd` and `windows_service` sensors side by side; each agent runs its own and lists the others as unsupported in its acknowledgement. Server-level rows avoid that noise.
- The server validates Windows rows on Linux, so the web interface and the API need no platform knowledge; the web labels the two new kinds.
- `sensors.event_channels` is a new agent setting (`MONITORFLOW_SENSORS_EVENT_CHANNELS`), ignored on Linux.
- Linux tests are unchanged; Windows tests read the real APIs (services, the Event Log with `eventcreate`, ACLs with `icacls`, PowerShell) and run in the `windows-sensors` CI job.
- Follow-ups: the file `log` sensor on Windows (rotation follows file IDs instead of inodes), PowerShell 7 (`pwsh`) as an option, volumes mounted in folders, per-core counters beyond the first processor group, and disk I/O counters on both platforms.
