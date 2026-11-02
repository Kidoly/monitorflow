# ADR-0082: Follow-up and acknowledgement

Date: 2026-11-02
Status: accepted

## Context

Channels (ADR-0060, ADR-0077, ADR-0078) and the person on call (ADR-0081) are told of every alert of their place, once. Nobody knows whether anyone reacted: a phone on silent, a person asleep, a chat nobody reads at night. Teams that run services expect two things: someone says "I'm on it", and if nobody does within a set time, the next level is told, and so on. ADR-0081 left this out and kept what it needs: who is on call at any instant, and who covers a gap.

The usual name, escalation, is taken. In MonitorFlow an incident is "escalated" when it gets more severe (`incident.escalated`, ADR-0060), and the AI remediation loop "escalates" to a human when it stops (ADR-0043). A screen saying "escalated to Bob" next to "Escalated to critical" would be read either way.

The promises already made must hold: grouping's at most one message a minute per customer and 65 seconds of delay (ADR-0080), nothing a silence holds back is lost and a silenced place pages nobody (ADR-0079), channels always get their events and a gap goes to the nearest administrators (ADR-0081), and the server can restart at any time.

## Decision

**Words.** "Acknowledge" is saying "I'm on it". "Follow-up" is who is told next when nobody acknowledges, in steps. A follow-up step "tells" people; the screen says "Bob will be told at 03:27 if nobody acknowledges". "Escalate" keeps its two meanings and is never used for this, on screen or in code (`follow_up`, `incident_follow_ups`, `follow_up_policies`). The glossary records it.

**The first message does not change.** What is sent today (the channels of the place and the person on call, grouped) stays the first message. A follow-up only adds steps after it. Routing (ADR-0078), on call (ADR-0081) and their tests are untouched, and an installation that sets nothing up keeps every message it had, plus the default below.

**A follow-up** belongs to a place, like a channel or a schedule: everything, one customer or one site, at most one per place (`follow_up_policies`). The narrowest one covering an incident's site applies; the others do not add to it. It is an ordered list of at most 10 steps. Each step is: after N minutes without acknowledgement (1 to 1440, counted from the step before, or from the first message for the first step), tell any of:

- the person on call of one or more schedules, or whoever covers their gap (ADR-0081),
- named people,
- channels,
- the nearest administrators of the incident's place who can be told: of its site, else its customer, else everything, the same rule as a gap.

An empty list is allowed: "the first message is all", to turn follow-ups off for a place.

**The default needs no setup**: a critical alert nobody acknowledged within 15 minutes goes, in person, to the nearest administrators of its place who can be told, once. Administrators are who already answer for a gap; they need only have set "How you are reached" on the On call page. Nobody configures anything to have it, and a place where no administrator can be told is told nothing more, which the follow-up shows.

**The last step is not repeated: the follow-up stops after it.** Everyone the follow-up names has then been told in person or in their channel; telling them again every few minutes until someone gives in makes acknowledging a way to stop noise rather than a statement, and would page all night for an alert left open on purpose. A team that wants a reminder adds the same step again; the incident stays open and red on screen, and the page says "everyone in the follow-up was told; nobody acknowledged".

**Severity.** Only critical incidents are followed up. A warning is told once and not repeated (ADR-0079); waking an administrator for a warning nobody acted on is the noise that teaches people to ignore alerts. A warning that gets worse starts its follow-up when it is sent as critical. A critical incident that drops back to a warning stops ("no longer critical"), and starts over from the first step if it gets worse again. Approvals and failed updates are not incidents and have no follow-up.

**Acknowledging.** One acknowledgement per incident, the first one, recorded with the person's name and the time (`incident_acknowledgements`), written on the timeline (`acknowledged`) and audited (`incident.acknowledge`). It stops the follow-up at once and for good: getting worse later does not restart it, as the person is on it. It changes nothing else: the incident still resolves on its own, the channels still hear of its changes, AI remediation is not involved. A resolved incident cannot be acknowledged. The screen says "Acknowledged by alice at 03:12".

**Who acknowledges: operators and above of the incident's site**, the people who may silence its server (ADR-0079). Acknowledging stops people from being told, as a silence does; a viewer, like a customer's read-only account at an MSP, should not be able to stop the MSP's follow-up. From the interface, the incident page and the incidents list have the button, and a list of incidents sent together (`?group=`) has "Acknowledge all". A viewer on call is still told, without a link.

**The link in a personal message.** A message sent to a person (on call, named in a step, or an administrator) carries a link that acknowledges what that message tells: one incident, or every open member of a group. Channels do not get one: a link is a credential, and a channel is read by many. The link is:

- **single purpose**: it acknowledges, and shows the incidents it is about; it opens no session and reads nothing else;
- **tied to the person and the incidents**: it acknowledges only the incidents of its message, as its person, and only those they still may acknowledge with their roles at the time of the click; if they may acknowledge none, nothing happens (403). A person who lost their role sees nothing in it; a disabled one finds it dead;
- **expiring**: 12 hours, a night;
- **safe if forwarded**: whoever holds it can acknowledge those incidents in the person's name, nothing more, and the record says "from the notification link" so the person can tell;
- **safe if prefetched**: the secret (256 bits, of which only the SHA-256 is stored, as for password links, ADR-0059) is in the fragment, `/acknowledge/#<secret>`, which browsers, mail scanners and link previewers never send to the server. The page is static; it shows what the link acknowledges and as whom (a `POST`, which changes nothing), and acknowledges only on a click of its button (another `POST`). No `GET` acknowledges anything, and a scanner that runs the page's JavaScript only reads it.

So the link takes one click once open, with no password to type. A link that acknowledged on opening would be acknowledged by the scanners of mail providers, which open links before the person does.

Email bodies carry the link under the incident's; Slack, Teams and Discord cards get an "Acknowledge" button next to "Open in MonitorFlow"; a personal webhook gets `acknowledge_url` in its payload.

**Who manages follow-ups:** the administrators of the place, exactly as for channels and schedules (ADR-0078): global administrators for everything, a customer's for it and its sites, a site's for that site. Others get 403, decided before any lookup; a follow-up they do not manage is 404. A step may name only schedules and channels the editor manages that cover all of the place, and people who could be on call for it (ADR-0081: a role on its site or customer; global roles for global administrators). People already in a step may stay when someone else edits it. Changes are audited as `follow_up.policy_create`, `follow_up.policy_update` and `follow_up.policy_delete`. Running follow-ups use the steps as they are when their next step falls due.

**Channels for follow-ups only.** A channel covering a place hears every event of it, so naming it in a step would only repeat an alert it already had. A channel can be marked "Only for follow-ups" (`notification_channels.follow_up_only`): it then hears only the steps that name it and their resolution, like a managers' chat. Its mail server still sends personal email (ADR-0081).

**In the pipeline.**

- **Start.** When the dispatcher claims a critical `incident.opened` or `incident.escalated`, in the same transaction, it writes the incident's follow-up (`incident_follow_ups`): started now, the next step due after the first step's wait. The clock starts when people were told, not when the incident opened: grouping's wait (ADR-0080) and a silence (below) come first. An incident acknowledged before is not followed up.
- **A step falls due.** Each pass of the dispatcher takes the steps due, in one transaction that locks them, and moves each follow-up on to the next step's time or stops it. A step goes out at most once, like the outbox (ADR-0060). The message is the incident as it is now, with "Not acknowledged after 15 minutes." before its text, and is written to the outbox as sent, so it has an ID for webhooks and counts in the customer's window.
- **Grouping and delay.** A customer is told at most once a minute (ADR-0080): a step due within a minute of the customer's last message waits until the minute is over, or goes with the customer's other messages if some are claimed in this pass. The longest it waits is the same 65 seconds. Steps due together are grouped like fresh events, apart from them, with the same titles after "Not acknowledged after 15 minutes."; follow-ups started by one claim fall due together, so the 40 servers of a site that went dark are one message to the administrator, not forty. A personal webhook gets one request per incident, as always.
- **Silences.** A silence holds the first message back (ADR-0079), so no follow-up starts until it is sent. A step that falls due while a silence covers the incident is not told: it is put off until the silence ends plus the step's wait, so the people told at the end of the silence get their time to react. The page says "Silenced: after it ends, Bob will be told at 05:15 if nobody acknowledges".
- **Hand-offs.** A step that names a schedule tells whoever is on call when it is sent, like the first message (ADR-0081). The page shows who it will be at the step's time.
- **Restarts.** Every time is in the database (`next_at`), so a restart neither loses nor repeats a step; a server down past a step's time tells it at its first pass, and gives the next step its full wait from then.
- **The resolution reaches everyone who was told.** Each person told in person, and each channel a step told, is recorded with the incident (`incident_told`). Its resolution goes to them besides the usual routing, once each, grouped as usual; so the person on call before a hand-off, and an administrator a step told, hear that it is over. An acknowledgement or a resolution also stops the follow-up.
- **Tenancy at delivery.** Whatever a step names is checked again when it is told: a schedule or channel must still cover the incident, a person must still see its site and be reachable (ADR-0081's gap reasons), so a role lost or a channel moved since the follow-up was saved never carries an incident outside its customer.

**API.** `POST /api/incidents/acknowledge` (`{"incident_ids"}`, up to 500), `GET`/`PUT /api/follow-up/policies` (the place in the body), `DELETE /api/follow-up/policies/{id}`, and without a session `POST /api/acknowledge-link` (what a link acknowledges) and `POST /api/acknowledge-link/use`. Incidents gain `acknowledged_by`, `acknowledged_at` and `can_acknowledge`; the detail gains `acknowledged_via` and `follow_up` (`state`, `step`, `steps`, `next_at`, `next`, `stop_reason`, `policy`); the timeline gains `acknowledged` and `followed_up` (who a step told). Channels gain `follow_up_only`. Webhook payloads stay version 1: a step is an `incident.opened` with a `follow_up` object (`step`, `steps`, `minutes`), like a silence's release carries `silence`; a personal one adds `acknowledge_url`.

**Interface.** The incident page says who acknowledged and when, or offers the button, and what happens next in one sentence: "bob and Slack #ops will be told at 03:27 if nobody acknowledges", or why nothing will. The list shows "Acknowledged by alice" or the button. The On call page, for administrators, has "If nobody acknowledges": the default in one sentence, and each place's follow-up as sentences, edited in place. The channel form has "Only for follow-ups".

Rejected:

- **Calling it escalation.** Above.
- **Letting the first step replace the channels.** It would make routing depend on a second setting and break ADR-0081's "channels always get their events".
- **Repeating the last step until acknowledged.** Above; a team adds the step again if it wants a reminder.
- **Following up warnings,** or making it a setting. One good rule, as for grouping's windows.
- **Acknowledging on opening the link** (a `GET`). Mail scanners and chat previews open links first.
- **A link that signs the person in.** A forwarded message would hand over a session; this one can only acknowledge.
- **Links in channel messages.** Anyone in the channel, or any tool behind a webhook, would hold a credential in someone's name; and there is no one person to act as. Channel readers acknowledge from the page.
- **Letting viewers acknowledge.** Above. Anyone told in person who should be able to acknowledge needs the operator role on the place.
- **Unacknowledging.** One more state to explain; the incident resolving is the end of the story.
- **A message to the others when someone acknowledges.** Useful, but every acknowledgement would page everyone told; the page and the timeline say it.
- **Several follow-ups per place, or adding a customer's steps to a site's.** One answer per incident, the narrowest, like a mail server for a schedule.

## Consequences

- An alert nobody reacts to reaches the next person within minutes of its wait, in person, without anyone setting anything up beyond "How you are reached".
- Existing installations get the default at upgrade: administrators who set a contact for on call are told of critical alerts nobody acknowledged after 15 minutes. That is a new kind of message for them, the one this task asked for.
- ADR-0081's "until escalation lands, nobody is told when they do not react" no longer holds.
- Each personal message about an open incident writes a link row, kept until it expires, and each person or follow-up channel told writes one `incident_told` row per incident, kept with the incident.
- A step adds an outbox row like an event, so a customer's window counts it: a fresh alert of the same customer may wait up to a minute behind a step, as behind any message (ADR-0080).
- Someone told in person who is only a viewer gets no link and cannot acknowledge; the administrator gives them the operator role on the place.
