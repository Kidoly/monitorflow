# ADR-0105: One peer ID, one role, one site

Date: 2026-11-09
Status: accepted

## Context

ADR-0004 put the role in the certificate, so `monitorflow://agent/web-01` and `monitorflow://relay/web-01` are two identities. The database followed: `servers.peer_id` and `relays.peer_id` are each unique, and nothing relates them. Several places then treated the bare ID as if it named one peer (security audit of 2026-11-09):

- The registry of live streams was keyed by ID. A relay named like an agent replaced the agent's stream, again at every reconnection, so the agent's commands stopped; and the reverse closed a relay's own stream. Revoking one closed the other.
- Samples of a relay's own stream reached the threshold engine, which loads rules by `servers.peer_id`: the relay opened false incidents on the agent of its name, or hid real ones behind points dated in the future.
- A command's signature covers `agent_id` as a bare string (`common::command::signed_bytes`), so a relay's `self_update` could be delivered to the agent of its name.
- Enrollment checked a relay's ID against relays and an agent's against agents only. Customer administrators create tokens (ADR-0029), so one customer could do all of the above to another by choosing a name.
- An agent has no `servers` row between its enrollment and its first Hello, which lasts as long as an administrator takes with a key that asks for approval (ADR-0073). Its site is then the one of the latest consumed token for its ID (ADR-0008). An administrator of another customer who guessed the ID could consume a token for it in their own site, and the agent's first Hello created its row there, for good.
- Revoking a relay closed its stream and those of the peers attached to it, but not those further down a chain (ADR-0028), which run on the revoked relay's connection too.

Some fleets have an agent and a relay of one ID today, and not by mistake: an install key names both after the host (ADR-0066, ADR-0073), so a relay's host that also runs an agent gets the same ID twice.

## Decision

**Live streams are keyed by role and ID.** The registry, its view of other instances (ADR-0089), sending, closing and the dispatchers name a role with every ID. An agent and a relay of one ID are two entries that never replace, receive for or close each other. `peer_streams` had the role in its key already.

**Only agents feed thresholds.** Samples reach the engine from streams whose peer is an agent, as log lines and package reports already did.

**Closing a relay closes what passes through it.** A stream records every relay of its path, the connecting one first. Revoking a relay closes its own stream and every stream with that relay anywhere on its path.

**A new enrollment does not take an ID the other role holds.** The other role holds an ID while it has an unrevoked `servers` or `relays` row, an unrevoked and unexpired certificate, or a pending single-use token, compared exactly, as certificates are. Then:

- `POST /api/tokens` and `token create` answer 409 without saying whose it is;
- consuming a token is refused like any other refused enrollment, which covers two tokens created at the same moment; enrollments take the lock install keys already took, so two of them cannot both pass;
- an install key, which chooses the ID, counts both roles when it looks for a free one, without case as before: the second of `gw` becomes `gw-2`, whichever role came first. It does not bring a removed peer back (ADR-0062) under an ID the other role holds, and gives it a new one.

**Existing duplicates keep working.** The rule is about new identities. A peer that already holds its ID (an unrevoked row or a valid certificate of its own role) renews, reconnects and enrolls again with a token as before, whatever the other role has. Nothing is renamed and no migration runs. The duplicate ends when one of the two is removed: it cannot come back under that ID while the other holds it.

**An agent's ID stays in the site of its certificate.** For an agent without a `servers` row, an enrollment is refused when a host of that ID waits for approval in another site, or when an unrevoked and unexpired certificate exists and the token consumed before this one was of another site. `POST /api/tokens` answers 409 for the same cases, and for a row in another site, with one message that names no site.

**Names in a relay token are checked like confirmed names.** ADR-0066 left this as a follow-up. `POST /api/tokens` with `role=relay` now applies the refusals of confirmation to `server_names`: unusable names, the server's own names, and the names of relays of other sites, with the same exception for private addresses. `token create` on the server's host is not checked: whoever runs it holds the CA key, and it is how a relay on the server's own host gets the server's name.

## Alternatives rejected

- **One table of IDs, or a unique constraint across both.** It would need a migration that fails, or renames a peer, on the fleets that have duplicates, and a renamed peer has a certificate under its old name.
- **Refusing existing duplicates at their next Hello.** It would take a relay and its host's agent offline on upgrade, for a risk that needs two customers.
- **Adding the role to what a command's signature covers.** It changes the signed bytes, so every agent and relay would have to be updated before the server, or accept both forms for a release. New enrollments no longer share IDs, which closes the same gap without a protocol change; existing duplicates are in one site, where a relay may already forge its agents' data.
- **Refusing an ID the other role ever held.** A removed relay's name could then never go to a server, and install keys would number hosts for no reason.
- **Checking only when the token is created.** Two requests at once would both pass; the check at consumption is the one that holds.

## Consequences

- A customer administrator can no longer disturb another customer's agent or relay by choosing its ID, at enrollment or on a live stream.
- With install keys, a relay's host that also runs an agent now shows as `gw` and `gw-2`. Hosts enrolled before keep their shared ID.
- An existing agent and relay of one ID still share the string a command's signature covers. Both are in one site in practice, and removing and adding one of them again gives it its own ID.
- IDs are global: whoever takes one first has it, in either role, and a 409 tells an administrator that an ID is in use somewhere, as it already did within a role.
- An agent enrolled in the wrong site that never connected keeps its ID there until its certificate expires or is revoked; a host waiting for approval is rejected to free it.
- A relay token whose names are `localhost` or a loopback address is now refused by the API. Development setups on one host use `token create`.
