# ADR-0142: On-demand mirrors

Date: 2026-11-16
Status: accepted; amends ADR-0030, ADR-0034, ADR-0038, ADR-0067 and ADR-0074

## Context

A mirror copies every package of its distribution (ADR-0030, ADR-0067): about 150 GiB for Debian 13, 390 GiB for Ubuntu 22.04, and far more for the eight distributions of the catalog together than a homelab or a small company has. Relays that mirror need the same again (ADR-0034). Yet a fleet installs 1,000 to 3,000 packages of a distribution. The owner wants the mirrors of a whole fleet to fit in about 100 GiB, while every package of the distribution stays installable, as with a full mirror.

What makes a package installable through MonitorFlow is the signed index that lists it, not the package file being on our disk. The index files are small: measured on 2026-11-16, what an amd64 host reads (the `Release` file, one compression of each `Packages` index and the English translations) is 14 to 22 MiB for the release suite of Debian and Ubuntu and 0 to 11 MiB for their updates and security suites; the whole `repodata/` of a Rocky Linux or AlmaLinux repository is 44 to 156 MiB.

## Decision

Each mirror has a **mode**: `on_demand`, the default for everything added from now on, or `full`, the behavior of ADR-0030, unchanged. A global administrator chooses it when adding a distribution and can switch it later.

### What an on-demand sync downloads

Only the indexes, never a package. The server does it itself, in Rust, without `aptly` or `reposync`:

- **apt.** `dists/<suite>/Release` and `Release.gpg`, verified with `gpg --verify` against `upstream-keyring.gpg`, as aptly did: the run fails unless gpg reports a valid signature by a key of that keyring. Then, for each component and architecture of the mirror, one compression of `Packages` (xz, else gz, else plain), the `Release` of that directory and `i18n/Translation-en` when listed, each checked against the size and SHA-256 that `Release` gives. The server writes its own `Release`, listing only those files with the upstream's checksums, keeping the upstream's `Origin`, `Label`, `Suite`, `Codename`, `Version` and `Description`, its own date, and neither `Valid-Until` nor `Acquire-By-Hash`, as aptly's publish did: a snapshot pinned for weeks must not expire, and `by-hash` paths are not served. It signs it as `InRelease` and `Release.gpg` with the MonitorFlow key.
- **dnf.** `repodata/repomd.xml` and every file it lists, each checked against its size and checksum. `repomd.xml` is kept byte for byte and signed as `repomd.xml.asc`, as today. The upstream's `repomd.xml` is not verified by a signature, exactly as `reposync` did not verify it: the transport is https and dnf clients check each package's vendor signature.

Paths from any index follow ADR-0034's rules (relative, no `..`, no empty or dot segment, no backslash). Hosts see 100 % of the packages of the distribution in their indexes.

The sync also writes, beside the indexes, the snapshot's **package list**: for every package the indexes name, its path, size, checksum, name, version and architecture, sorted by path. It is what fetching on demand, prefetching and eviction read. It is hidden from the listener (`.packages`), and its presence is what makes a snapshot on-demand for the listener, the index and relays.

### Snapshots and the content store

A snapshot of an on-demand mirror holds only indexes. Index files are stored **once, by their SHA-256**, in a content store under `mirror_dir` (`ondemand/index/sha256/<2 hex>/<64 hex>`), and each snapshot's tree hardlinks them: the release suite's indexes, which never change, are shared by every snapshot, and the daily snapshots of an updates suite cost what changed that day. Snapshot names, `current`, the URL layout, campaign pinning (ADR-0038), relays' copies (ADR-0034) and retention (ADR-0074) keep their meaning: retention keeps the same snapshots for the same reasons, and removing an on-demand snapshot deletes its directory; an index file then goes with its last link.

Packages are stored by their checksum too (`ondemand/packages/<algorithm>/<2 hex>/<hex>`), never in a snapshot tree: the same package file serves every snapshot and every mirror that lists it.

### Fetching on the first request

When the listener gets a request for a file that is not in the snapshot's tree, and the snapshot has a package list naming that path, the server:

1. serves it from the content store if it is there;
2. otherwise fetches it from the mirror's upstream, at the same path below its URL, writes it to a hidden temporary file while counting and hashing every byte, cuts it off past the listed size, and keeps it only if its size and checksum are those of the package list, which comes from the signed index of the snapshot being served. Only then is it served. A file that does not match is deleted and the client gets 502;
3. answers 404 for a path the package list does not name: the listener fetches only what a signed index lists, so it is not an open proxy.

Concurrent requests for one file share one download: the first starts a task that does not depend on its client, the others wait for its result, and every client reads the stored file. A download is not cut short by its client going away, so a client that times out on a very large package finds it on disk when it retries. Downloads toward upstreams are bounded: at most 8 at a time per server or relay (others wait their turn), 30 s to connect and 600 s without progress, as relays have since ADR-0034. The package is not served while it downloads: verifying first is the security property, and a package of a few hundred MiB over a slow upstream may exceed a client's own timeout once.

### Old versions removed upstream

Debian and Ubuntu delete superseded package files from their pools, and Rocky Linux and AlmaLinux move a minor release out of the main tree when the next one comes. A campaign pinned to an older snapshot would then find nothing upstream. When the upstream answers 404, the server tries, in order, the distribution's own historical archive for the same relative path, and keeps the first answer whose checksum matches the package list:

| Distribution | Upstream | Historical archive | Checked on 2026-11-16 |
| --- | --- | --- | --- |
| Debian 12, 13 | `deb.debian.org/debian` | `snapshot.debian.org/archive/debian/<time>/` | Release files and pool paths resolve |
| Debian 12, 13 security | `deb.debian.org/debian-security` | `snapshot.debian.org/archive/debian-security/<time>/` | Release files and `pool/updates/` paths resolve |
| Ubuntu 22.04, 24.04 (release, updates, security) | `archive.ubuntu.com/ubuntu` | `snapshot.ubuntu.com/ubuntu/<time>/` | Release files and pool paths resolve; the service answered 5xx now and then, so a 5xx is retried once |
| Rocky Linux 9, 10 | `dl.rockylinux.org/pub/rocky/<major>/` | `dl.rockylinux.org/vault/rocky/<major>.<minor>/` | Same relative paths, packages resolve (9.5, 10.0) |
| AlmaLinux 9, 10 | `repo.almalinux.org/almalinux/<major>/` | `vault.almalinux.org/<major>.<minor>/` | Same relative paths, packages resolve (9.5, 10.0) |

`<time>` is the time the snapshot was synced, in the archives' own `YYYYMMDDTHHMMSSZ` form, which gives the archive's state at that moment; then the same time plus a day, for a package published between the archive's last state and our sync. The minor release is read at each sync from the tree's `.treeinfo` (`version = 9.8`) when it gives one, as Rocky Linux's does; AlmaLinux's says `9`, so its vault is tried from the newest minor down. At most 24 requests are spent on one missing package, and the checksum decides, so a wrong candidate costs a request and nothing else.

The archives are described per catalog distribution in `mirror::catalog`. A mirror added with `mirror add` has no archive: a package gone from its upstream and never fetched is 404, and the server logs it. So is a package the archive itself lacks, which for these distributions means a package withdrawn from the archive on purpose.

### The storage cap and eviction

Everything under `ondemand/` counts against one **cap**, which a global administrator sets on the Mirrors page, where it is shown with what is used. The default is 100 GiB or half the size of the disk of `mirror_dir`, whichever is smaller. Full mirrors are outside the cap: their size is what the administrator chose by choosing full.

Only packages are evicted; indexes are what make the mirror work and are removed by retention. When the store is over the cap, the least recently used packages go first, by the time they were last served (the file's modification time, which serving refreshes at most once an hour), except:

- packages listed by a snapshot pinned by a campaign that is not final, drafts included (the `.pinned` file of ADR-0038, which the campaign runner already keeps): its next wave needs them, and they were prefetched for it;
- packages listed by a mirror's `current` snapshot that were served or fetched in the last **14 days**. Fourteen days is twice the 7 daily snapshots retention keeps (ADR-0074): a package the fleet is still installing from the current state, typically an update that servers on a weekly maintenance window take one after the other, or one a host offline for a week still needs, stays; a package nobody asked for in two weeks is cheap to fetch again.

Eviction runs at the end of each retention pass (hourly) and whenever a download takes the store over the cap, at most one pass at a time. The cap can be exceeded by the downloads in flight, and by protected packages alone: then nothing protected is removed, the pass logs it, and the Mirrors page says the cap is too small for what campaigns and the current snapshots hold. The cap is never enforced by refusing to serve a package.

### Prefetching

Two moments know in advance what will be installed:

- **A campaign is pinned.** Within 10 minutes of a campaign with pinned snapshots being created, and every 10 minutes while it is not final, the server fetches into the store the pending updates of its target servers that the pinned snapshots list. A draft waits for its window anyway, so a few minutes cost nothing, and a pass over packages already stored only reads their files.
- **After each sync** of an on-demand mirror, the pending updates of the whole fleet that its new `current` lists, so that the next morning's automatic updates come from the disk.

The pending updates are those agents report (`server_packages.upgradable_version`, ADR-0031), matched by name, version and architecture against the package list; no server-to-mirror mapping is needed, since a version string names one distribution's package. Prefetches share the bounded downloads and the deduplication of requests, two at a time, leaving most downloads to clients' requests. A prefetch that fails is logged and left to the first request.

### Relays

A relay does the same in cascade. `/index.json` gains `on_demand: true` on a mirror entry (and on a pinned snapshot) whose snapshot has a package list, omitted otherwise, so the format stays version 1. For such a snapshot the relay copies the signed files and the index files as before, with the same checks, staging, hardlinks and swap of `current`, writes its own package list from the `Packages` and `primary` lists it already reads, and downloads no package. On a request it does not hold, it fetches the file from its upstream's listener at the same URL, which is the server or a parent relay that does the same, and verifies it against its own package list before serving and keeping it. Its store, cap and eviction are the server's code: `mirror.cache_max_bytes` (default: 100 GiB or half its disk), with the same 14 days and its own `.pinned`.

A relay older than this ADR ignores `on_demand` and copies the snapshot as before: it requests every package, which the server fetches from upstream and serves. It keeps working with a full copy, at the cost of the whole distribution passing through the server's store, which eviction bounds. A relay that updates itself (ADR-0055) leaves this state at its next update; others need theirs.

### Switching modes

The mode is a property of the mirror for its next sync, and each snapshot records the mode it was built in (`mirror_snapshots.mode`), so both kinds coexist and are served and retained each by its own rules.

- **On demand to full.** The next sync runs `aptly` or `reposync` as ADR-0030 says, which downloads every package, and its snapshot becomes `current`. On-demand snapshots still in use stay served until retention removes them.
- **Full to on demand.** The next sync builds an on-demand snapshot. Full snapshots that nothing uses (ADR-0074's reasons, without the count of 7) are removed at the next retention pass rather than after a week, and `aptly db cleanup` or the removal of dnf trees frees their packages, so the disk comes down to the cap. Packages are not copied from the full tree into the store: hashing hundreds of GiB to keep a few GiB is not worth it, and prefetching after that sync brings back what the fleet needs. Once no full snapshot remains, the aptly mirrors or the dnf working tree are dropped.

Switching is refused while the mirror is being removed, and otherwise takes effect at the next sync, which the page offers to start. It is audited as `mirror.mode`. Changing the cap is audited as `mirror.cache_cap`. Fetches, prefetches and evictions are not audited, like scheduled syncs: they are frequent, change no state anyone chose, and are logged.

### Adding a mirror

Each distribution of "Add a mirror" shows both sizes side by side, "about 150 GiB full, about 5 GiB on demand", with "On demand" selected. The on-demand figure is the indexes of 7 snapshots plus 3,000 packages, the high end of a fleet's working set, for that distribution. A distribution is refused for lack of space only when its full size does not fit in full mode, or its indexes do not fit in on-demand mode.

### What on demand cannot do

With the upstream unreachable, a package that was never fetched is not available: the client gets an error from the mirror and its install fails, while every package already fetched or prefetched installs as usual. The interface says this next to the mode, and a site that must install anything without Internet access chooses full. A relay cut off from the server serves what it holds, the same way.

### Rejected

- **Trimming the full mirror only** (amd64 only, no sources, debug packages or translations). The catalog already takes amd64 without sources; what is left to trim is 10 to 20 %, far from 100 GiB for a fleet.
- **Mirroring only the packages the fleet has installed, plus their dependencies.** It breaks `apt install` or `dnf install` of a package no server has yet, which is the first thing an administrator does on a new server, and the indexes would have to be rewritten to list only those packages, so the upstream's signature could not be kept even in principle.
- **A caching HTTP proxy in front of the upstream.** Without our indexes, snapshots and pinning would be lost; and caching the upstream's mutable index files cannot freeze a state for a campaign.
- **Streaming a package to its first client while it downloads.** Faster for very large packages, but a client could then receive bytes that turn out not to match: every file is verified before it is served.

## Consequences

- A fleet's mirrors fit in the cap: a few GiB of indexes per distribution and the packages servers actually install, instead of hundreds of GiB per distribution, on the server and on each relay.
- Installing a package for the first time depends on the upstream, or on the server for a relay, being reachable at that moment. Full mode remains for sites that need independence.
- The first install of a package waits for its download from the upstream; later ones, on any server, come from the disk.
- The server, not only its tools sidecar, now reaches the upstreams over https. In the production compose it already has outbound access on the `front` network.
- An on-demand sync uses no `aptly` or `reposync`, so the tools sidecar is needed only for full mirrors and for gpg; signing still goes through it.
- Packages removed from an upstream with no historical archive cannot be fetched again, so a campaign pinned to an old snapshot of a mirror added outside the catalog may find a package missing. Prefetching at pinning time is what protects it.
