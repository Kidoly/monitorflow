# ADR-0069: Settings page

Date: 2026-10-31
Status: accepted; amends ADR-0054 and ADR-0056

## Context

Retention, the mirror address and session lifetimes could only change in `.env` or a mounted `server.toml`, and the server had several settings pages with nothing tying them together (simplicity audit, M11). Each of the four behaves differently:

- **Session lifetimes** were read once at start, and the idle timeout is checked on every request.
- **The mirror address** was read at start, and the compose file always set it, so a page could not tell a default from a choice.
- **The download URL** already had a screen, the custom install source of ADR-0054, but only install commands followed it: self-updates kept using `agent_download_url`.
- **Retention** is not the server's at all: it is the `-retentionPeriod` start flag of the VictoriaMetrics and VictoriaLogs containers, which the server cannot touch.

## Decision

**Administration › Settings**, for global administrators, has the public address, the default install source, sign-in, the package address and retention, then links to every other settings page. The existing pages stay where they are; the first two are the components of the tokens page, used in both places.

**Precedence is the same everywhere: configuration, then the web interface, then the default.** Whatever the configuration sets shows as locked, with the variable to remove, and the API answers 409 to a change. `api_session_lifetime` and `api_session_idle_timeout` become options to tell "set" from "default", and the compose file passes `SESSION_LIFETIME` and `SESSION_IDLE_TIMEOUT` so `.env` can lock them. Values equal to the default are stored as NULL, so they follow later defaults.

**The compose default of the mirror address moves to `mirror_default_url`.** It is `http://DOMAIN:MIRROR_PORT` as before, but it is a default and not a lock. `MIRROR_PUBLIC_URL` in `.env` keeps working, and now locks.

**Runtime values live in memory, in `Live`.** Sessions read them on every request and `self_update` payloads are built in synchronous code, so a database read at each use is not an option. Each server process reloads the row of `server_settings` every 10 seconds, as for AI (ADR-0064), and a process applies its own change at once.

**Sessions.** A shorter lifetime applies to open sessions in the same transaction: an administrator who shortens it to end long sessions means it. A longer one applies from the next sign-in. The interface accepts 1 hour to 30 days and an idle timeout of 5 minutes to 24 hours, no longer than the lifetime, so nobody can lock themselves out of the page that would undo it. The configuration is not bound by these limits.

**One download URL.** With the custom install source chosen, self-updates fall back to its URL too, or a site that moved off GitHub would install from its copy and update from GitHub. The updater's source of server releases stays `RELEASE_URL` and `AGENT_DOWNLOAD_URL` in `.env`: a copy of the agent files need not hold server images, and the updater reads no database.

**Retention goes through the updater,** the only container with the Docker socket, which already edits `.env` (ADR-0056). The page asks in days; the server drops `retention.json` in the inbox; the updater writes `UI_METRICS_RETENTION` and `UI_LOGS_RETENTION` in `.env` and recreates the two stores with `docker compose up`, their data staying in their volumes, then reports. The compose file reads `${METRICS_RETENTION:-${UI_METRICS_RETENTION:-90d}}`, so a value set by hand wins and the updater can tell the two apart. A failure puts `.env` and the stores back and the page shows why. A shorter value deletes older data, so the page asks first.

`.env` is the only place retention is stored. The updater reports it in `status.json`, so the page shows what the stores were started with, and a restore on a new host cannot show a value that is not in force. Without an updater reporting it, as outside the compose deployment, the page says so and offers nothing.

**Docs.** The three install guides open with a "Getting started" of the one-line paths, and lose what those paths replaced: the by-hand relay install, one-token-per-host automation, and the manual agent steps except on hosts without systemd.

Rejected:

- **Retention read-only on the page.** It is the setting most tied to a full disk, and it would keep a file edit in the path of the most common change.
- **Storing retention in `server_settings`.** The stores would then have two sources of truth, and a new host restored from a backup would show a value they do not use.
- **A shell wrapper in the stores' containers reading a file from a shared volume.** They would still need a restart, which only the updater can do.
- **A free-form retention value.** Days cover what the page needs; `.env` still takes `1y`.
- **Moving the existing pages under `/admin/settings/`.** Links, tests and the navigation would change for no gain; a list of links finds them.

## Consequences

- Upgrading changes nothing: unset values keep their defaults, and an existing `MIRROR_PUBLIC_URL` shows as locked.
- A second server process applies a change up to 10 seconds later.
- Changing retention interrupts metric and log writes for a few seconds; the server's write queues hold samples meanwhile, within their limits (ADR-0013).
- Changing retention needs a running updater, like updates and backups.
