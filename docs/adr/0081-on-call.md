# ADR-0081: On call

Date: 2026-11-02
Status: accepted

## Context

Channels (ADR-0060, ADR-0077, ADR-0078) send every alert of a place to a fixed address: a team mailbox, a Slack channel. Somebody has to watch it, and at night nobody does. Teams that run services take turns instead: one person is on call for a week, gets the alerts in person, and hands over. Holidays and swaps move a turn without rewriting the rotation. Escalation (acknowledge, or the next person is told) comes next and builds on who is on call; it is not part of this decision.

People are global accounts with roles on everything, a customer or a site (ADR-0026, ADR-0059). They have a username and nothing to reach them by.

Time is where this goes wrong: a "Monday 09:00" hand-off in Paris is 08:00 UTC in winter and 07:00 in summer, some local times happen twice or not at all, overrides overlap, the hand-off can fall while a message is going out, and a person can lose their role in the middle of their week.

## Decision

**A schedule** covers everything, one customer or one site: `notify::Coverage`, the same columns and the same rule as a channel (ADR-0078). It has a name, people in order, and a rotation: the turn hands off every day or every week, at a local time, in an IANA time zone the administrator chooses (the browser's by default). It lives in `on_call_schedules`, `on_call_members` (position, user) and `on_call_overrides`.

**Turns.** A schedule stores an anchor: a local date on which a hand-off happened and the member who took over then (`anchor_date`, `anchor_position`). A weekly rotation hands off on the anchor's weekday. Turn *k* starts at the hand-off time on the anchor date plus *k* days or weeks, and is the member at `(anchor_position + k) mod n`. Who is on call at an instant is the turn whose start is at or before it and whose successor starts after it; the hand-off instant belongs to the person taking over.

**Time zones and daylight saving.** Hand-offs keep their local time: computed per date in the schedule's zone with the IANA database compiled into the server (`chrono-tz`), so the week across a change lasts 167 or 169 hours and nobody's turn moves. For a local time that does not exist on one night (02:30 when clocks jump from 02:00 to 03:00) and one that happens twice, the rule of RFC 5545 (3.3.5) applies: a skipped time is read with the offset before the change (03:30), a repeated one is the first. The server's own zone plays no part. PostgreSQL stores the date, the local time and the zone's name, not instants, so a change to a zone's rules in a later release moves future hand-offs with it. The screen shows turns in the viewer's zone and states the rule in the schedule's ("Hands off every Monday at 09:00, Europe/Paris time").

**Overrides** put one person on call from one instant to another, at most 90 days, instead of whoever the rotation names: "Alice covers Bob from Friday to Monday", a holiday, a swap (two overrides). **Where overrides overlap, the newest wins** (latest `created_at`, then ID), so the last decision taken is the one in force and adding a cover never needs another one changed. Overrides end on their own; removing one restores the rotation.

**Editing keeps who is on call now.** Saving a schedule moves its anchor to the last hand-off under the new settings, with the person on call by the rotation now at that position. Reordering, adding someone, or moving the hand-off day or time therefore does not change who is on call at that moment, only the turns that follow. If the person on call is taken off, the one after them in the list takes over at once. A new schedule starts at the last hand-off with its first person.

**Who can be told.** The person on call is told when they have a contact (below), their account is enabled, and they still see the whole place through a role: a global one, one on the customer, or one on the site for a site's schedule. Their turn is otherwise a **gap**, and so is a removed user's turn and a schedule without members. The reason is named: removed, disabled, can no longer see the place, no contact, or only an email address and no email channel to send it.

- A removed user's place in the list stays, empty (`ON DELETE SET NULL`), so the others' turns do not shift; their overrides go (`ON DELETE CASCADE`), which gives the turn back to the rotation.
- Losing a role, being disabled or dropping one's contact takes effect at the next message, without touching the schedule, and undoing it restores the turn.

**A gap does not lose the alert.** Channels always get their events, on call or not; a schedule adds a person, it never takes the channel's place. During a gap, the person's message goes to **the nearest administrators of the place who can be told**: those with an administrator role on the site itself for a site's schedule, else on its customer, else global administrators. Only one level is told, the narrowest that has someone. If nobody at any level can be told, only the channels are; the screen says so in advance. Administrators are chosen rather than "the channels only" because a schedule exists for when nobody watches the channels, and rather than the next person in the rotation because that person is off duty and did not agree to be.

**Gaps are shown before they happen.** The schedule's view lists the coming four weeks with every gap, its dates, its reason in plain words and who is told instead, and each member who cannot be told is flagged in the list and in the form.

**Contacts.** Each user keeps one contact in `user_contacts`: an email address, and optionally one personal destination of the channels' kinds: Slack, Teams, Discord or a webhook. Messages go to both. Only the user changes their own contact (`/api/on-call/contact`): it decides where alerts about places they can read are sent, and an administrator changing it could redirect someone else's alerts; administrators see who has none. The chat or webhook follows the channels' rules: the URL of a chat service is its secret and never returned, the audit keeps its host, a saved secret stays only with its target, and a destination saved by anyone other than a global administrator reaches only public addresses (`public_only`, ADR-0078).

**Email goes through an email channel's mail server.** There is no second mail server to set up. A personal email about a schedule is sent with the mail server, port, username, password and sender of **the narrowest email channel that covers the schedule's whole place** (a site's, else its customer's, else one for everything; the oldest of equals), to the person's address only. A channel narrower than the schedule is never used, so a customer's mail server carries nothing about another customer, and its `public_only` limit comes along. With no such channel, people who gave only an email address cannot be told and their turns are gaps, which the screen explains. A customer administrator's users may therefore be emailed through the MSP's server; the recipient is their own address and the text is about a place they can read.

**Delivery** is the existing pipeline. The dispatcher claims events as before, a customer's together after grouping's wait or window (ADR-0080); a silence holds them as before (ADR-0079); then, with one `now` for the whole pass, it computes for each schedule who is on call (or the fallback) and adds one delivery per person to the channels' deliveries:

- **The person on call is whoever is on call when the pass sends**, not when the event happened. Every message of one pass goes to the same people, retries included. A hand-off during a pass applies from the next pass, at most 5 seconds later. A group waiting for its window (ADR-0080) is not tied to anyone: it goes to whoever is on call when it is sent, so events from before a hand-off that are sent after it reach the incoming person.
- A held event released after a silence goes to whoever is on call at the release.
- Routing: a schedule hears exactly the events a channel of its coverage would (ADR-0078's table). A person on call in several schedules that cover one event is told once, through the mail server of the narrowest of them.
- **Grouped like a channel.** A person gets, for the events of their schedules, exactly what a channel of the same coverage would: email and chat destinations get ADR-0080's groups (the same check on several machines, the rest of a site, what a silence held back) with the same titles, lists and links, and a personal webhook gets one request per event, since a tool behind it may follow each incident by its ID. Grouping happens after routing, per destination, so a person whose schedule covers one site never sees another site's members, and a group never mixes customers. Personal delivery adds no wait of its own: events are claimed once for channels and people together, so the longest delay is ADR-0080's 65 seconds, a lone alert 10 to 15. Copies owed to one person are due at once, as copies for one channel are, and stay out of the customer's window. A group sent to people and channels with the same members has one ID, so its link is the same list.
- Personal chat and webhook destinations are paced, gathered and retried like channels of their kind (ADR-0077), and an owed message they could not take while rate limited, a group's every member included, is copied for that person (`notification_outbox.user_id`) as for a channel; the copy goes to that person's chat alone. Personal chat services count in the release budget of a silence's end, like channels'. So ADR-0079's and ADR-0080's promises hold in person: nothing a silence held back is lost, every member of an owed group is named, and a resolution reaches a personal webhook. Rendering and size limits are the same. The outcome is the contact's status, shown to its user.

**Who manages schedules:** the administrators of what they cover, exactly as for channels (ADR-0078): global administrators for everything, administrators of a customer for it and its sites, of a site for that site. Others get 404 for a schedule they do not manage, and 403 for creating one for a place they do not manage, decided before any lookup. **A member** sees the schedules they are on, read-only, without who is told during gaps or the mail server. **Who can be put on a schedule:** people with a role on the schedule's site or its customer; global administrators may also put people with a global role, an MSP's staff. A customer administrator therefore picks only from their own customer's people, and the list they are shown has no one else. People already on a schedule may stay when someone else edits it, though they might not be addable now, so editing the name never fails because of a role change; their turns are gaps until it is fixed. Overrides follow the same rule.

**API**, all under `/api/on-call/`: `GET` and `POST schedules`, `PUT` and `DELETE schedules/{id}`, `POST schedules/{id}/overrides`, `DELETE overrides/{id}`, `GET people?customer_id=&site_id=`, `GET` and `PUT contact`, `POST contact/test`. A schedule's view carries `now`, `next`, `turns` for four weeks (each with `problem` when it is a gap and `override_id` when someone covers), `fallback`, `mail_host` and `can_manage`. Audited: `on_call.schedule_create`, `on_call.schedule_update`, `on_call.schedule_delete`, `on_call.override_create`, `on_call.override_delete`, `user.contact_update`.

**Interface:** a page "On call" under Monitoring, for everyone. At the top, "How you are reached": an email address and an optional chat, with "Send test". Then each schedule: who is on call now and until when, who is next and from when, a banner for each gap ahead, and the coming four weeks; "Cover…" on a turn opens a form already set to that turn's dates, where one picks who covers. Administrators create and edit schedules in place: what it is for (only when there is a choice), people in turn with up and down, every day or every week, the day, the time and the time zone.

**For escalation.** `notify::on_call` answers who is on call at any instant and who comes next (`Schedule::on_call`, `Schedule::spans`) and who is told in a gap. Escalation can keep its own record of who was told about which event and when, and tell the next person when nobody acknowledges; nothing here needs to change for it.

Rejected:

- **A server-wide mail server for people.** One more form to fill, while every installation with email already has one in its channels; and a customer administrator could not set it.
- **Contacts changed by administrators.** Faster onboarding, but redirecting someone's personal alerts is a power nobody needs; the screen shows who has none.
- **Gaps going to the next person in the rotation**, or to everyone on the schedule. People off duty are not on call.
- **Gaps going to the channels only.** They already get every event, so this would mean "nobody", the case a schedule exists for.
- **Hand-offs computed in UTC or the server's zone.** A Monday 09:00 hand-off would move by an hour twice a year.
- **Several layers, restricted hours (business hours only), and rotations of other lengths.** More to explain for what most teams do; overrides cover the rest. Restricted hours would also create gaps by design.
- **Overrides by the people on call themselves.** Swaps are agreed between people, but someone must answer for the schedule; administrators do it in a few clicks.
- **Making the schedule a kind of channel.** A channel has one fixed destination and one status; a schedule has many people, contacts and a fallback.

## Consequences

- A team takes turns and each alert reaches one person, in person, while the channels stay as they are.
- An installation without an email channel can still use on call through personal chats; people with only an email address then show as gaps.
- Every pass that sends something reads the users, their roles and contacts; that is small next to sending, and nothing is read when there is nothing to send.
- A wide outage reaches the person on call as a few grouped messages, at most one a minute per customer, like the channels; a personal webhook still gets one request per event. Until escalation lands, nobody is told when they do not react.
- A schedule's people are global accounts with roles; a customer that wants its own people on call needs them invited and granted a role on it first (ADR-0059).
- A time zone's rules are those of the server's release; a country changing them needs an update for future hand-offs to follow.
