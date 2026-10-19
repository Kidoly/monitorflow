# ADR-0024: Script and log sensors, and what the server may make an agent do

Date: 2026-10-19
Status: accepted

## Context

The last two Phase 2 sensors run a custom check and count pattern matches in a log file. Sensor configurations are pushed by the server (ADR-0016) and applied by every agent of a site. If the server could name a command, a custom script sensor would be remote code execution as the agent's user on the whole fleet; if it could name any path, a log sensor would read any file the agent can read (keys, `/etc/shadow` when the agent runs as root) and leak it through match counts. A compromised server, a stolen admin account or a bad API row must not be able to do either. The server's API validates rows with the agent's registry (ADR-0015), but it cannot know what each host allows.

## Decision

### Agent-side limits

The agent's own configuration file, written by the host's administrator, sets what server-configured sensors may touch: the `[sensors]` section of `agent.toml`. The server's configuration only names things inside these limits. `registry::Local` carries them. `registry::build` checks parameters syntactically, with the default limits, and the API keeps using it; the agent calls `registry::build_local`, which also refuses (`BuildError::Refused`, "refused by this agent") what this host does not allow, like a script nobody installed. Refusals go into the configuration acknowledgement like any other refused spec (ADR-0016), so the operator sees them. What only shows at run time (a script whose mode changed after it was accepted) is a permanent sensor error: logged at error level, and the sensor stops until it is configured again or the agent restarts (ADR-0012).

Neither sensor is in the built-in defaults; both need parameters only the operator knows.

### Script

**What the server can name.** Parameter `script`, a file name of `[A-Za-z0-9_.-]`, at most 64 bytes, not starting with `.` or `-`, without `..`: never a path or a command. The agent runs it only from `sensors.script_dir` (default `/etc/monitorflow/scripts.d`, rather than under `data_dir`, which the agent writes itself). Optional `args`, a comma-separated list of at most 8 plain words of `[A-Za-z0-9_.:/=@%+-]`, up to 128 bytes each, passed as separate arguments. There is no shell, so the charset is not what prevents injection; it keeps arguments readable in logs and harmless for scripts that pass them on. A script that should not take arguments from the server must refuse them itself.

**Checks, at build and before every run.** The script directory and the file's own directory (after resolving symlinks) are directories owned by root or the agent's user and not writable by group or other. The file, symlinks resolved, is a regular file with the same owner and mode rules, executable, and starts with `#!` or an ELF header: a file of plain shell commands is refused rather than handed to `/bin/sh`, which glibc's `execvp` does on `ENOEXEC` when the process is not started with `posix_spawn`. A symlink is followed only to a file and directory that pass the same checks, so packaged plugins (`/usr/lib/nagios/plugins`) can be linked in. Only someone who can already write the script directory, root or the agent's user, can change what runs between the check and the `exec`.

**How it runs.** `tokio::process` with the resolved path, the fixed arguments, an empty environment apart from `PATH=/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin` and `LC_ALL=C`, working directory `/`, stdin from `/dev/null`, stderr discarded, in a new process group. It stops 200 ms before the collection deadline (at most half the remaining time), like the port sensor (ADR-0021), so a hung script still reports instead of being cancelled with nothing. The process group is killed with SIGKILL at that point, when the scheduler cancels the collection, and after a normal exit, so nothing the script left in the background outlives the run. Standard output is read up to 64 KiB.

**Output contract.** Nagios plugin exit codes, which administrators already know and existing plugins follow:

| Metric | Value |
| --- | --- |
| `script_status` | Exit code 0 OK, 1 WARNING, 2 CRITICAL, 3 UNKNOWN. Also 3 when the script timed out, was killed by a signal, exited with another code or wrote more than 64 KiB |
| `script_duration_seconds` | How long the run took |
| `script_<name>` | One gauge per `name=value` line of standard output |

`script` stands for the configured name. Samples carry the label `script` (the file name) and the configured instance, so thresholds work as `monitorflow_backup_status >= 2`, or `>= 1` for warnings. Perfdata names must be valid metric suffixes (ADR-0015, `[a-z][a-z0-9_]*`) and not `status` nor `duration_seconds`; values must be finite numbers; the first of a repeated name wins; at most 32 gauges per run. Other lines, like the human summary on the first line, are ignored. Nagios' `'label'=value[UOM];warn;crit` syntax is not parsed: units would have to be mapped to Prometheus base units, and thresholds belong to the server. Perfdata stays inside the configured name's namespace, so a script cannot impersonate another sensor (ADR-0015).

**Failure semantics.** Anything the script itself does (non-zero exit, timeout, signal, too much output) is a status, because thresholds can only alert on samples. Spawn errors `ENOENT` and `EACCES` (the file vanished, a `noexec` mount) are permanent; other spawn errors are transient.

**Installing a script.** As root:

```
install -d -o root -g root -m 0755 /etc/monitorflow/scripts.d
install -o root -g root -m 0755 check_backup /etc/monitorflow/scripts.d/
```

Then create a sensor with `kind = "script"`, `name = "backup"`, `params = {"script": "check_backup"}` and a threshold on `monitorflow_backup_status`. Test it locally first: `monitorflow-agent sensors probe script --param script=check_backup --param args=-w,2`, which reads `script_dir` from the agent's configuration file when it exists.

### Log

**What the server can name.** Parameter `path`, an absolute path without `.` or `..` segments, at most 256 bytes so it fits a label value. The agent reads it only if it is under one of `sensors.log_dirs` (default `/var/log`; an empty list disables the sensor), lexically when the configuration is applied and, at every collection, once the path and the directories are resolved with `canonicalize`, so a symlink inside `/var/log` pointing to `/etc/shadow` is refused (permanent error). The resolved file is opened with `O_NOFOLLOW` and `O_NONBLOCK` and must be a regular file, so a FIFO swapped in cannot hang a blocking thread. Someone able to swap directories inside an allowed directory between the check and the open could still redirect the read; that takes write access to the log directory, which is root's on `/var/log`. `openat2` with `RESOLVE_BENEATH` would close this but needs Linux 5.6, newer than RHEL 8.

**Patterns.** `pattern` (required) and `exclude` (optional, matching lines are not counted), regular expressions of the `regex` crate on bytes: matching is linear in the line length whatever the pattern, so a hostile pattern cannot stall the agent (no backtracking). Patterns are at most 1024 bytes and compile within 1 MiB, which refuses blow-ups like `(\w{1000}){1000}`. They are checked by `registry::build`, so the API refuses bad ones.

**Reading.** On the blocking pool, bounded by the deadline like the other sensors, with at most one read in flight per sensor. The first collection after the sensor is built starts at the end of the file: history is not counted, which would alert on old lines after every restart. A file that appears later (missing at first) is read from its start. The sensor keeps the file open between collections: when the path names another inode (rotation by rename), it first reads the old file to its end, then switches to the new one from its start; when the file is shorter than the offset (copytruncate), it restarts from 0. At most 1 MiB is read per collection; the rest waits for the next one. Only the first 4096 bytes of a line are matched, the rest is skipped, and a line without its newline yet waits for it.

**Metrics.**

| Metric | Value |
| --- | --- |
| `log_matches` | Lines that matched since the previous collection |
| `log_lag_bytes` | Bytes written to the file and not read yet |

With label `path` and the configured instance. `matches` is a gauge of the last interval rather than a counter, because the threshold engine (ADR-0017) compares raw sample values and has no `rate()`: `monitorflow_log_matches > 0` alerts on any match, and the threshold duration (or a longer interval) gives "N matches in a window". A counter would also restart at every agent restart and configuration change. `lag_bytes` above zero says the file grows faster than 1 MiB per interval.

**Failure semantics.** A missing file is transient (logged by the scheduler, rate-limited) and then read from its start when it appears. A path resolving outside the log directories or to something that is not a regular file is permanent. Other I/O errors are transient.

**Matching lines are not forwarded.** The agent protocol has no log samples (ADR-0013 only writes server events to VictoriaLogs). Forwarding matching lines needs an additive envelope variant, server ingest and redaction rules, and is a follow-up. Even without lines, a pattern reveals whether matching lines exist, one bit per collection; the log directory allowlist is what bounds this, so administrators should keep it to logs the monitoring admins may see.

**Example.** `kind = "log"`, `name = "nginx_errors"`, `params = {"path": "/var/log/nginx/error.log", "pattern": "\\[(crit|alert|emerg)\\]"}`; threshold `monitorflow_nginx_errors_matches > 0`. Locally: `monitorflow-agent sensors probe log --param path=/var/log/syslog --param pattern=error`, which counts the lines written during the second between its two rounds.

## Consequences

- The server can at worst run, with chosen plain-word arguments, scripts the host's administrator installed. Arguments are the remaining channel, so scripts must treat them as untrusted input.
- A script installed or fixed after the configuration arrived runs once the configuration is resent or the agent restarts, because refused specs and permanent errors stop the sensor (ADR-0012). Retrying refused specs is a possible follow-up.
- Scripts run as the agent's user. Dropping privileges per script (a `User=` setting, systemd-run) is a follow-up if agents run as root.
- Lines written while the agent is stopped, or between the last collection and a configuration change (which rebuilds the sensor), are not counted. Persisting offsets is a possible follow-up.
- An administrator narrows or empties `sensors.log_dirs` to limit what the server can probe; the default `/var/log` includes `auth.log`, whose match counts the server can see.
- Stderr is discarded rather than logged, since it can contain anything; forwarding script output to VictoriaLogs needs agent log samples, which the protocol does not have yet.
- Tests write scripts through a child process: a test forking in parallel would otherwise inherit the write descriptor until its `exec`, and running the file fails with `ETXTBSY`.
