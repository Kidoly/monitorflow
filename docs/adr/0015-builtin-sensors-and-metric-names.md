# ADR-0015: Built-in sensors, metric names and the sensor registry

Date: 2026-10-19
Status: accepted

## Context

The first built-in sensors (cpu, memory, disk) each measure several things with different units: CPU usage is a ratio, load averages are plain numbers, disk space is in bytes. ADR-0012 gives each sample one sensor name, set by the scheduler from the configuration, and ADR-0013 turns it into the metric name `monitorflow_<sensor>`. One configured `cpu` sensor would then produce a single metric mixing units, which Prometheus naming forbids and which makes queries and thresholds awkward. The configuration pushed by the server (a parallel task) also needs a way to turn a configured kind and its parameters into a sensor, and to refuse bad parameters before the first tick.

## Decision

**One sensor, several metrics.** A sensor sets `Sample.sensor` to a metric suffix (`usage_ratio`, `load1`) or leaves it empty. The scheduler sets the full name to the configured name, `_`, and that suffix (`cpu_usage_ratio`), or to the configured name alone when the suffix is empty, then checks it with ADR-0012's sensor name rules; a sample whose full name is invalid is dropped and counted as invalid. This amends ADR-0012, where the scheduler overwrote the name: a sensor still cannot leave its configured namespace, so a custom script cannot impersonate another sensor. `Sample::metric(suffix, value)` builds such a sample. The dimension within one metric is the `instance` (a core, a mount point), plus labels for descriptive attributes.

**Names.** Prometheus conventions, with the unit as the last word: `_bytes`, `_seconds`, `_ratio` (0 to 1, never percent). Gauges never end in `_total`, which Prometheus reserves for counters, so inode counts are `disk_inodes` and `disk_inodes_free`. Plain counts with no unit (load averages, inodes) have no suffix.

| Metric | Instance | Labels | Meaning |
| --- | --- | --- | --- |
| `cpu_usage_ratio` | `total`, `0`, `1`, … | | Busy share of the time since the previous collection |
| `cpu_load1`, `cpu_load5`, `cpu_load15` | | | Load averages |
| `memory_total_bytes` | | | `MemTotal` |
| `memory_available_bytes` | | | `MemAvailable` |
| `memory_used_bytes` | | | Total minus available |
| `memory_available_ratio` | | | Available over total |
| `memory_swap_total_bytes`, `memory_swap_free_bytes` | | | `SwapTotal`, `SwapFree` |
| `disk_total_bytes`, `disk_free_bytes`, `disk_available_bytes` | mount point | `fstype`, `device` | `statvfs` sizes; available is what unprivileged users can use |
| `disk_used_ratio` | mount point | `fstype`, `device` | Used over used plus available, like `df` |
| `disk_inodes`, `disk_inodes_free` | mount point | `fstype`, `device` | Omitted when the filesystem reports no inode limit |

**Kinds and the registry.** `registry::build(kind, params)` returns the sensor for a kind (`cpu`, `memory`, `disk`) or a `BuildError` for an unknown kind, an unknown parameter or a bad value. Parameters are strict: a typo is an error, not a silent default. Today the kind is the configured sensor name, so metrics are `monitorflow_cpu_*`; the configuration task may add an explicit kind later without changing the registry. `registry::KINDS` lists the kinds.

**CPU.** Usage comes from `/proc/stat` deltas between two collections, so the first collection only primes the state and emits load averages alone. Busy is user, nice, system, irq, softirq and steal; idle is idle and iowait; guest time is already in user. If the summed counters of a core go backwards, the core is skipped for one round and primed again; single fields may dip (iowait does on some kernels), so deltas saturate at zero. Cores that appear are primed, cores that disappear are forgotten (hotplug). Parameter `per_core` (default `true`) controls per-core samples.

**Memory.** Used is `MemTotal - MemAvailable`, as procps `free` computes it since version 4. Kernels without `MemAvailable` (before 3.14) are not supported by the agent.

**Disk.** Mounts come from `/proc/self/mountinfo`; pseudo and read-only image filesystems (proc, sysfs, tmpfs, devtmpfs, overlay, squashfs, cgroup, …) are skipped by default. Parameters `fstypes_include` and `fstypes_exclude` adjust the type list and `mounts_exclude` skips mount points and everything below them. A device mounted more than once (bind mounts, btrfs subvolumes of one filesystem show different devices and are kept) is reported once, at its first mount point. `statvfs` can hang on a dead network filesystem, so each call runs on the blocking pool and is bounded by the collection deadline; a mount whose call is still stuck is skipped, without a new thread, until the call returns. Mounts that time out are left out of the round and logged when they become stuck and when they recover; the others are still reported.

**Files.** `/proc` files are read with a size cap; a missing file is a permanent error, anything else (a parse failure included) is transient. Parsers take strings and are tested on fixtures of Debian and RHEL formats.

## Consequences

- Each built-in metric has one unit, so thresholds and graphs need no label filter on a metric kind.
- A sensor that emits one value per configuration (a port check) keeps leaving the suffix empty and gets the configured name, as before.
- Renaming a configured sensor renames its metrics, like before.
- The first CPU usage point arrives one interval after the agent starts.
- Disk I/O counters from `/proc/diskstats` are a follow-up.
