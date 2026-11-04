# ADR-0098: AI commands turned on and off from the web interface

Date: 2026-11-16
Status: accepted

## Context

AI commands, the `shell` kind of AI remediation (ADR-0043, ADR-0084), run only where the host's administrator turned them on: `MF_ALLOW_COMMANDS=1` with the Linux installer (ADR-0064), `monitorflow-agent.exe ai-commands on` on Windows (ADR-0084). Each server reports the answer and the interface shows it (ADR-0076). The agent decides by itself what it runs (architecture §8), so a central server that is taken over cannot run a shell anywhere the host did not agree.

That boundary costs a login on every server to change one setting, and turning commands off in a hurry, during an incident with a misbehaving AI, means the same round of logins. The owner wants the switch on the server's page. The boundary must hold for every server whose administrator did not agree to give it up.

## Decision

### Consent stays on the host

**`commands.from_web`** in `agent.toml`, off by default, lets the central server turn `commands.shell` on and off. Only the host's administrator sets it:

- Linux: `MF_AI_COMMANDS_FROM_WEB=1` with the installer, as a systemd drop-in (`ai-commands-from-web.conf`, `MONITORFLOW_COMMANDS_FROM_WEB=true`), like `MF_ALLOW_COMMANDS`; `0` takes it back. It works on an installed agent too.
- Windows: `monitorflow-agent.exe ai-commands from-web on|off`, which edits `agent.toml` like `ai-commands on|off`, or the MSI property `AI_COMMANDS_FROM_WEB=1`, which runs that command at install and upgrade.

The agent's user cannot write either place: `agent.toml` is `root:monitorflow 0640` on Linux and read-only for `NT SERVICE\MonitorFlowAgent` on Windows (ADR-0050), and drop-ins belong to root. No command, signed or not, can grant the consent.

**Off always works.** Turning AI commands off needs no consent: it only takes power away, and it is what one wants to do fast. Without consent, the agent refuses "on" with the line that allows it, and `Hello` reports the new kind `ai_commands` as `off` (ADR-0076), so the server knows before trying.

**Existing installs keep today's behaviour**: no consent, so their page offers "Turn off" when commands are on, and otherwise the one line to run, from the installer or the Windows command.

### The switch is a signed command

A new command kind, **`ai_commands`**, payload `AiCommandsSwitch { on }`. It travels and is checked like every command (ADR-0031): over the agent's mTLS stream, through relays unchanged, signed by the certificate authority's key and checked against it, bound to one agent, journaled so a redelivery is not applied twice, and refused after it expires. A separate signing key was not added: the CA key already signs every command and certificate, and a server that has it could sign with any other key it holds. The consent above is what holds against a server that is taken over.

**Where the choice lives.** The agent keeps the server's last choice in `<data_dir>/ai-commands`, `on` or `off`, written through a temporary file and a rename before it takes effect. The data directory is the agent's own, kept by upgrades and self-updates on both systems. What applies:

| Stored | `from_web` | AI commands |
| --- | --- | --- |
| nothing | any | `commands.shell` |
| `off` | any | off |
| `on` | yes | on |
| `on` | no | `commands.shell`: taking the consent back undoes the server's "on" |

A file that cannot be read, or holds anything else, counts as `off`. A choice made on the host wins over the last one from the server: the installer with `MF_ALLOW_COMMANDS=0|1` and `ai-commands on|off` remove the file.

**`shell` and `ai_commands` share one state**, read by `ShellHandler` at each command and by both handlers' `acceptance`, so the report cannot differ from what the agent does (ADR-0076).

**Reported at once.** ADR-0076 sent the report in `Hello` because nothing could change it without a restart. Now the server can, so the agent closes its stream and reconnects immediately when the state changes, and the new `Hello` carries it. Relays already keep and replay the last `Hello` of each agent, and open a new upstream stream with each new one (ADR-0027), so they need no change. A `CommandResult` that was not sent yet waits in the queue for the new stream, and the server sends an unfinished command again, which the journal answers. No new message type was added (see ADR-0076 on old relays).

### On the server

**`POST /api/servers/{id}/ai-commands`**, `{"on": true}` or `{"on": false}`, queues the command and answers 201 with it.

- **Who**: administrators of the server's site, of its customer or of everything, the people who approve high-impact commands there (ADR-0043). For an MSP, a customer's administrators switch their own servers. Others get 403, or 404 for a server they cannot see.
- **Refusals**, 409 with the reason: a removed server; "on" when the agent does not report `ai_commands` as accepted (the reason names the line to run there); either when it does not report `ai_commands` at all (an agent too old, which updates by itself).
- **Lifetime**: "on" expires after an hour, so a server that comes back days later does not turn on by surprise; "off" waits up to seven days for an offline server. A new request cancels an older one of the same server still queued, so the last click wins.
- **Audit**: `server.ai_commands`, with the server, the choice and the command; the command's result is audited as every command's is.
- **Notification**: `ai_commands.changed` reaches every channel covering the server's customer or site: "alice asked to turn AI commands on on web-01". Turning commands on is the step that gives AI a shell on a host, so the people watching a place hear of it.
- **Who and when** for the page: `GET /api/servers/{id}` adds `ai_commands_change`, the last `ai_commands` command of the server with its choice, state, requester and times.

`POST /api/sites/{id}/ai-commands` does the same for every server of a site that can take it, for the site's administrators: on for those reporting `ai_commands` accepted and AI commands off, off for those reporting `ai_commands` and AI commands on. It answers with the number queued and the servers skipped with why. It is the same command per server, with the same audit entry each.

### On screen

- **Add servers**: a checkbox "Allow AI commands to be turned on from the web interface", off by default, with one sentence: "Administrators can then turn AI commands on and off from the server's page; without it, they can only turn them off." It adds `MF_AI_COMMANDS_FROM_WEB=1` to the Linux line and `AI_COMMANDS_FROM_WEB=1` to the Windows one.
- **The server's page**: the AI commands fact reads "On, turned on by alice at 14:02", "Off", or "Turning on… asked by alice at 14:02" while the command waits, with "Turn on" or "Turn off" for administrators of its site. Where only off is possible it says "Can only be turned on on the server" with a link to the line to run, which the Commands tab keeps showing to administrators. A request that failed or expired says so next to the fact.
- **Administration › AI** no longer shows the install lines: it says AI commands are turned on per server from each server's page, and keeps the counts of ADR-0076.
- **The site's page** offers "Turn AI commands on/off for all its servers" to its administrators, with the result in one line.

Rejected:

- **A switch the server sets without consent.** It would let a compromised central server give itself a shell on every host, the case the local opt-in exists for.
- **Writing `agent.toml` from the agent.** It would need the agent to write its own configuration, which nothing else does, and a remote write to the file that holds the consent.
- **A field in `SensorConfig` or a heartbeat answer.** Not signed per agent, not journaled, and not audited as a command; the command channel already has all three.
- **Restarting the agent to apply the change.** It drops samples and logs for nothing; the handlers read the state at each command.
- **A consent asked again on the server's page at each "on".** The host's administrator already said yes once, and the web interface cannot ask the host.

## Consequences

- An administrator turns AI commands on and off from the server's page, where the host agreed, and off anywhere an agent knows the command, without logging in to the server.
- A central server that is taken over can still turn AI commands on where hosts agreed: the consent is that agreement, and the Add servers page says what it does. It can always turn them off, which denies AI remediation but nothing else.
- Commands queued before an "off" still reach the agent first, as commands run in order; they were classified and, for changes, approved already.
- `servers.command_kinds` gains `ai_commands`. Servers whose agent or relay is older show no switch until they update, which they do by themselves.
- A notification migration (`20261116110000`) adds the event to the outbox's list.
