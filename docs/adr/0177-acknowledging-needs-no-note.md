# ADR-0177: Acknowledging needs no note

Date: 2026-11-22
Status: accepted; amends ADR-0130

Numbered ADR-0166 when merged, renumbered because three other ADRs had the same number.

## Context

ADR-0130 made a note required when a person signed in acknowledges an incident ("What are you doing about it?"), so the fleet and the follow-up could say why. Acknowledging is the action people take most, and now often for many incidents at once from the incidents list's checkboxes. The owner asked for the note to be optional: acknowledging should take one click, and a note is added when there is something to say.

## Decision

- `POST /api/incidents/acknowledge` takes `note` as optional for everyone, people and API keys alike. A blank note is the same as none. A note stays limited to 500 characters, trimmed.
- The web interface asks "What are you doing about it?" in the same place, with the field optional: Acknowledge is enabled at once.
- Everything else of ADR-0130 stands: the note, when given, is shown with the acknowledgement in the fleet, on the incident and on its timeline, and audited.
- The incidents list no longer has an Acknowledge button per row: a row's checkbox and the bulk bar do it, for one incident or many.

## Consequences

- Additive for the API: requests refused before (signed in, no note) are now accepted. Nothing that worked stops working.
- The fleet says "Acknowledged by alice 14:02" without a reason when there is no note, as it already did for API keys and acknowledge links.
