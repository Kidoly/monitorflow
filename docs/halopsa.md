# HaloPSA

MonitorFlow can open a HaloPSA ticket for an incident and close it when the incident is resolved, read the ticket back so that closing or taking it in HaloPSA acknowledges the incident, and sync servers as HaloPSA assets (ADR-0164, ADR-0166, ADR-0191). This page says how to set it up and what it does.

The connection is set up by a global administrator under Settings › Integrations › HaloPSA, or through the public API (`/api/v1/psa`, tag `psa` in the API documentation).

## Create the API application in HaloPSA

1. In HaloPSA, create an agent for MonitorFlow, so that what MonitorFlow does in Halo is attributed to it. Give it only the permissions it needs: read clients and sites, create and edit tickets, and read and edit assets if servers are to be synced.
2. Go to Configuration › Integrations › HaloPSA API › View Applications, and add an application.
3. Authentication method: **Client ID and Secret (Client Credentials)**. Login type: agent, the agent from step 1.
4. Under Permissions, tick **all**: MonitorFlow asks for a token with the scope `all`.
5. Save, then copy the **Client ID** and the **Client Secret**. Halo shows the secret once.

The address to enter in MonitorFlow is the one of your Halo web interface, like `https://halo.example.com`, without `/api`. MonitorFlow signs in on `https://halo.example.com/auth/token` and calls `https://halo.example.com/api`. A hosted instance whose sign-in needs it also takes its tenant name.

**Test connection** signs in and reads the application's agent, without changing anything in Halo. The secret is sealed in the database (ADR-0125) and never shown again; to change it, enter a new one.

## Match customers and sites to HaloPSA clients

A ticket goes to the HaloPSA client of the incident's site; else to the client its customer is matched to; else to the default client, if one is set; else no ticket is opened, and the ticket list says why.

- **Customers.** Under "2. Match customers to HaloPSA clients", each customer can be matched to a client. **Suggest matches by name** reads HaloPSA's client list once and proposes the client whose name reads the same once case, accents, punctuation and company forms (SAS, SARL, Ltd, Inc, GmbH...) are left out, when exactly one does. Nothing is matched until you confirm, one by one or all at once. A client matches one customer at most. A single company sees one choice instead: the HaloPSA client of this installation.
- **Sites.** Under "3. Match sites to HaloPSA clients", a site can have its own client and, if you want, one of that client's sites. The site of a matched customer can only take the customer's client, with one of its sites. A HaloPSA site can be matched to one MonitorFlow site only.
- **Default client.** Below the customers, "Sites matched to nothing" takes a client and optionally one of its sites, for incidents of sites with no match and whose customer has none.

The lists come from HaloPSA, read through the connection. Changing the connection's address removes every match, the default client, the priorities and the asset settings: the ids of another instance mean nothing.

## Tickets

Under "4. Tickets", choose the ticket type and the category of new tickets from HaloPSA's own lists (some instances refuse a ticket without a category), the least severity that opens a ticket (critical by default), the priority of warning and critical incidents' tickets (from the ticket type's SLA; none for HaloPSA's default), the status a ticket moves to when its incident is resolved (Closed by default, or none to add a note only), the statuses that mean closed (Closed by default), and whether a ticket assigned to a technician acknowledges its incident (on by default).

- **Opening.** An incident at that severity, or one that becomes critical, opens one ticket for the client found above, with the priority of its severity, and names the server's HaloPSA asset when it has one (an instance that refuses that gets the ticket without). Its summary names the severity, the measure and the server; its details give the value, the site, when it opened and a link to the incident, and end with `MonitorFlow incident: <id>`. Only MonitorFlow's own fields are sent, never the AI's text.
- **Silences.** A silence covering the incident, as it would cover its notifications, holds the ticket until the silence ends ("Held by a silence"). Then the ticket is opened if the incident is still open; an incident that opened and resolved during the silence gets no ticket.
- **Escalation.** An incident with a ticket that becomes critical adds a note and moves the ticket to the critical priority.
- **Read back.** Every minute, MonitorFlow reads its open tickets back from HaloPSA, one by one, up to 30 a minute, those read longest ago first. A ticket moved to a closed status, or assigned to a technician other than MonitorFlow's own agent, acknowledges its incident, if it is open and not acknowledged yet: reminders and follow-ups stop, and the incident page says who in HaloPSA, like "Alice Martin (HaloPSA)". The incident still resolves only when its measures recover. A ticket deleted or merged in HaloPSA ends its link ("Ended in HaloPSA").
- **Closing.** When the incident is resolved, MonitorFlow adds a private note to the ticket and moves it to the chosen status, unless it saw the ticket closed in HaloPSA already. It never reopens a ticket.
- **Refusals.** A 4xx from HaloPSA (a missing category, a permission) is kept, in HaloPSA's words, on the ticket's line and as the connection's last error, and is not retried: fix the setup. No answer, a rate limit (429) or a server error is retried, from 30 seconds to an hour apart, for a day.
- **No duplicates.** One ticket per incident. When an answer is lost after the request, the retry searches the client's tickets for the incident's id before creating another.

The sync follows the same events as webhooks (ADR-0102), at least once and in order per incident. It runs on the leading central server, as do the read back and the asset sync.

## Servers as assets

Under "5. Servers as HaloPSA assets", turn the sync on and choose, for Linux and for Windows servers, a HaloPSA asset type and where each value goes: the host name, the serial number, the maker and the model, each into the asset tag or one of the type's fields, or nowhere. A family without a type is not synced. The values come from the inventory (ADR-0187).

- **One way.** Servers of matched sites and customers become assets of their HaloPSA client and site. MonitorFlow never reads HaloPSA's assets into servers and never deletes an asset.
- **Existing assets.** A server is first matched among the active assets of its client and type: the same serial number, else the same host name in the type's key field. A match is only proposed ("To confirm"): confirm it, or create a new asset instead. Without a match, the asset is created, with a link back to the server's page in MonitorFlow.
- **Changes.** A server is looked at again at most once an hour, and its asset updated only when what it would receive changed. **Sync now** sends again without waiting for the hour.
- **Removed servers.** Their asset is left as it is, unless "Mark the asset inactive when its server is removed" is ticked.

## Rate limits

HaloPSA allows 700 requests in a rolling 5 minutes. MonitorFlow allows itself 600 per central server, leaving room for your other integrations: past that, a request waits for the next pass, and a page says when to try again. A 429 from HaloPSA pauses every request for the time it asks (at most 5 minutes). Reading tickets back takes at most 150 requests in 5 minutes.

## Not yet

- **Webhooks from HaloPSA.** MonitorFlow reads tickets back by polling; HaloPSA's webhooks to a route of MonitorFlow would be faster but are a new inbound route, for a later ADR.
- **Requester and team** of tickets are left to HaloPSA's defaults for the ticket type, and an acknowledgement in MonitorFlow adds no note to the ticket.
- **The ticket on the incident page.** The latest tickets are listed on this page and in the API; the incident page does not show its ticket yet.
- **A trial instance.** What HaloPSA's public documentation does not say (how a ticket names its asset, the field list of an asset type, finding a ticket after a lost answer) is handled safely by the code but not yet checked on a trial instance.
