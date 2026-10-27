# ADR-0056: Updating the central server from the web interface

Date: 2026-10-27
Status: accepted; implemented in stages (see the end)

## Context

The production compose (ADR-0051) builds the server and web images on the Docker host from a git checkout, and upgrading means `git pull` and a rebuild in a shell. Agents update themselves from signed releases (ADR-0053) and the server keeps a cache of those releases for them. Operators want the server to follow the same way: in the web interface, "Check for updates", then "Update".

Replacing the server's containers needs the Docker Engine API, which is root on the host. The server process is the most exposed part of the system: it serves the web interface's API and every agent's stream. Whatever performs the update must not give that process the Docker socket, and must not let it choose what runs.

## Decision

### Published images, covered by the release signature

The release workflow builds three images, for `linux/amd64` and `linux/arm64` on native runners, and pushes them to the GitHub Container Registry tagged with the version: `ghcr.io/kidoly/monitorflow-server`, `-web` and `-updater`. It records the digest of each multi-platform index in `images.json`:

```json
{"version": "0.2.0",
 "images": {"server": "ghcr.io/kidoly/monitorflow-server@sha256:…",
            "web": "ghcr.io/kidoly/monitorflow-web@sha256:…",
            "updater": "ghcr.io/kidoly/monitorflow-updater@sha256:…"}}
```

`images.json` is a release file like the others, listed in `SHA256SUMS`, so the release signature covers which images a release runs, by content. A reference must be `name@sha256:<64 hex digits>`: tags are refused, since whoever controls a registry can move them. Unknown keys are ignored, so a later release can add an image.

The compose file reads each service's image from a variable, `SERVER_IMAGE`, `WEB_IMAGE` and `UPDATER_IMAGE` in `deploy/.env`. Unset, they keep naming the local builds (`monitorflow-server:local` and so on), so `up --build` from a checkout still works for development and for sites that build their own images.

### Who performs the update: an updater container

A new binary, `monitorflow-updater` (`crates/updater`), runs as the `updater` service of the compose file, in an image with the Docker CLI and its compose plugin. It is the only container with the Docker socket. It has no listener and no other input than one file:

- The server and the updater share a volume, `update`. Its root belongs to root and only its `inbox/` subdirectory to the server's user. To ask for an update, the server writes `inbox/request.json`: a request ID, the target version and who asked. Unknown fields are refused, and nothing else decides what runs: not a URL, not an image.
- The updater downloads the release files of that version from its own setting (below), verifies `SHA256SUMS.sig` with the keys compiled into itself (ADR-0053), then `VERSION` and `images.json` against their lines, and refuses any version that is not newer than the running server's. Then it pulls the images by digest.
- It reports through `status.json` in the same volume: a heartbeat, the running version and images, and the steps, state and result of the last update.

A compromised server or web process can therefore ask for an update to a newer, genuine release, and nothing more: it cannot name an image, a source, an older release or a command. It can write only in `inbox/`, which the updater reads without following links, so it cannot plant a link where the updater writes, nor forge `status.json`. The updater's own state, including what to roll back to, lives in a second volume, `updater`, that the server does not mount, and so do the backups.

Rejected:

- **The Docker socket in the server.** One bug in the API would be root on the host.
- **A systemd unit or script on the host.** It would keep the socket out of every container, but it adds a host installation step to a deployment that is otherwise only `docker compose up`, needs its own update path, and needs the release keys on the host. The updater container gets the same isolation from the server with one compose service, and is itself updated by the release it installs.
- **The MonitorFlow agent on the Docker host, through a root helper as in ADR-0053.** It requires an agent on that host, enrolled in the server it updates, and couples the server's availability to an agent command that the server itself dispatches.
- **Watchtower and similar tools.** They follow tags, and do not verify a signature, back up the database or roll back.

### Sources

The updater and the server's check use the same base, computed by the same code (`common::server_update`): the `release_url` setting (`RELEASE_URL` in `deploy/.env`) when set, else `agent_download_url`. With the default `agent_download_url`, GitHub's `…/releases/latest/download/` gives the latest release and `…/releases/download/v<version>/` a given one; a self-hosted directory holds one release, whose `VERSION` must be the one asked for. `release_url` may be plain HTTP: as for agents (ADR-0053), the signature protects the files, not the transport. The updater reads the setting from its own environment, never from the request. The agent install source of ADR-0054, kept in the database, does not change it: the updater cannot read the database, and the server's check must use the source the updater will.

### The update

When it picks up a request, the updater:

1. **Verifies** the release as above. The running version is what `monitorflow-server --version` prints in the running server container, so a local build counts too; the same version is a no-op and an older one is refused, with no override.
2. **Downloads** the other files of the release (agent executables, installer files, MSI) into the `update` volume, each checked against `SHA256SUMS`, for step 7, and **pulls** the three images by digest.
3. **Backs up** PostgreSQL with `pg_dump -Fc` streamed from the `postgres` container into the `backups` volume (or the host directory `BACKUP_DIR`), as `monitorflow-<from>-to-<to>-<time>.dump`, and records the latest applied migration. The update stops if the dump fails. Dumps are not deleted; the operator prunes them.
4. **Pins** the new digests in `deploy/.env` (`SERVER_IMAGE`, `WEB_IMAGE`, `UPDATER_IMAGE`), written next to it and renamed, so a later `docker compose up` by hand keeps them. The previous values are kept in the updater's state.
5. **Recreates** `server` and `web` with `docker compose up -d --no-deps --no-build --pull never --wait`. The server applies migrations when it starts.
6. **Checks** that the server is healthy (its health check is `/api/health/ready`, which needs the database) and that it now reports the target version.
7. **Refreshes the agent release cache** with `monitorflow-server agent-release import` in the new server container, so agents can follow (ADR-0053; the rollout is ADR-0055's) and hosts can install from the server (ADR-0054). A failure here is a warning, not a failed update.
8. **Replaces itself** when `UPDATER_IMAGE` changed: a one-off container of the new updater image (`docker compose run`) recreates the `updater` service and exits, since a process cannot recreate the container it runs in.

To run compose against the operator's project, the updater reads the compose labels of its own container (project name, working directory, files and environment file), mounts the project directory (`.`), and links the host's path of that directory to the mount inside its container, so relative paths in the compose file resolve to the same host paths as when the operator runs it. Compose files outside that directory are refused.

**Failure and rollback.** If step 5 or 6 fails, the updater restores the previous pins and recreates `server` and `web` again. Migrations are forward-only and the previous server refuses a database that has migrations it does not know. When the latest applied migration changed, the result says so, names the backup and gives the restore commands (`docs/deploy-server.md`): stop the server, recreate the database, `pg_restore` the dump, start the previous version. The updater does not restore by itself: data written by the new server since the dump would be lost silently, and that choice is the operator's. When no migration ran, the rollback is complete.

If the updater restarts in the middle of an update, as after a host reboot, it rolls back on start when the new server was not yet confirmed healthy, and completes the update otherwise.

### API and web interface

Every route is for global administrators: the server is shared by every customer.

- `GET /api/server-update`: the running version, whether the updater is there (a heartbeat in the last two minutes), its running images, and the status of the last update.
- `POST /api/server-update/check`: fetches `VERSION`, `SHA256SUMS`, its signature and `images.json` from the source and verifies them as the updater will. Returns the latest version, whether it is newer, the release notes link (the GitHub release page when the source is GitHub), and what changes: the server version, whether the web and updater images differ from the running ones, and whether the agent release of that version is already cached. When the source cannot be reached, the answer says so, and the page explains the offline path.
- `POST /api/server-update`: body `{"version"}`, after a confirmation in the interface. Refused with 409 when the updater is missing or busy, 400 when the version is not newer than the running one. Writes `inbox/request.json` and audits `server.update` with the versions.

The page polls the status. While the server and the web container restart, requests fail for a while; the page keeps the last status, says it is reconnecting, and resumes. The status survives the restart because the updater, which does not restart, keeps it in its volume.

### Offline servers

Without Internet access the check fails and the page says so. The manual path: on a connected machine, download the release files, pull the images of `images.json` and `docker save` them; on the server, `docker load` them and run `docker compose run --rm updater update --dir /release` with the release files mounted. The same updater checks, backs up, updates and rolls back, without downloading. This needs Docker's containerd image store, the default since Engine 29, so that loaded images keep their digests.

## Consequences

- An administrator updates the server in two clicks; a release's images are bound to its signature, and neither the server nor the web container can run anything the release key did not sign.
- The `updater` container holds the Docker socket, which is root on the host. Its code is small, reads one file, and runs fixed compose commands; it is the part of the deployment to review most carefully.
- The first release with these images publishes packages to GHCR; the maintainer must make them public once (`docs/releasing.md`).
- The updater changes images, not the compose file: a release that needs another compose file (a new service, a new volume) must say so in its notes, and operators update that file by hand.
- The updater edits `deploy/.env`, the image lines only. An operator who manages `.env` with another tool must keep those lines.
- Rollback after a migration needs the operator, with the backup the update made; data written between the backup and the rollback is in no backup.
- Builds from a checkout can update to a published release; going back to local builds means removing the pins from `.env`, and is a downgrade if the release was newer.

## Stages

1. This ADR and `common::server_update`: `images.json`, the sources and the status and request files.
2. The updater, its image and the compose changes.
3. The images and `images.json` in the release workflow.
4. The server's API.
5. The web interface.
6. The end-to-end test in CI, the documentation and the roadmap.
