# ADR-0189: Script library and automation rules

Date: 2026-11-23
Status: accepted

## Context

Phase 20 brings what people otherwise keep a remote management tool for. Its third task: scripts kept in MonitorFlow (shell and PowerShell), with their parameters, versions and the role allowed to run them, run on one server, a group or a whole site, now or on a schedule, with the output of each server kept; automation rules that run a script when something happens, with the same approvals and limits as AI commands; and library scripts the AI can use as ready-made fixes.

The phase's rule: every action that changes a server needs that server's agreement at install, so a compromised central server cannot give itself control of servers that did not agree. What already exists:

- Signed commands on the agent's stream, checked by the agent, journaled, refused after they expire (ADR-0031), and root helpers that check the signature themselves (ADR-0108).
- The `shell` kind of AI remediation (ADR-0043, ADR-0084): one line, as the agent's user, environment reset, killed with everything it started at its timeout. Off unless the host's administrator turned it on; the web interface may turn it on only where the host consented to that too (ADR-0098).
- Risk rules that classify a command line by rules, never by the AI (ADR-0041, ADR-0084), approvals by role and tier (ADR-0045), and per-site policies for low-impact changes.

Another task of the phase gives the AI a short catalogue of exact root actions, run by the agent's root helper (ADR-0177, in progress). Scripts must not become a second, generic way to root.

## Decision

### The library

A **script** has a name (`[a-z0-9][a-z0-9_.-]{0,62}`, what people and the AI call it), a description, an interpreter (`sh` for Linux, `powershell` for Windows), the least role that may run it (`operator`, `approver` or `administrator`; viewers never run scripts), whether the AI may propose it, and a timeout (10 s to 1 h, 10 min by default). It belongs to every customer or to one: administrators of everything manage the former, administrators of a customer as a whole the latter, and anyone reading a customer sees both.

Its **text and typed parameters are versioned**: every change of either makes a new version, earlier ones are kept and shown, and a run names the version it ran. Parameters are `string`, `integer`, `boolean` or `choice`, with a label, a default and whether they are required; a value is one line of at most 256 characters, checked against its type. Values reach the script as environment variables `MF_PARAM_<NAME>`, **never inside its text**: a value cannot change what the rules classified. A script is at most 64 KiB. Deleting one keeps its versions and runs; it is refused while automation rules run it.

### Runs

A run is one version, with its values, on servers of one customer: one or several chosen servers, every server of a site, or the servers of a business service (ADR-0155, the "group" people already name), taken as they are when the run is made. It runs now or at a time up to a year ahead.

Every run is **classified by the risk rules of AI commands**, for each server by its customer's and site's rules: each line that is not empty or a comment is classified as one command line of the script's shell, the run takes the highest tier, and a forbidden line or a value matching a deny pattern refuses it. Anything the rules do not know is high impact, so a real script with loops or variables is high impact. Then:

- **A person** who may approve that tier on every server's site (approver for read-only and low impact, administrator for high impact, as for AI commands) runs it directly: their request is the decision. Anyone else with the script's role asks, and the run waits in the approval queue.
- **An automation rule** is treated as the AI: read-only runs at once, low impact when every site's policy lets AI changes run on their own, the rest waits for approval.
- **Forbidden** never runs.

A run waiting for approval expires after 24 hours. Approving classifies it again: rules added since still refuse it, and the role needed is that of the higher tier. When its time comes, every server gets a signed `run_script` command, queued behind its other commands, waiting an hour beyond the script's timeout for an offline server. A server that was removed, runs the other system, reports scripts off, or that a rule now forbids is skipped, with the reason. **Each server's output** is the command's row: exit code, the last 64 KiB of stdout and stderr, times, kept as every command result is, and shown per server.

Approve, reject and cancel are audited (`script_run.*`), as are runs (`script.run`) and changes to scripts and rules.

### On the agent

`run_script` carries the interpreter, the text, the parameters, the timeout, the script's ID and version. The agent runs it **as its own user**, with the isolation of `shell`: environment reset (plus the parameters), no stdin, from `/` or `C:\`, its own process group or job object, killed with everything it started at the timeout. On Linux the text goes to `/bin/sh -c`; on Windows it goes on stdin to Windows PowerShell 5.1, read whole and run as one script block, so its size is not bound by the command line. An agent refuses an interpreter of the other system.

**Local consent, off by default.** `commands.scripts` in `agent.toml`, which only the host's administrator writes (ADR-0050, ADR-0098): `MF_ALLOW_SCRIPTS=1` with the Linux installer (a systemd drop-in, `0` takes it back), `monitorflow-agent.exe scripts on|off` or the MSI property `ALLOW_SCRIPTS=1` on Windows. There is no switch from the web interface and no command that turns it on: `Hello` reports `run_script` as `accepted` or `off` (ADR-0076) and the server skips servers that refuse, saying how to allow scripts there.

Why off by default, when ADR-0175 ticks the web consent of AI commands by default: that box only lets an administrator turn AI commands on later, on the server's page, audited and notified, and AI commands are single lines classified by rules. A script is arbitrary code written by people of the central server; with this switch on, whoever controls the central server can run code as the agent's user on the host. That is exactly what the phase's rule says the host must agree to. Add servers offers "Allow scripts from the script library", unticked, which adds `MF_ALLOW_SCRIPTS=1` or `ALLOW_SCRIPTS=1` to the install line.

**No root.** Scripts never reach root: no sudo rule, no helper, no elevation. A fix that needs root is for the catalogue of root actions (ADR-0177), whose exact actions the root helper checks itself; a script that needs root fails with the agent's permissions, and says so in its output. The two do not overlap: scripts are the administrators' own code at the agent's privileges; root actions are MonitorFlow's fixed list.

### Automation rules

A rule covers everything, a customer or a site (as integration endpoints), and runs a script with given values on the server where an event happens:

- `incident_opened`: an incident opens, of any metric but `offline` (nothing runs on a server that stopped reporting), or of one metric;
- `check_failed`: an incident opens on one named check;
- `server_joined`: a server enrolls for the first time.

Rules read the events integrations already get (ADR-0102), written in the transaction of each change, so nothing new hooks into alerting or enrollment. Each event a rule takes is recorded first (`automation_firings`, keyed by rule and event), so every instance of a cluster may look and only one acts. A rule takes only events after it was made and of the last hour: it never replays the past. **Limits**, beside the approvals: one run per server per cooldown (60 minutes by default), and 50 runs per rule per hour. A skipped event says why on the rule's page. A rule's script must be one of every customer, or the place's customer's own; its administrators manage it.

### For the AI

The remediation prompt lists, after its limits, up to 30 scripts marked for AI use that fit the server's shell (the customer's own winning over one of every customer with the same name), with their parameters. The AI proposes one as its whole command, `mf-script <name> [name=value ...]`; no new tool is added to the provider layer (ADR-0175 rejected that for the same reason). The loop resolves it among the scripts offered, never guessing a name; classifies the script's lines and values as above; and approves, parks or refuses it like any command, inside the incident's command and duration limits. It is sent as `run_script` with the incident's command timeout as a ceiling, its output goes back to the AI, and the incident's history keeps it. A server refusing scripts escalates with `scripts_off`. Approving such a proposal checks the server's `run_script` report instead of `shell`.

### API and screens

Everything is in `/api/v1` under the area `remediation`, documented in the OpenAPI document: `/remediation/scripts` (list, add, get with versions, change, delete, run), `/remediation/script-runs` (list, get with output, approve, reject, cancel), `/remediation/automation-rules` (list, add, get with firings, change, delete).

Remediation in the sidebar gains **Scripts** (the library, a script's page with its versions, its run form and its runs, and a run's page with each server's output) and **Automation** (the rules). **Approvals** shows script runs waiting for a decision under the AI's commands. **Add servers** gains the unticked box above.

Rejected:

- **A web switch for scripts, as ADR-0098 has for AI commands.** It would let the central server enable code execution it controls; the consent would be one click away from a compromised server on every host that ticked the box.
- **Reusing `commands.shell` as the consent.** AI commands and scripts are different powers; a host that let AI run classified one-liners did not agree to arbitrary scripts.
- **Parameters written into the script's text.** Every value would need quoting per shell, and a value could change what the rules classified.
- **Classifying a script as one command.** Multi-line text fails the lexer at once; per line keeps deny patterns and allow rules meaningful, and an unknown line still makes it high impact.
- **Scripts as root through a sudo rule.** It is the generic root ADR-0177 exists to avoid.
- **A new trigger on incidents and servers for rules.** The integration events are already the durable, transactional record of these events.
- **Recurring schedules.** A run is now or at one time; campaigns' windows and recurrence can follow if asked for.

## Consequences

- Administrators keep their fixes in MonitorFlow, versioned, and run them on a server, a business service or a site, now or tonight, with each server's output on one page.
- Nothing runs on a host whose administrator did not set `commands.scripts`; existing installs refuse scripts until they do. The run page names the line to run there.
- Most real scripts are high impact and need an administrator, or an administrator's approval: the rules know no more of a loop than of any unknown command. Read-only scripts made of known commands run for operators and rules alike.
- Automation rules act within seconds of an incident, through the same approval queue as the AI, and stop themselves after their limits.
- The AI can propose the administrators' own scripts; their risk is the script's, not the AI's opinion.
- `servers.command_kinds` gains `run_script`. A migration (`20261221000000`) adds the tables.
