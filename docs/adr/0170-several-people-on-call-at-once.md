# ADR-0170: Several people on call at once

Date: 2026-11-22
Status: accepted

## Context

A schedule (ADR-0081) puts one person on call at a time. Teams ask for two: a pair that shares the night, or a Linux person and a network person who both hear every alert of a place. Two schedules on the same place already do it, but nothing on screen says so, the two rotations drift apart when one is edited, and covering a holiday means finding the right schedule first. ADR-0081 rejected layers to keep schedules simple; this keeps that, with one number.

## Decision

**People per turn.** A schedule has `people_per_turn`, 1 (the default, today's behavior) to 5, and never more than its members. Turn *k* takes that many members in the list's order, from `anchor_position + k × people_per_turn`, wrapping around: with alice, bob, carol and dan two at a time, alice and bob, then carol and dan, then alice and bob. With an odd number the pairs shift (alice and bob, carol and alice, bob and carol), and nobody ever holds two places of one turn. Hand-offs, time zones and the anchor stay those of ADR-0081: everyone in a turn hands off together.

**Places.** Each person of a turn holds a numbered place (`seat`, 0 for the first). A cover replaces one place: "dan covers bob" takes bob's place only, and alice stays on. Covers of the same place follow ADR-0081's "newest wins". A cover of a place the schedule no longer has, after the number went down, does nothing until the number goes back up. Editing keeps the first place's person on call, as ADR-0081 keeps the one person today.

**Who is told.** Everyone on call is told, each through their own contact. A place in a gap goes to the nearest administrators as before, and someone told twice (an administrator covering two gaps) gets one message. A follow-up step that names the schedule (ADR-0082) tells everyone on call in it at the step's time.

**API.** Additive: schedules take and return `people_per_turn`; a turn has `seat`; a schedule's view adds `now_all` and `next_all`, one entry per place, while `now` and `next` keep meaning the first place, so existing scripts read what they read; covers take and return `seat`. Migration `20261210090000_on_call_together` adds both columns with defaults that keep every schedule as it was.

**Interface.** The schedule form has "On call at once" (1 to 5, shown with two people or more) and says in a sentence who goes together. The card says "2 people at once", "On call now: alice and bob", and the coming weeks list each place's turns. "Someone else covers…" asks whose place.

## Consequences

- A pair on call is one schedule, one list, one hand-off; two schedules on one place still work for two independent rotations.
- Turns of a pair cannot be ordered freely (alice with carol one week, with bob the next): the list order decides, and covers handle the exceptions.
- Each person on call adds personal messages, so a pair doubles them; the channels are unchanged.
