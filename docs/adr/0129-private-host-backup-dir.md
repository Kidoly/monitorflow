# ADR-0129: A host backup directory belongs to root alone

Date: 2026-11-12
Status: accepted; amends ADR-0058

## Context

ADR-0058 lets the server download backups: the updater makes the backups directory `root:10001`, mode `2750`, and each backup `0640`, and the server, whose group is gid 10001, mounts it read-only.

In the `backups` volume, that is safe: the volume lives under Docker's directory, which no user of the host can enter. A security audit (2026-11-12, part 5) found that with `BACKUP_DIR`, a directory of the host, the same numbers apply on the host. Gid 10001 is where LDAP and SSSD groups often start, so the members of whatever group has that number on the host could read every backup: the database dump, the CA keys and the mirror signing key.

The updater cannot see the host's groups or the modes of the directory's parents, so it cannot tell whether gid 10001 is safe there.

## Decision

- Compose passes `BACKUP_DIR` to the updater as `MONITORFLOW_BACKUP_HOST_DIR`. When it is set, the updater makes the directory `root:root`, mode `0700`, and gives root's group to backups already there. New backups keep mode `0640`, and the group they inherit is now root's.
- The server cannot read the directory then. `GET /api/backups` reports `host_dir` and no downloads, a download answers 409, and **Administration, Backups** says the files are in `BACKUP_DIR` on the server. "Back up now", the status and restore are unchanged.
- The `backups` volume keeps ADR-0058's group and modes, and downloads.

Serving downloads through the updater would keep them for host directories, but the updater has no listener and would need a copy step and its own cleanup. An operator who picks a host directory can already reach the files.

## Consequences

- No host group can read backups in `BACKUP_DIR`, whatever gid 10001 is on the host.
- With `BACKUP_DIR`, downloads from the page stop; files are copied from the host directory. A compose file older than this ADR does not pass the variable, so its updater keeps the shared mode until the compose file is updated with the release.
