# ADR-0079: Silences

Date: 2026-11-02
Status: accepted; what a silence held back is sent together, and a release counts messages, not rows (ADR-0080); a follow-up step due during a silence waits for its end plus the step's wait (ADR-0082)

## Context

Planned work sets off alerts: a server restarts, a disk fills during a migration, a relay is moved. Notifications (ADR-0060) reach people outside the interface, so during known work they page someone for nothing. Nothing today stops them for a while. The only switch near it is `offline_alerts` (ADR-0072), which stops offline incidents from opening, resolves those open, and stays until someone turns it back on. It answers another question: "stop watching this machine", not "keep watching, tell nobody until 15:00". Used for maintenance, it hides a real outage from the interface and from history, and it is forgotten on.

## Decision

**What a silence is.** A row of `silences`: a scope, a start, an end, a reason and who created it. The scope is a server, a relay, a site, a customer, or everything. A relay is in the list since ADR-0072 lets it be turned off on its own: without it, moving a relay would mean silencing its whole site.

**It stops notifications only.** The engine, the offline rule and the AI remediation loop do not know silences exist: incidents open, escalate and resolve as usual, relays still speak for the servers behind them, and an AI command runs if the rules allow it. Only the dispatcher reads them. A silence covers every event of its scope: incidents, approvals and failed updates. Events of the whole central server, like a failed backup, have no scope, so only a silence of everything covers them.

**It ends on its own.** `ends_at` is required and at most 30 days after the start, in the API and by a CHECK in the database. For longer, the machine is off and needs `offline_alerts` off or removing. "End now" sets `ended_at` and keeps the planned end; before the start it calls the silence off.

**Boundaries.** A silence runs from its start, included, to its end, excluded, on the server's clock. Dates in events are not used: an offline incident is dated from the last message, minutes before anyone could have known, and sample times are the agent's. What counts is when the dispatcher gets to the event, at most 5 seconds after it was written: an event still pending when the silence starts is held, and an event written during a silence is held even if the server was down until after it ended.

The dispatcher holds an outbox row by setting `held_at`, and writes a `silenced` event on the incident's timeline. Holding and claiming are one transaction, so a crash loses nothing and nothing slips through between them. Once no silence covers a held row, it is settled. Overlapping silences hold until the last one ends. For an incident, all its held rows settle together into at most one message:

| During the silence | At the end | Sent |
| --- | --- | --- |
| Opened (or got worse) | Still open | One message with its severity and value now: "Still open after a silence. Critical: …" |
| Told before the silence, resolved during it | Resolved | The resolution: "Resolved: … (during a silence)" |
| Opened and resolved | Resolved | Nothing. The timeline and history keep it |
| Told before the silence, nothing new | Still open | Nothing: it was already told, and the next change is sent as usual |

An incident still open is sent whatever its severity. A warning would have been sent without the silence, and warnings are not repeated, so holding it forever would lose it; "a critical problem must be sent" is the same rule. An incident that opened and resolved during the silence is left out because that is what a silence is for: a machine restarted during maintenance went down and came back, as expected. It is not lost: it stays in the incidents list with its dates, marked as silenced, and its timeline says nothing was sent. A resolution of an incident already told is sent, or the channel, and any tool behind a webhook, would keep it open forever.

Other events are not incidents with a state to look at. A command still waiting for approval is sent; one decided or expired meanwhile is dropped, as nobody can act on it, and the command did not run. Anything else, like a failed update or backup, happened once and nothing else would tell it, so it is sent as it is. The silence may have lasted longer than the day after which events are dropped (ADR-0060): a released row counts as written at the end of the silence.

All this is in the database, so a restart during a silence, or at its end, loses nothing.

**With routing and chat channels.** A silence is decided before routing: it holds an event back from every channel, whatever channel would have taken it. A released event is an outbox row like any other, so ADR-0078 routes it by its customer and site, and ADR-0077 paces it. Two of ADR-0077's rules would break the promise above, so a released message is **owed**, marked by its `silence` field:

- **It is never gathered.** A chat channel sends at most a few messages a pass and gathers the rest into one that names only the first 15; an incident still open at the end of a silence must be named. Owed messages go one each, and only fresh events are gathered, in the room left. To keep a pass to the service's count, a release lets out at most the narrowest chat channel's count minus one per pass (4 with Teams, counting owed messages not yet sent); the rest stay held, in the database, for the next pass, five seconds later. Without chat channels a release goes at once.
- **A rate limit does not lose it.** A chat channel still limiting after three tries gets nothing more that pass. The owed messages it did not take are copied into the outbox for that channel alone (`channel_id`), and the next pass sends them, with the same pacing and the same 429 handling. A copy keeps the time of its release, so a channel limiting for a whole day loses it like any event that old. A copy has its own ID, so a webhook would see another `X-MonitorFlow-Delivery`; only chat channels are limited this way, and they show no ID. Other failures keep ADR-0060's rule: three tries, at most once.

While an incident has rows held, a new event of it waits with them, silence or not, so that a "Resolved" never goes out before the "Still open" a paced release still owes.

**Messages.** Webhook payloads keep version 1 and the event names of ADR-0060, so receivers need no change. A held event gains `"silence": {"ended_at": "…"}`. A message that sums up an incident keeps the event of its first held row, `incident.opened` when nobody was told it opened and `incident.escalated` otherwise, with `severity`, `value` and `peak_value` as they are at the end. Its `text` says it comes after a silence.

**Signs.** `GET /api/incidents` adds `silenced_until`, when the last silence now covering an open incident ends, and `silenced`, whether a silence ever held a notification back. The timeline adds the kinds `silenced` and `silence_ended`, the latter with `notified`. Server and relay views add `silenced_until`. The incidents list, the incident page and the fleet show the sign, and a silenced server, relay or site says so in a banner at the top of its page, with the reason, who created it and a way to end it.

**Interface.** Those pages carry "Silence for: 1 hour, Until tomorrow, 8:00, Custom…". The first two take one click and say "Planned maintenance"; the reason shows in the banner, and Custom lets people say more or start later. While silenced, the same buttons read "Silence longer": a second silence overlaps the first. A Silences page, under Monitoring, lists active and upcoming ones, and is where a whole customer or everything is silenced.

**Reconciling with offline alerts.** They stay, as they are, and are not a kind of silence. They answer "is this machine watched?", permanently, and belong with turning a threshold off (ADR-0057). A silence answers "is anyone told?", for a while. So that the screen keeps them apart, "Mute" becomes "Turn off" and "Muted" becomes "Off", the words already used for a threshold, and "silence" means only this; `muted` stays as an internal reason. A switched-off machine turning into a permanent silence would break "every silence ends", and a silence that closed offline incidents would hide outages.

**Who.** Like the settings of what it covers: operators and above of the site for a server, relay or site (ADR-0057), operators and above of the whole customer for a customer, and global administrators for everything, as it reaches every customer and the central server's own events. Ending one takes the same rights. A user sees silences of its sites, of a customer it has a site or a grant of, and of everything, since those cover its sites; never another customer's or, with a site grant, another site's. `can_end` tells the interface. Creating and ending are audited as `silence.create` and `silence.end`.

Rejected:

- **Dropping events during a silence and sending a summary of what is still open at the end, from the incidents table.** It needs to know what each channel was already told, or it repeats every open incident after every silence. The held rows are that knowledge.
- **Sending one message per held event at the end.** Replays an outage in order after the fact, with stale severities.
- **Deciding in the trigger that writes the outbox.** It would decide when the event is written and could not look again when the silence ends; the rules belong in one tested place.
- **Sending a summary of what opened and resolved during a silence.** More to read for what the silence was meant to hide. Alert grouping may bring one.
- **Silencing per metric or severity.** Options that a "known work on this machine" does not need.
- **Stopping AI remediation during a silence.** "Silence" promises less noise, not less action; a pause would be its own decision.

## Consequences

- Maintenance needs one click and pages nobody, with a banner so that nobody forgets, and nothing has to be turned back on.
- A wide silence ending can send one message per open incident. Alert grouping (Phase 10) is the place to address it.
- AI remediation keeps running on a silenced server, and a command waiting for approval is not announced until the silence ends; it may expire unseen, which runs nothing.
- The offline rule, the engine and the triggers are unchanged. A channel added later needs nothing: held rows are about events, not channels.
- A wide silence ending with a chat channel set up releases a few incidents per pass: 100 incidents take about two minutes with Teams. The outbox gains `channel_id` for the copies a limited chat channel is owed.
- `silences` keeps its rows, as the record of what was quiet and why; incident timelines point to them.
