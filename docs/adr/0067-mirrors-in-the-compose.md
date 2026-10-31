# ADR-0067: Mirrors in the production compose

Date: 2026-10-31
Status: accepted; amends ADR-0030 and ADR-0056

## Context

Mirrors could not work in the production compose (simplicity audit, B7). ADR-0030 runs `aptly`, `reposync`, `createrepo_c` and `gpg` as local programs and expected them in the server image, which has none. Adding a mirror took a shell, a signing key command and a keyring filled by hand, and the mirror port was not published.

Running the real tools for the first time also showed that the command lines of ADR-0030, tested against fake programs only, did not work: `aptly` refused Debian's security archive, could not publish a component without packages, `reposync --arch=x86_64` left out every noarch package, and upstream groups and modules were dropped.

## Decision

### A sidecar image

A fourth image, `monitorflow-mirror`, holds the tools and runs as the `mirror` service, started by default. Keeping the tools out of the server image leaves the server small, and keeps `aptly` and `dnf`, which parse whatever an upstream sends, away from the CA keys: the sidecar mounts only the new `mirrors` volume, has no database access and sits on its own network, which reaches upstreams and nothing of the stack.

The server keeps running jobs, locks and the database. The `Runner` trait of ADR-0030 was made for this: `ToolsRunner` sends each command to the sidecar over a Unix socket in the shared volume, `<mirror_dir>/tools/tools.sock`, in a directory only the shared user (10001) can enter. One connection is one command: a JSON line with the invocation and its timeout, answered by a JSON line with its output. If the client goes away, the command is killed. Without the socket the local programs run as before, so an install with the tools on the host is unchanged and there is no setting to get right. The sidecar binary, `monitorflow-mirror-tools`, takes the directory as its only argument.

A socket in a volume shared by two containers of one host is not a network listener, so it does not add to the exceptions of "every link is mTLS".

Debian trixie, the base of the other images, packages all the tools for both platforms, but `reposync` only as a dnf plugin, so the image adds a small `reposync` command around `dnf reposync`.

Rejected:

- **The tools in the server image.** Simpler, but it doubles the image for an optional feature and puts archive parsers next to the CA keys.
- **A worker in the sidecar that takes jobs from the database.** It gives the sidecar database credentials and moves the locking, for no gain.
- **A TCP listener between the two.** It needs mTLS and certificates for a link inside one host.
- **Request files, as the updater does.** A sync takes hours; a socket gives the end of the command and of the client without polling.

### Storage

`mirrors` is its own volume, mounted at the default `mirror_dir`, `/var/lib/monitorflow/server/mirrors`, in the server, so no setting changes. It sits over the directory of the same name in `serverdata`, whose agent release cache the server fills again at the update installing this compose file (ADR-0056, step 7). The updater mounts it at `/serverdata/mirrors`, so backups keep what they kept: the signing key and the keyring, without the repositories (ADR-0058). `mirrors/tools` joins the exclusions.

The compose file publishes `MIRROR_PORT` (7480) and sets `mirror_public_url` to `http://DOMAIN:MIRROR_PORT`, or `MIRROR_PUBLIC_URL`, so the sources of agents work without a setting.

### Published with the release

The release workflow builds and pushes `monitorflow-mirror` like the other images, and `images.json` names it under `mirror`, so the signature covers it. It is optional when read: a release from before this ADR has no such key. `MIRROR_IMAGE` in `deploy/.env` joins the pins, unset for a local build. The updater pulls it, pins it, rolls it back with the others and recreates `mirror` with `server` and `web`, when the compose file has the service: one from before this ADR does not, and an update must not fail for that. A running sync is interrupted and its mirror synced again at the next interval.

### Adding a mirror in the interface

`GET /api/mirrors/catalog` lists the distributions an administrator can add, with an estimate of their size, the free space of the disk of `mirror_dir` and whether the tools answer. `POST /api/mirrors` with `{"distribution": "debian-13"}` adds one. There is no form: anything outside the list stays the CLI's `mirror add`.

A distribution is every repository its hosts need, one mirror each, because a host needs them together: the release, updates and security for Debian and Ubuntu, BaseOS and AppStream for Rocky and AlmaLinux. They are for amd64, synced daily, with a snapshot after each sync. Ubuntu leaves out `restricted`, several hundred GiB of kernel modules in `-updates`, and `multiverse`. Mirrors are created in one step, and the scheduler starts them within a minute, under `max_concurrent_syncs`.

The estimates are constants in `mirror::catalog`, added up from the upstream package indexes and rounded up. A live estimate would need upstream at the moment the page opens, and would change little.

A distribution larger than the free space cannot be added: its row says "Debian 12 (bookworm) needs about 125 GiB and this server has 60 GiB free.", and the API refuses it with the same sentence. The volume shares its disk with the database by default, so there is no override.

Everything else is done by the first add. The signing key is created if there is none. The sidecar adds the archive keys of its image to `upstream-keyring.gpg` at each start, keeping other keys, so a newer image brings a new Debian's key.

### Tool command lines

- `aptly mirror create` takes `-force-components`: Debian's security archive lists `updates/main` in its `Release` and serves `main`, as apt asks for it.
- `aptly publish snapshot` takes `-architectures`, needed for a component without packages.
- Contents indexes are no longer published: they take hundreds of MiB per snapshot and only `apt-file` reads them.
- `reposync` takes `--download-metadata`, which `createrepo_c --update` keeps, so groups, modules and advisories now come along. This settles the follow-up of ADR-0030.
- `--arch` is an exact match, so `--arch=noarch` goes with any architecture.
- `createrepo_c` writes gzip: the zstd default is not read by older dnf and yum.

## Consequences

- Mirrors work out of the box: three clicks in the interface, and no step on the host. The CI compose job proves it with a small signed upstream, real tools, and real apt and dnf clients installing from the result. The catalog's URLs and sizes are checked by hand, not in CI: a real sync takes hours and hundreds of GiB.
- The compose file gains a service, a volume and a published port. As ADR-0056 says, a release that needs another compose file must say so: this one's notes must. Until that file is updated, the server works as before and "Add a mirror" says the tools are not running.
- An updater older than this ADR ignores `mirror` in `images.json`, so the first update to this release leaves `MIRROR_IMAGE` unset. The next update pins it. An operator who takes the new compose file sooner sets it from `images.json`.
- Snapshots accumulate, as ADR-0030 said: without contents indexes a published snapshot costs tens of MiB of metadata and hardlinks a day, and packages removed upstream stay in the pool. Retention needs the list of snapshots in use and is still to do. Meanwhile the free space shown when adding a mirror is what an administrator has to watch.
- The size estimates age. They are rounded up, and the check is against them, not against the real need, so a disk can still fill up during a sync; the run then fails with the tool's error.
- Only amd64 is in the catalog. arm64 hosts need `mirror add`.
