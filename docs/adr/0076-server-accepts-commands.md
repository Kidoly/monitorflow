# ADR-0076: Each server says whether it accepts commands

Date: 2026-11-01
Status: accepted

## Context

An agent decides by itself what it runs (architecture §8), and until now nothing outside it knew the answer. ADR-0064 left this open: an administrator could set up AI, approve a command, and only learn from the refusal that the server had never agreed. The server's own guess, `package_updates` (ADR-0063), covers two kinds at most.

What an agent does with a command depends on the kind:

| Kind | Linux | Windows | Decided by |
| --- | --- | --- | --- |
| `shell` | refused unless allowed | refused | `commands.shell`, usually through `MF_ALLOW_COMMANDS=1`; the platform |
| `apply_updates` | runs, or refused without a package manager | runs | the host (ADR-0063, ADR-0049) |
| `configure_sources`, `revert_sources` | the same | refused | the host (ADR-0047) |
| `self_update` | runs | runs | nothing: always accepted |

So one "accepts commands" flag would be true on every server for `self_update` and say nothing useful, or false on a server that takes updates.

## Decision

**The report is per kind, and comes from the handlers.** `Hello.command_kinds` (8) lists `CommandKindState { kind, acceptance }` with `ACCEPTED`, `DISABLED` or `UNSUPPORTED`. `Handler::acceptance` sits next to `Handler::run` and reads the same state, and `Handlers::report` asks every registered handler. The dispatcher goes through those same handlers, so the report cannot say one thing while the agent does another: a key in `agent.toml`, the `MONITORFLOW_COMMANDS_SHELL` variable, the Windows build and a missing package manager all end up as the handler that is registered. Nothing else on the agent interprets them.

**It reports refusals only, not outcomes.** Accepted means the handler will start. A sudo rule that is missing still fails the command when it runs, as ADR-0031 says; finding that out in advance would mean trying privileged commands at every start.

**Why `Hello`.** The agent reads this configuration once, at start, so it can only change with a restart, and a restart opens a new stream. Every stream starts with a `Hello`, so a change reaches the server as soon as the agent is back, with no timer and no new message to ack. `MF_ALLOW_COMMANDS=1` restarts the agent, and the page changes by itself. The relay already keeps the last `Hello` of every agent, in memory and in `hello.pb`, and sends it at the start of every upstream stream, live or replayed (ADR-0027), through chains too (ADR-0028), so relays need no change.

A new `Envelope` variant was rejected: a 0.1.0 relay or server decodes it as an envelope with no payload, which is at best dropped, and it would need the store-and-forward decisions made again.

**Compatibility.** The protocol stays 1.1: the field is additive and only the major version is checked.

| Agent | Relay | Server | Result |
| --- | --- | --- | --- |
| 0.1.0 | any | new | Unknown |
| new | 0.1.0 | new | Connects; unknown until the relay updates. Relays decode and encode every envelope, and prost keeps no unknown fields, so the report is lost there and everything else arrives |
| new | none | 0.1.0 | Connects; the server ignores the field |
| new | new | new | Reported |

All four rows were run with real 0.1.0 binaries against this version; the tests in `crates/common/tests/hello_compat.rs` keep the wire part from regressing.

**Empty is unknown, and unknown is never a refusal.** A repeated field cannot tell "not sent" from "empty", and needs not: an agent that reports always has handlers, so an empty list means it did not report, or a relay lost it. The server stores `NULL`, and a kind missing from a report, or a value it does not know, stays unknown too. Each `Hello` replaces the report, also with nothing, so an agent rolled back after a failed update, or moved behind an old relay, goes back to unknown rather than keep an answer it no longer gives. A deferred replay sends the last `Hello` the relay stored, which is what that agent last said.

**Storage.** `servers.command_kinds`, JSONB, `{"shell": "off", "apply_updates": "accepted", ...}`, with `NULL` for not reported. The server keeps only the kinds and states it knows, since a report crosses relays: this bounds what reaches the database, and refusing a stream for it could lock out a newer agent. `GET /api/servers` and `GET /api/servers/{id}` show it as `command_kinds`, under the access rules they already have. The API's word for `DISABLED` is `off`.

**A reported refusal stops what would be refused; unknown stops nothing.**

- `POST .../approve` answers 409 with the reason, and `GET /api/remediation/approvals` gives `shell_acceptance`, so the page offers no Approve or Edit and says how to turn commands on. Rejecting stays possible. The incident waits as it would for any decision, so turning commands on in time lets the approval through.
- The incident loop escalates with the same reason before a `shell` command is queued, and before any snapshot is taken for it.
- `apply_updates` and the source commands keep their `package_updates` check, which rests on the same fact on the host, and which already survives an old relay through the OS family. `self_update` is never refused.

The agent still decides. These checks spare people a refusal; they do not protect anything, and a stale or forged report can only change what the interface offers.

**On screen.** The server page shows **AI commands**: On, Off with how to turn them on, Not available with why, or Not known yet, which says it will fill in by itself. The state of an offline server is marked as of its last contact. The Commands tab of a server that is off gives administrators the line to run; others are told an administrator can do it. Administration › AI counts the servers that accept AI commands and names the others by reason. Details on the server page list every kind. The other kinds stay under Details because the page's "Package updates" fact already covers the case where they matter.

Rejected:

- **A single `accepts_commands` boolean.** See the table above.
- **Asking the agent to confirm with a heartbeat field.** Sent thousands of times to say what cannot change without a restart.
- **Inferring from past refusals.** It needs a refused command first, which is what this ADR is meant to avoid, and old results say nothing about the configuration now.
- **Treating unknown as off in the interface.** A 0.1.0 agent with the opt-in would look refusing, and its commands would be blocked until it updates.

## Consequences

- Servers show Not known yet until both their agent and every relay on the way have updated, which automatic updates do by themselves.
- The AI page's "accept" count can lag behind by the time an agent takes to reconnect, or stay at the last answer while a server is offline. Nothing can reach an offline server anyway, and its next `Hello` comes before any command does.
- Reports do not say whether sudo rules are installed: `apply_updates` can show as accepted and then fail.
- A future kind needs adding to the server's list to be stored, and a future refusal needs only `Handler::acceptance`.
