# ADR-0061: Update available, and release import in the interface

Date: 2026-10-31
Status: accepted

## Context

Nothing tells an administrator that a release exists: the update page (ADR-0056) only checks when someone clicks, and ADR-0053 and ADR-0054 left the background check and `agent-release fetch` unbuilt. Servers without Internet fill the agent release cache from a shell, with `agent-release import` run inside a container (ADR-0054), which is the last step of the update story that needs a terminal for a routine job (simplicity audit, M7, M8).

## Decision

### The daily check

A task on the server asks the release source once a day, a minute after the start, then again an hour after a check that got no answer. It uses the code and the source of the check button (`common::server_update`, ADR-0056): `SHA256SUMS`, its signature, `VERSION` and `images.json`, verified with the built-in keys. So the banner only announces a release the updater would accept.

A verified answer goes in a new table, `seen_releases`, one row per version, `first_seen_at` and `last_seen_at`. The check button records its answer the same way. The newest `last_seen_at` is what the source offers today; `GET /api/server-update` shows it as `available` when it is newer than the running server, so the banner ends when the server catches up, and also if a release is withdrawn.

A failed check changes nothing. It logs at debug level and shows nowhere: on a server without Internet that is the normal state, not an error, and nobody can act on it.

### The banner

Global administrators see one line above every page: the new version, the running one, and a link to the update page, where the check runs by itself on arrival, so what the update changes and "Update to …" are already there. It hides on that page and while an update runs. Customer administrators do not see it, since they cannot update the server. There is no dismiss button or setting: the line is short, and it goes away with the update.

### The notification

The first time a version newer than the running server is seen, the same transaction adds `update.available` to the notifications outbox (ADR-0060). `seen_releases` is what makes this once per version: the outbox is pruned after a week, so a unique index there, as for failed backups, would repeat the notice weekly. The event is server-wide, so it goes only to channels for all customers, and links to the update page.

The webhook payload gains one object, `release`: `version`, `running_version`, `release_notes_url`. Its `text` reads "MonitorFlow 0.3.0 is available; this server runs 0.2.0".

### Release import in the interface

A box on the update page imports a release into the cache, for global administrators, as `agent-release import` does, from either:

- **Files.** `POST /api/agent-releases/import`, multipart, one part per file: the administrator chooses every asset they downloaded. The file name selects the part; names a release does not keep are skipped, so choosing everything works and no path is ever built from client input. It is the only route over the API's 64 KiB body limit, at 2 GiB, and is read only after the session check.
- **A URL.** `POST /api/agent-releases/import-url`, body `{"url"}`: a directory of a web server of the site. The server downloads the kept files; a 404 means the release lacks that file. Plain HTTP is accepted as for `release_url`: the signature protects the files, not the transport. This also stands in for `agent-release fetch`, which stays unbuilt. A server with Internet needs neither: an update caches its own release.

Both write to `<mirror_dir>/agent/.upload`, on the disk of the cache and hidden from the listeners by its dot name, and then call `ReleaseCache::import`, which verifies the signature and every file. One import runs at a time, since they share that directory, and each starts by emptying it, so a restart leaves no lasting trace. Refusals are sentences for the form ("These files are not a genuine MonitorFlow release: …"). The import is audited as `agent_release.import` with the version, the missing files and `upload` or the URL's host, not the URL, which may embed a token.

This covers agents and relays. Updating the server itself offline still needs its images, `docker load` and the updater on the host (ADR-0056), so the page names that path in one line rather than offering an upload it cannot finish.

The token page, the automatic updates page and the 404 of `/agent/latest/…` now point at this box instead of the command. The command stays, for scripts.

Rejected:

- **Fetching releases into the cache on the daily check.** It would download a release that may never be used, and the update already caches its own.
- **A release archive to upload.** It would need building for releases already published, and choosing many files works in every browser.
- **A setting to turn the check off.** It sends no data but the request for four public files, and a server without Internet stays silent anyway. `release_url` already says where it goes.

## Consequences

- An administrator hears of a release on the page they are on and through their channels, once, and is two clicks from installing it.
- A server without Internet shows no update errors; it shows the import box.
- An imported release newer than the fleet starts automatic updates if they are on (ADR-0055), as the command does.
- The API accepts one large body, for global administrators only, which needs that much free space in the mirror directory.
- The URL import lets a global administrator make the server fetch an internal address. As with webhooks (ADR-0060), they already control the server, and only a validly signed release is kept.
- A daily request to the release source from every server. A self-hosted source sees it too.
