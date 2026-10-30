# ADR-0058: Daily backups and restore

Date: 2026-10-29
Status: accepted; amends ADR-0056

## Context

Backups of the compose deployment (ADR-0051) were manual: the guide said "back up `serverdata`" and gave a `pg_dump` command. Each server update took a database dump (ADR-0056), which nothing pruned, and restoring one took five commands. The simplicity audit (`docs/design/simplicity-audit.md`, M6) asks for a daily backup with retention, "Back up now" and "Download" in the interface, and one command to restore.

A useful backup has the database and `serverdata`: without the CA and the command key in `serverdata`, every agent and relay must enroll again.

## Decision

### The updater takes backups

The `updater` service (ADR-0056) already has the Docker socket, runs `pg_dump` in the `postgres` container and owns the `backups` volume. It now also mounts `serverdata` at `/serverdata`. No new container.

A backup is one file, `monitorflow-backup-<UTC time>-<kind>.tar.gz`, where the kind is `daily`, `manual` or `update`, written by GNU tar, which keeps owners and modes:

- `manifest.json`: format 1, time, kind, the server version and the latest migration;
- `database.dump`: `pg_dump -Fc`;
- `serverdata/`: the volume, without the mirror repositories (`mirrors/aptly`, `mirrors/dnf`, `mirrors/dnf-work`) and the agent release cache (`mirrors/agent`). Those are large and come back from upstream and from the next update. The mirror signing key and configuration are kept.

It is written next to its final name and renamed, so a listed file is complete.

- **When.** Five minutes after the updater starts if no backup exists, then 24 hours after the last one that succeeded, and an hour after a failure. Also when an administrator asks, and before every update, which stops if the backup fails, as the dump did.
- **Retention.** The 7 newest backups of all kinds are kept, and the older ones deleted after each success. The dumps of earlier updaters (`monitorflow-*-to-*.dump`) are counted by their modification time, so they go too. When the last update rolled back after migrating, its backup is kept whatever its age: it is the one to restore. There is no setting: 7 daily backups cost a few times the database size, and an operator who wants more copies them elsewhere.
- **Status.** `status.json` gains `backups`: the last run (kind, state, error or warning, file), when the last success finished, when the next one is due and the files kept. A failed backup stays visible there until one succeeds, and is logged. Notifications can read it from there. A run interrupted by a restart of the updater is marked failed when it starts again.

### The server lists, requests and serves them

- "Back up now" writes `inbox/backup.json` (an ID, who asked, when; unknown fields refused), as an update request does. The updater reads it without following links.
- The server mounts the `backups` volume **read-only** at `/var/lib/monitorflow/backups` (`backup_dir`), to serve downloads. ADR-0056 kept the backups out of the server; reading them gives it nothing it does not have, since it holds the database credentials and `serverdata`. It still cannot write, delete or replace a backup. The updater makes the directory `root:10001`, mode `2750`, and files `0640`. The server image's group is now fixed at gid 10001, like its user.
- `GET /api/backups`, `POST /api/backups` and `GET /api/backups/{name}` are for global administrators, as server updates are. A name is served only if it parses as a backup name. Requests and downloads are audited (`server.backup`, `server.backup.download`).
- **Administration, Backups** shows the last backup, the next one, the files with "Download", and says plainly that a backup holds the CA keys and every customer's data.

### Restore

`deploy/restore.sh FILE` runs `monitorflow-updater restore` in a one-off updater container, with the file mounted read-only. It reads the manifest and refuses a backup taken by a newer server than the image in `.env`. Then it stops `server` and `web`, starts PostgreSQL if needed, drops and creates the database, `pg_restore`s the dump, replaces `serverdata` (keeping the excluded trees), and starts every service with `up --wait`. It holds the updater's lock, so no backup or update runs meanwhile. It works on the deployment the backup came from, and on a fresh install: the server's identity and agents' records come back together, so agents reconnect. The one-line server installer will call it for `--restore`.

The updater still never restores by itself after a failed update (ADR-0056): the page names the backup and the command.

## Consequences

- Backups need no setup and are on in the production compose; restore is one command.
- A backup is as sensitive as `serverdata`. It sits in a volume or `BACKUP_DIR` on the same host, so it protects against mistakes and failed updates, not against losing the host. Downloading copies elsewhere is the operator's job; the page says so.
- Metrics, logs and mirror repositories are not backed up.
- Deployments whose compose file predates this ADR keep working. Their updater has no `/serverdata`, so backups hold the database only and say so in a warning. Their server has no `backup_dir`, so the page cannot offer downloads. Their PostgreSQL no longer needs the `backups` mount, which this compose file drops.
