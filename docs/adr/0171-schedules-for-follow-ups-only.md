# ADR-0171: Schedules for follow-ups only

Date: 2026-11-22
Status: accepted

## Context

"If the person on call does not answer, a second person behind": follow-up steps (ADR-0082) already tell the person on call of a named schedule after a wait without acknowledgement. But every schedule also hears of every alert of its place in the first message (ADR-0081), so the second person was woken at once with the first, which is exactly what a second level should not do. Channels had the same problem and got "Only for follow-ups" (`follow_up_only`); schedules did not.

## Decision

**A schedule can be "Only for follow-ups"** (`on_call_schedules.follow_up_only`, false by default, migration `20261210100000_on_call_follow_up_only`). Its people are never told of an alert's first message; they are told only by a follow-up step that names the schedule, with the same rules as any step: whoever is on call in it at the step's time (all of them with several at once, ADR-0170), a gap to the nearest administrators, the resolution reaching everyone told. Everything else about a schedule is unchanged: rotation, covers, gaps shown ahead.

This is the same word and the same meaning as for channels, so the glossary keeps one term. On screen the form asks "When they are told": "With every alert" or "Only for follow-ups: when nobody acknowledges", and explains it as a second level.

**Visible and simple.** A schedule for follow-ups only says on its card which steps tell it ("Told after 15 minutes without acknowledgement: step 1 of the follow-up of acme"). When no step names it, its people would never be told: the card says so and offers "Tell them after 15 minutes", which adds that step to the follow-up of the schedule's own place through the existing `PUT /api/follow-up/policies`. A place that had only the default keeps it after the new step, so the administrators are still told if the second level does not answer either.

**API.** `follow_up_only` on schedule requests and views, documented in OpenAPI. Adding the step is the follow-up API as it is; nothing new is needed for scripts.

## Consequences

- A first and a second level are two schedules and one step: "alice this week, and bob after 15 minutes if alice does not acknowledge".
- A schedule for follow-ups only that no step names tells nobody; the card says it plainly rather than refusing to save, since the step may be added just after.
- Removing the step or the follow-up leaves the schedule silent, which the card shows again.
