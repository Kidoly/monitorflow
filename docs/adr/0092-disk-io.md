# ADR-0092: Disk I/O

Date: 2026-11-03
Status: accepted

## Context

The built-in sensors (ADR-0015, ADR-0048) say how full a disk is, not how hard it works. A server whose disk is saturated or failing looks healthy until something times out. Phase 13 asks for disk I/O on Linux and Windows at the level of cpu, memory and disk: on by default, with a sensible alert, readable on screen, and cheap enough for a large fleet. ADR-0015 left `/proc/diskstats` as a follow-up; ADR-0048 listed disk I/O counters among the Windows follow-ups.

## Decision

### A kind of its own: `diskio`

A new kind, `diskio`, rather than more metrics from `disk`. Space is per mounted filesystem, I/O per block device: one LVM volume group can span several disks, and one disk holds several filesystems, so the two do not share an instance. A separate kind can also be turned off on its own (a host with hundreds of multipath devices) without losing disk space alerts. It takes no parameters. It runs on both platforms and joins the built-in defaults (cpu, memory, disk, diskio), every minute; the defaults are code (`sensor_config::builtin_defaults`), so no migration is needed. `diskio` rather than `disk_io`, because the web finds a metric's sensor by the longest name that prefixes it (`disk_` would match both).

### Metrics

Per device, with the device as instance and no labels. All are averages over the time since the previous collection, so the first collection only primes the state, as for CPU usage.

| Metric | Meaning |
| --- | --- |
| `diskio_reads_per_second`, `diskio_writes_per_second` | Completed operations |
| `diskio_read_bytes_per_second`, `diskio_written_bytes_per_second` | Bytes moved |
| `diskio_busy_ratio` | Share of the time with at least one I/O in flight (iostat's `%util`) |
| `diskio_wait_seconds` | Average time a completed I/O took, queue included (iostat's `await`) |
| `diskio_queue_depth` | Average number of I/Os in flight (iostat's `aqu-sz`) |

Units are last, as ADR-0015 asks; `_per_second` is the unit of a rate. `wait_seconds` is 0 when nothing happened, and the whole interval when I/Os are in flight and none completed: a stuck disk then reads as very slow instead of idle, which the default alert catches.

### Linux: `/proc/diskstats`

**Which devices.** The names under `/sys/block`, which lists whole devices: disks (`sda`, `vda`, `nvme0n1`, `mmcblk0`), md arrays and device-mapper devices (LVM, LUKS, multipath). Partitions are not, so their I/O, already in their disk's, is not counted twice under another name; md and dm devices are, as their wait is what the filesystems on them see. Loop, RAM disks, zram, floppy and optical drives (`loop`, `ram`, `zram`, `fd`, `sr`) are skipped, and so is any device with no read or write since boot (the 16 idle `nbd` devices a loaded module creates). Without `/sys` (an unusual container), partitions are told by their names (`sda1`, `nvme0n1p1`, `mmcblk0p2`). At most 128 devices are reported, in kernel order, and the cap is logged once.

**Names.** The instance is the kernel name, except for `dm-N`, which takes its device-mapper name from `/sys/block/dm-N/dm/name` (`vg0-root`, `luks-…`, `mpatha`): the number changes between boots and tells an administrator nothing. A name already taken falls back to the kernel name.

**Counters.** Fields 4 to 14 of each line: reads and writes completed, sectors read and written (always 512 bytes), milliseconds reading and writing (summed into the time I/Os took), I/Os in flight, milliseconds busy, weighted milliseconds. Discard and flush fields are not used. Lines with fewer fields (partitions on 2.6 kernels) are skipped. Rates use the agent's monotonic clock between collections.

**Wrap, reset, hotplug.** The millisecond fields are 32-bit on every kernel and wrap after 49.7 days of busy time; every field is 32-bit on 32-bit kernels. A counter that went back from a value that fits in 32 bits is unwrapped; one that went back from beyond is a reset. A device whose busy time grew by more than the elapsed time (plus a quarter and a second of slack for jiffies) is a device replaced under the same name, not a wrap. A reset, a replaced device, or a device whose `major:minor` changed is primed again and reports from the next collection. A device that appears is primed; one that disappears is forgotten.

### Windows: `IOCTL_DISK_PERFORMANCE`

Each physical drive `\\.\PhysicalDrive0` to `63` is opened without access rights and asked for its `DISK_PERFORMANCE`, the counters behind PerfLib's PhysicalDisk object. PDH was the other option: it gives the same numbers through counter paths that are localized (`\Disque physique(*)\…` on a French system) unless `PdhAddEnglishCounter` is used, needs a query object and two samples, and loads `pdh.dll`; the IOCTL is one call per drive, needs no access rights on the drive (`FILE_ANY_ACCESS`), so it does not depend on the agent's account being an administrator (the MSI runs it as `NT SERVICE\MonitorFlowAgent`, ADR-0050), and maps onto the same counter model as Linux. The instance is `PhysicalDrive0`, as Windows names the drive (Disk Management says "Disk 0"). Bytes become 512-byte sectors, times go from 100 ns to milliseconds. Busy time is the reading's system time minus its idle time, whose increase is the busy time; the time reads and writes took is both the per-I/O wait and the queue over the interval, as PhysicalDisk's "Avg. Disk sec/Transfer" and "Avg. Disk Queue Length" compute them. Read and write counts are 32-bit and unwrap as on Linux. The calls run on the blocking pool, bounded by the deadline. Storage Spaces virtual disks appear as drives of their own; volumes are not mapped to drives.

### Default alert

One threshold, on wait: **warn when the average wait of a disk is over 200 ms for 10 minutes**, no critical, for every device. Busy share was the obvious candidate and was rejected: SSDs, NVMe drives and RAID arrays serve many I/Os at once, so they read 100% busy while still answering in a millisecond, and every nightly backup would warn. Wait measures what programs feel, whatever the device: a healthy SSD answers in under a millisecond and a busy hard disk in tens of milliseconds, so 200 ms held for 10 minutes means work is stalling, and a stuck disk (I/Os in flight, none completing) reads as a minute of wait per minute. Ten minutes, like CPU, keeps short bursts quiet. A disk under LVM or LUKS opens one incident for the disk and one for the volume on it; alert grouping (ADR-0080) puts them under one notification. Sites and servers change or turn it off like any default.

### On screen

The server's sensor list has one row per device, "Disk busy sda 12.4%", sorted next to "Disk used /". A disk's page shows its busy share with the usual chart and, below it, four more for the same device: "Read and written" (MiB/s), "Reads and writes" (per second), "Average wait per I/O" and "I/Os in flight". An incident shows on the row as "Disk wait 250 ms · since …". The alert reads "Warn when the average wait of a disk is over 0.2 s for 10 min." and its form takes seconds. In the list of checks, the sensor is "Disk activity". Rates format as `3 MiB/s` and `2.3/s`. Long ranges summarize wait, busy and queue by their maximum and the rates by their average (ADR-0020). No new route: the pages use the metrics API, which already scopes by customer and site.

### Cardinality

Seven series per device. A typical server has one to three devices (a disk, its LVM root, sometimes swap or a second disk): about 21 series, against roughly 35 for cpu, memory and disk on a four-core server with three mounts. At 1,000 agents that is about 21,000 more active series and 350 more samples per second at one-minute intervals, about 30 million points a day, a few tens of megabytes a day in VictoriaMetrics, which holds millions of active series on one node. The worst case, 128 devices, is 896 series for one agent; a host with that many devices (a SAN host with multipath) can turn the sensor off.

## Consequences

- Every server reports disk I/O after its agent updates, with no setup. Until then an older agent lists `diskio` as an unknown kind in its acknowledgement, retries it every few minutes (ADR-0024) and keeps running the other sensors.
- Thresholds on `_seconds` metrics now show and take seconds in the web interface.
- The first I/O point arrives one interval after the agent starts.
- Not done: per-partition or per-volume I/O, mapping Windows drives to their volumes (`C:`), discard and flush counters, a parameter to exclude devices, NFS and other network filesystem I/O (`/proc/self/mountstats`), and charts of several devices side by side.
