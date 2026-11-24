# HaloPSA

MonitorFlow can open a HaloPSA ticket for an incident and close it when the incident is resolved (ADR-0164, ADR-0166). This page says how to set it up and what the first version does.

The connection is set up by a global administrator under Settings › Integrations › HaloPSA, or through the public API (`/api/v1/psa`, tag `psa` in the API documentation).

## Create the API application in HaloPSA

1. In HaloPSA, create an agent for MonitorFlow, so that what MonitorFlow does in Halo is attributed to it. Give it only the permissions it needs: read clients and sites, create and edit tickets.
2. Go to Configuration › Integrations › HaloPSA API › View Applications, and add an application.
3. Authentication method: **Client ID and Secret (Client Credentials)**. Login type: agent, the agent from step 1.
4. Under Permissions, tick **all**: MonitorFlow asks for a token with the scope `all`.
5. Save, then copy the **Client ID** and the **Client Secret**. Halo shows the secret once.

The address to enter in MonitorFlow is the one of your Halo web interface, like `https://halo.example.com`, without `/api`. MonitorFlow signs in on `https://halo.example.com/auth/token` and calls `https://halo.example.com/api`. A hosted instance whose sign-in needs it also takes its tenant name.

**Test connection** signs in and reads the application's agent, without changing anything in Halo. The secret is sealed in the database (ADR-0125) and never shown again; to change it, enter a new one.

## Match sites to HaloPSA clients

Each MonitorFlow site is matched to a HaloPSA client and, if you want, to one of that client's sites. The lists come from HaloPSA, read through the connection, with a search on the client's name. An incident on a site without a match opens no ticket. A HaloPSA site can be matched to one MonitorFlow site only; a client can take several.

Changing the connection's address removes every match: the ids of another instance mean nothing.

## Tickets

Under "3. Tickets", choose the ticket type and the category of new tickets from HaloPSA's own lists (some instances refuse a ticket without a category), the least severity that opens a ticket (critical by default), and the status a ticket moves to when its incident is resolved (Closed by default, or none to add a note only).

- **Opening.** An incident at that severity, or one that becomes critical, on a matched site opens one ticket for the site's HaloPSA client and site. Its summary names the severity, the measure and the server; its details give the value, the site, when it opened and a link to the incident, and end with `MonitorFlow incident: <id>`. Only MonitorFlow's own fields are sent, never the AI's text.
- **Closing.** When the incident is resolved, MonitorFlow adds a private note to the ticket and moves it to the chosen status. It never reopens a ticket.
- **Refusals.** A 4xx from HaloPSA (a missing category, a permission) is kept, in HaloPSA's words, on the ticket's line and as the connection's last error, and is not retried: fix the setup. No answer, a rate limit (429) or a server error is retried, from 30 seconds to an hour apart, for a day.
- **No duplicates.** One ticket per incident. When an answer is lost after the request, the retry searches the client's tickets for the incident's id before creating another.

The sync follows the same events as webhooks (ADR-0102), at least once and in order per incident, and runs on every central server with leased claims.

## Not yet

These parts of ADR-0164 come later (ADR-0166):

- **Status from HaloPSA to MonitorFlow.** A ticket closed or assigned in HaloPSA does not acknowledge the incident yet; the poller will.
- **Assets.** Servers are not synced as HaloPSA assets yet. What an asset will receive is ready: the inventory (ADR-0187) gives each server's maker, model and serial number, shown as `asset` in `GET /api/v1/servers/{id}/inventory`, and `server_inventories.changed_at` tells which servers changed. The sync still needs its settings (an asset type per system family, the fields that receive each value), the matching of existing assets by serial number then host name with a confirmation, and a check on a trial instance.
- **Rate limits.** MonitorFlow backs off on a 429 like any failure; Halo's actual limits, and the lengths of summary and details, are to be checked on a trial instance.
- **Silences** do not hold tickets yet.
- **Customers.** Only sites are matched; no name suggestions and no default client for unmatched sites.
- **Priority, requester and team** are left to HaloPSA's defaults for the ticket type.
- **Finding a ticket after a lost answer** relies on HaloPSA's ticket search finding the incident's id in the details; this, and whether External Links are better, is to be checked on a trial instance.
