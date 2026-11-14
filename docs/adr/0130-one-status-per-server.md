# ADR-0130: One status per server, a status strip and a site tree

Date: 2026-11-14
Status: accepted; amends the fleet screen of the design handoff (`docs/design/`)

## Context

The fleet showed online or offline, then a "Silenced" label, then the worst incident, and three counters that lived only on its own page. Elsewhere the Incidents count was the only sign of trouble. At a few thousand servers over many sites, a flat list is too long to scan, and a server someone silenced or is already working on looked like any other problem.

## Decision

- **One status per server**, computed by the central server and added to `GET /api/servers` as `state`, worst first:
  1. `silenced`: a silence covers it now;
  2. `offline`: it is offline, and either has no open incident or one nobody acknowledged;
  3. `critical`, then `warning`: the worst open incident nobody acknowledged;
  4. `acknowledged`: it has open incidents and someone acknowledged each one;
  5. `ok`.
  A silence wins because it is a choice someone made for a while. An offline server's incidents are stale, so offline comes before them.
- **`GET /api/servers/summary`** counts the servers the user reads in each state, narrowed by `site_id` or `customer_id`. The live status comes from the registry, so the counts are made in the server's code, not in SQL.
- **A status strip** at the top of every signed-in page shows the six counts in the scope. Each opens the fleet filtered with `?state=`. A count that needs a look is tinted; zero and OK stay plain, so a large healthy fleet does not shout. It replaces the three counters of the fleet.
- **Silenced and acknowledged say who, until when and why.** A silence already has an author, an end and a required reason (ADR-0079). An acknowledgement now takes a `note` of up to 500 characters: `POST /api/incidents/acknowledge` requires it from someone signed in. It accepts a missing note from API keys, so existing scripts keep working, and the one-click link of a personal message (ADR-0082) stays one click, without a note. Each server row carries `silence` (`by`, `until`, `reason`) and `acknowledgement` (`by`, `at`, `note`), and the fleet shows them under the problem: "Silenced by alice until 18:00 today: migration".
- **The fleet groups servers by site**, under their customer once the user sees a second one (ADR-0104). A site where every server is OK folds to one line. A folded group shows a count per state. When a group has exactly one problem, that count names it instead ("web-1 · Disk used 83%"). The handoff had removed group headers; at this scale they come back for the fleet only.

## Consequences

- `state`, `silence`, `acknowledgement`, `acknowledged_note` and the summary are additive. A request without a note is refused only for sessions, and only after the permission checks.
- The web interface asks for the note in place of the Acknowledge button, never in a popover. Escape closes it. `open_incidents` and `silenced_until` stay as they were.
- The web interface guesses a state from the other fields for a central server older than this ADR. That guess cannot tell a warning from a critical one, nor see acknowledgements.
- The strip polls one small request every 15 seconds on every page, beside the Incidents count.
