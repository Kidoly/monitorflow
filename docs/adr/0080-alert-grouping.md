# ADR-0080: Alert grouping

Date: 2026-11-02
Status: accepted; the person on call gets the same groups as a channel (ADR-0081); follow-up steps are grouped the same way and wait for the same window (ADR-0082)

## Context

Every incident event is one message to every channel that covers it (ADR-0060, ADR-0078). When many things break at once, that is a flood. A site loses its network and forty offline incidents open within two minutes. An update breaks a service on thirty servers. A silence ends with every incident it held still open. ADR-0077 caps chat channels by gathering what is over a pass's count into "26 more notifications", which names only the first 15 and only fixes the flood within one pass. ADR-0079 left a wide silence's end to this decision.

Two parts are already settled and stay that way:

- **One incident per cause, for relays.** The offline rule opens nothing for a server behind a relay that is offline (ADR-0072). It had one gap. When the relay came back, its agents reconnected through it over the next seconds, and a check in between found each of them 5 minutes silent and opened one incident per server, resolved 30 seconds later.
- **Incidents stay one per server or relay.** History, the incident page, AI remediation and the fleet counts are per machine. Grouping changes what is sent, not what is recorded.

Escalation and on-call (ADR-0081) will build on what is sent and to whom, so this needs a small model they can extend.

## Decision

**When.** The dispatcher claims a customer's events together, in two steps, both in the claim query:

- **A quiet customer** (nothing claimed in the last minute) waits until its oldest pending event is 10 seconds old, then sends everything it has pending. The offline rule and the threshold engine write one check's events within a second; 10 seconds is enough to get them into one message even if a pass starts halfway through.
- **After a claim** the customer's next events wait until a minute after it. A customer is told at most once a minute, and what happens in that minute goes out together at its end. What was already written at the claim goes with it, so a pass that hit its 100-event batch finishes the claim at the next pass.

**The longest an event waits for grouping is 60 seconds, plus the 5-second poll: 65 seconds.** A lone alert waits 10 to 15 seconds. That is small next to what comes before it: an offline incident opens after 5 minutes of silence, and thresholds usually hold for minutes. Events of no customer (failed backups, automatic updates, new versions) and copies owed to one channel (ADR-0079) do not wait.

The window lives in the database, not in memory. It is the last `sent_at` of the customer's claimed rows, and pending rows stay pending. A restart in the middle of a window neither sends early nor loses anything. One index on `sent_at` serves the query and pruning.

**What goes together.** Per channel, and per customer, since a group never mixes customers, each incident event joins the first group it fits. A group needs two members:

1. **What a silence held back**, all of it, in one list. It replaces "one message per open incident" at the end of a wide silence.
2. **The same check on several machines**: same metric and instance, so `offline` across a site, or `service_nginx` after a bad update across three sites. A relay offline with servers behind it falls here too.
3. **The rest of one site.**

Problems (opened and escalated) and resolutions never share a group. An incident that opened and got worse within the window is told once, as it is now. Approvals and failed updates are not grouped: they are few and each needs its own action. They still follow the window.

**The cause, once.** The offline rule now counts the start of a relay's stream as heard from for the servers assigned to it. After the relay is back, they get the same 5 minutes as anyone to reconnect. A relay's offline message says what it hides: "Critical: relay gw-1 (paris) is offline; the 40 servers behind it cannot report". The dispatcher adds that count to the payload as `relay.servers`. A site without a relay that loses its link opens one incident per server, as before, and they become one message: "Critical: 40 servers are offline at acme / paris". The rule cannot tell a site's lost link from forty failed machines, and it would be the wrong place to guess.

**What a group says.** A title in plain words with the worst severity, how many machines, the check and where. Then one line per member, the line it would have had alone, and a link to the list in the interface.

| Group | Title |
| --- | --- |
| Offline | Critical: relay gw-1 and 12 servers are offline at acme / paris |
| Back | Resolved: 12 servers are back online at acme / paris |
| Same check | Warning: disk_used_percent /var on 8 servers in 3 sites of acme |
| One site | Critical: 5 alerts on 4 servers at acme / paris |
| After a silence | Critical after a silence at acme / paris: 7 incidents still open, 2 resolved |

**Per channel.** Each channel gets the group of its own members: ADR-0078's coverage filters events before grouping. A site channel sees its site's part of a customer group and never another site's. A channel for everything gets one group per customer. A silence holds events before claiming, as before, so silenced members never join a fresh group. They are named when it ends.

- **Email** lists every member in one message.
- **Slack, Teams and Discord** list at most 20 lines (12 on Discord), each cut to 150 characters. Discord's share is counted after escaping. Slack's list goes in sections of 10 lines under its 3000 characters. A fresh group ends with "and 29 more", which the link lists. A group of what a silence held back is owed (ADR-0079): every member is named, in as many messages as needed ("(2 of 3)"), never gathered. A rate limit copies those members for that channel's next pass, as before.
- **Webhooks keep one request per event.** Version 1 receivers follow incidents by ID, and ADR-0079 promises that a resolution reaches any tool behind a webhook. A new event that bundles incidents would reach existing receivers as an event they ignore. Their events still follow the window.

**Owed messages, counted in messages.** ADR-0079 let a release out at most the narrowest chat channel's count minus one, in rows. Grouped, that would trickle a 100-incident silence out four rows a minute. The release now counts the messages its rows will make: one per 12 incidents of a customer (the narrowest list), one per other event. That is 48 incidents per pass with Teams, so a 100-incident silence ends in two or three messages.

**Interface.** Each group sent is recorded in `notification_groups` (`id`, `customer_id`, `incident_ids`), shared by the channels sent the same members. Its link is `/incidents/?group=<id>`. `GET /api/incidents` takes `group`, still filtered by what the reader may see, and the incident detail lists `notification_groups`. The incidents list says the rows were "Sent together in one notification" with a way back to all. An incident page says "Sent together with 39 other incidents in one notification" and links to them. Groups are kept like incidents, with no pruning. Each is one row a minute at most per customer.

**For escalation and on-call.** A group is a set of incidents of one customer sent at one time to one set of channels, recorded with its ID. Escalation can follow a group's open members instead of each incident, and on-call can choose who gets a group. Neither needs another key.

Rejected:

- **Waiting before every event, as Alertmanager's `group_wait` does.** A minute before every critical alert to save messages that a quiet customer does not send. The first event of a quiet customer waits 10 seconds only.
- **A sliding window that waits for quiet.** A long outage would hold its messages for as long as it lasts. The window is fixed.
- **Grouping in the database triggers.** Groups depend on the channel's coverage and on silences, which are decided at delivery.
- **A site-down incident.** It would be a guess from timing, and it would be a second incident for each server, which the interface counts.
- **Grouping webhooks.** Above.
- **A setting for the windows.** One good default, as for the offline delay (ADR-0072).

## Consequences

- An outage of a site, a relay or a bad update is one message per channel and per minute, with the count, the worst severity and a link to the list. Its end is one more.
- Every alert reaches people up to 65 seconds later than before, a lone one 10 to 15 seconds later. Webhooks too, since they follow the same claim.
- ADR-0077's gathering still applies to fresh messages beyond a pass's count, which grouping makes rare. ADR-0079's "a released message is never gathered" holds for groups, and its "one message per open incident" consequence is replaced.
- A relay coming back no longer opens an incident for each server behind it. A server whose agent stays down is reported 5 minutes after its relay is back, not at once.
- Webhook payloads gain `relay.servers` on a relay's offline incident. Payloads stay version 1.
- Chat and email payloads of a group carry the event name `incident.group`. No webhook receives it.
- `notification_groups` grows by at most one row a minute per customer during outages and is not pruned.
