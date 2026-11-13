# ADR-0119: A full relay buffer makes room from strangers and the largest agent first

Date: 2026-11-12
Status: accepted; amends ADR-0027

## Context

While its upstream is down, a relay accepts any agent certificate from the CA without asking the server, of any customer and any site, and buffers its batches (ADR-0027). The server's relay policy only runs when the relay replays them. ADR-0027 accepted that an agent of another site can fill the buffer until the link returns. It did not consider what this costs the relay's own agents: a full buffer dropped the oldest segment of the whole relay, so a foreign agent with network access to the relay could flood it during an outage and push out the backlog of every agent the relay really serves. A security audit (2026-11-11) rated this Low.

## Decision

**The relay remembers which agents the upstream accepted through it.** When the upstream accepts a stream the relay opened for an agent, live or for a replay, the relay writes an empty `confirmed` file in the lane's buffer directory, next to `hello.pb`. It is read when the buffer opens, so it survives restarts. A permanent refusal from the upstream (the statuses ADR-0027 lists), which deletes the agent's records, also deletes the file: the agent starts over as unknown.

**A full buffer makes room in this order.** Until the new record fits, the relay drops one segment at a time, always the oldest segment of:

1. the agent never confirmed that uses the most space, while there is one with records;
2. otherwise, the confirmed agent that uses the most space.

Ties go to the agent with the oldest segment. A record of an unconfirmed agent never drops a confirmed agent's records: when only confirmed agents have records left, the new record is dropped and counted in `dropped_full`.

So a stranger can only use the room the relay's known agents leave free, and gives it back as soon as they need it. Among known agents, the one with the largest backlog pays first, so one noisy agent cannot push out the rest of the site.

Agents enrolled while the link is down, or never seen through this relay before, are still buffered; they lose out only when the buffer is full and a known agent needs the room. They are confirmed on their first accepted stream.

## Consequences

- A flood during an outage costs a site's known agents nothing while the buffer has room for them.
- Data is no longer dropped strictly oldest first across the whole relay. Within each agent it still is.
- The `confirmed` files are not secrets and carry no data. Like everything else in the buffer they belong to a lane (ADR-0027): the same agent reached by another path is confirmed separately.
- Paths through a downstream relay are already refused while the upstream is down unless it accepted them before (ADR-0027); this decision matters most for agents connected directly, which are always buffered.
