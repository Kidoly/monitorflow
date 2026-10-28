# ADR-0057: First-run setup in the browser

Date: 2026-10-28
Status: accepted

## Context

A new server has no user, and the only way to create the first one was `monitorflow-server user add` inside the server container. The login page did not say so: every attempt answered "Wrong username or password" (simplicity audit, B2). The person installing the server already has a shell on the host; what they lack is a path the interface shows by itself.

Letting the first visitor of an empty server create the administrator would be simpler, but the web interface is often reachable from a network before the installer has finished, and whoever got there first would own the fleet.

## Decision

While no user exists, the server issues a **setup link**: `<web_url>/setup/#<secret>`, with a random 256-bit secret.

- **Who gets it.** The server logs it at every start while no user exists (a `WARN` line with `setup_url`), and `monitorflow-server setup-link` prints a fresh one on the host. Both need access to the host or its container logs, which already allows `user add`: the link grants nothing the holder could not do on the command line.
- **One live link.** `setup_tokens` holds at most one row, with only the SHA-256 of the secret, like sessions. Issuing a link replaces the previous one.
- **Single use and short-lived.** It expires after 24 hours. `POST /api/setup` deletes the row as it checks it, so two requests with the same link cannot both succeed. A used, replaced or expired link answers 410 with the command that prints a new one. Creating any user, through the link or `user add`, deletes the row in the same transaction, so the link dies with the first account. After that, `/setup/` sends the browser to sign in and the API answers 409.
- **What setup does.** In one transaction: create the user with a global administrator role, create the site `main` under the default customer if no site exists, start a session, and audit each step (`setup.link_issued`, `user.create` and `site.create` by `system`/`setup`, then `auth.login`). The administrator lands on the fleet signed in, with a site to add servers to. `user add` with a global administrator role creates the same default site.
- **The secret stays out of request logs.** It travels in the URL fragment, which browsers never send; the page reads it, removes it from the address bar, and posts it in the body.
- **The login page tells.** `GET /api/setup` (no authentication) answers `{"pending": true}` while no user exists. The login page then replaces the form with "No account yet" and the command that prints a link.
- **Where the link points.** A new server setting, `web_url` (`MONITORFLOW_WEB_URL`), which the production compose file derives from `DOMAIN` and `HTTPS_PORT`. Unset, it is `https://` and the first `SERVER_NAMES` entry.

The setup link is the one secret the server writes to its log, as an exception to the rule that secrets never go in logs. It is acceptable because the log is only readable with host access, the link is useless once an account exists, and the alternative, a step that needs a guide, is what this change removes.

Rejected:

- **First visitor wins.** Simplest, but hands the server to whoever reaches it first.
- **A setup password in `.env`.** Another value to generate and type, which the installer would print anyway.
- **Keeping the secret only in memory.** It would avoid the table, but `setup-link` runs in another process and could not issue one.

## Consequences

- A fresh install goes from `up` to a signed-in administrator in the browser, with no command to look up. The one-line installer (Phase 8) prints the link with `setup-link`.
- `monitorflow-server user add` keeps working, before or after setup.
- Someone who can read the server's logs before setup can create the administrator. That person can already run `user add`.
- A server left without users for more than a day needs `setup-link` or a restart for a new link; the login page shows the command.
