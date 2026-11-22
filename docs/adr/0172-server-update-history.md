# ADR-0172: A history of the server's updates

Date: 2026-11-22
Status: accepted; extends ADR-0056 (updating the server from the web interface)

## Context

The server update page showed the last update only, because the updater's status file (ADR-0056) holds one run, replaced by the next. Administrators asked for the history of the server's versions there: which version, when, who asked, and how it ended. The rollout history of the automatic updates of agents (ADR-0055) is not it: it tells the releases agents were moved to, not the server's.

## Decision

**The server keeps each update in a table, `server_updates`, by the ID of its request.** A request from the web interface adds a row in state `requested`. The server then copies the updater's run into it whenever it reads the status file: when the page asks for it, and every minute with the look at failed backups the notification dispatcher already takes. An update started from the command line has no request; it gets a row the first time the server sees its run.

**Older updates come from the audit log.** The migration copies each `server.update` audit entry (who asked, when, from and to which version) with the state `unknown`. How such an update ended is inferred when the history is read: it succeeded when a later update started from the version it installed, or when it is the newest and the server runs that version. Otherwise it reads "unknown". The same applies to a row the updater never finished telling about.

`GET /api/v1/server-update/history` serves the newest updates, as the page shows them.

## Consequences

- The history survives the status file and the updater's replacement, and is in the database backups.
- A run that ended while no server was reading the status file, followed by another before the next read, is recorded with the state the server last saw; the inference above then usually tells how it ended.
- Updates made before the web interface could start them (by hand with the installer) are not in the history.
