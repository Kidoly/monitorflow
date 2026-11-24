# ADR-0181: AI root actions, and the diagnosis in the reader's language

Date: 2026-11-23
Status: accepted; Linux only

## Context

AI remediation is what sets MonitorFlow apart, and it failed on a plain incident. A Linux server's root disk reached 91 %. The AI found why, 4.6 GB of `.deb` files in `/var/cache/apt/archives` and 3.2 GB in `/var/log/journal`, then escalated: "Every safe fix needs root, and commands with sudo are refused, so a human has to act". That is the design so far. AI commands run as the agent's unprivileged user (ADR-0043), and the risk rules refuse `sudo` and every other way to change privileges (ADR-0041). The commands that would have fixed it, `apt-get clean` and `journalctl --vacuum-size`, were even classified low impact, and would have failed for want of root.

Free root commands stay out of the question. A central server that is taken over signs commands for every host; what holds it back is the host's own consent (ADR-0098) and helpers that act only on typed, signed requests (ADR-0108).

Two smaller faults showed on the same incident page: the AI wrote in English to a team reading the interface in French, and its Markdown showed raw, `**` and backticks included.

## Decision

### A catalog of root actions, never a command line

The AI may ask for one of four actions, written as a command of a program no host has:

| Action | Runs as root | Parameters |
| --- | --- | --- |
| `monitorflow-root clean-package-cache` | `apt-get clean`, else `dnf clean packages`, else `zypper --non-interactive clean`, whichever is installed | none |
| `monitorflow-root vacuum-journal <size>` | `journalctl --vacuum-size=<N>M` | `500M` or `2G`, from 100 MB to 64 GB |
| `monitorflow-root prune-docker` | `docker image prune --all --force`, then `docker builder prune --all --force`; refused where `/usr/bin/docker` is missing | none |
| `monitorflow-root restart-service <unit>` | `systemctl restart <unit>.service` | a unit name, `.service` added when it has no unit suffix; never another unit type, a template, or a critical service: ssh, systemd's own, D-Bus, networking, NetworkManager, polkit, the consoles, user managers, MonitorFlow's own |

`monitorflow_common::root_action` holds the catalog: the server parses the AI's line with it, and the agent's root helper checks the signed payload with it again. A new action is a new variant of the `RootAction` protobuf message, a new parser arm and an amendment of this decision.

### How it travels

- **Risk rules.** `monitorflow-root` is decided by the built-in rules after the deny patterns, the character checks and the strict lexer (ADR-0041): alone (no pipe, no glob, no environment prefix) and well formed, it gets its action's tier; anything else is forbidden (`root.invalid`). Administrators' allow rules never apply to it, their deny patterns still do. On Windows it is forbidden (`root.unavailable`).
- **The runner** sends a root action as a new command kind, `root_action`, with the typed `RootAction` payload, never as a `shell` command. It travels, is signed, journaled and expires like every command (ADR-0031).
- **The agent** refuses `root_action` unless AI commands are on (ADR-0098) and the host allowed root actions, checks the payload, and passes the signed command to `sudo -n /usr/bin/monitorflow-agent root-action`, one new exact sudo line (ADR-0032, ADR-0036, ADR-0094).
- **The helper**, as root, trusts nothing the agent says. It reads the consent from `agent.toml` itself, without the environment; checks the command with its own trust anchor, for this machine, of kind `root_action`, unexpired and never run before (ADR-0108); checks the parameters; then runs fixed programs, by full path, with a reset environment (`PATH` of the system directories, `LC_ALL=C`) and no stdin.

### Classification, and why

| Action | Tier | Runs |
| --- | --- | --- |
| `clean-package-cache` | low impact, rule `root.clean_package_cache` | at once, like a read-only command |
| `vacuum-journal` | low impact, rule `root.vacuum_journal` | at once, like a read-only command |
| `prune-docker` | high impact | after approval (Remediation › Approvals) |
| `restart-service` | high impact | after approval |

- **The package cache** holds copies of packages already installed or never needed. Package managers download them again when they need them; nothing is lost but bandwidth. No snapshot is taken first: it would cost more than what it protects.
- **Vacuuming the journal** removes the oldest archived journal files and keeps at least 100 MB, so the recent days stay. Old logs have some value, but a full root filesystem loses new logs and more, and logs a team must keep belong in a log store, which MonitorFlow's own log collection is (ADR-0091). No snapshot either.
- These two run without a person whatever the server group's "run low-impact changes automatically" policy says. They stay `low_impact` rather than `read_only` in the classification, the audit trail and the approval queue, which would otherwise say something false about them.
- **Pruning Docker** removes images a stopped container, a rollback or the next deployment may need, some of them built locally or from a registry that is gone. A person decides.
- **Restarting a service** interrupts what it serves, and the AI picks which. `systemctl restart` as the agent's user is low impact, but it seldom works without root; as root it always does, so a person approves it, whatever the group's policy. Critical services are refused outright, before any approval: an approver should not be one click from cutting the host off.

### Consent stays on the host

**`commands.root_actions`** in `agent.toml`, off when absent, allows root actions while AI commands are on. Turning AI commands off, from the host or the web interface, turns root actions off too. The central server can never grant it: the helper reads the file itself, which only root writes (`root:monitorflow 0640`), and ignores the environment, so neither a command nor the agent's user can set it.

- **A new install allows it**, as the Add servers page now ticks "Allow AI commands to be turned on from the web interface" by default (ADR-0175): `install.sh` runs `monitorflow-agent ai-commands root-actions on` when it writes a new `agent.toml`. `MF_AI_ROOT_ACTIONS=0` refuses them at install, `MF_AI_ROOT_ACTIONS=1` allows them, also on a host already installed; without the variable an existing install keeps its choice.
- **On the host**, `sudo monitorflow-agent ai-commands root-actions on|off` edits the key and keeps the rest of the file. The helper applies it at its next run; the agent reports it once restarted.
- **Existing installs** have no key, so no root actions, until their administrator runs the installer again or the command above. A self-update never changes `agent.toml`.

The agent reports `root_action` in `Hello` (ADR-0076): `accepted`; `off` without the consent or with AI commands off; `unsupported` on Windows, and on Linux when `sudo -n -l` says the sudo rules do not know the helper. The runner lists the actions in the prompt only for a server that reports `accepted`, and tells the AI otherwise that root actions are not available there. A root action for a server that reports a refusal escalates before anything is queued, with the code `root_actions_off`, `root_actions_sudo_rules` or `root_actions_windows` and the way out.

### Existing servers and the sudo line

Self-update replaces the binary only, and its watchdog runs the previous binary (ADR-0053), so it cannot bring the new sudo line. Servers installed before this decision get it the way they got the snapshot lines (ADR-0094): by running the install command again, which downloads the release's `monitorflow-agent-sudoers`, checks it with `visudo` and installs it with the new binary. Until then the agent reports `unsupported`, the incident says "the sudo rules on this server predate AI root actions; run the install command again", and a helper run that reaches sudo anyway reports the same instead of sudo's "a password is required". There is no distribution package of the agent; release files carry the sudo rules (`deploy/release-files.sh`).

### Windows

Not in this decision. The Windows agent has no root helper but the update service (ADR-0049), whose pipe accepts typed requests for updates only; adding service restarts and cache cleaning there is a design of its own. The Windows agent reports `root_action` as unsupported, the prompt does not mention it, and the rules forbid it.

### The diagnosis in the reader's language

The server keeps no language per organization, only each person's interface language (ADR-0137), and an incident is read by everyone covering its server. So: **`server_settings.ai_language`**, `en` or `fr`, set by a global administrator under Administration › AI or with `PUT /api/v1/settings/ai/language`, audited as `settings.ai_language`. Unset, it follows the people: the language most enabled users chose for the web interface, English when none did or on a tie. `GET /api/v1/settings/ai` shows `language` and `effective_language`. It is not part of `[ai]`, so it stays editable when the configuration sets AI.

The prompt asks for the diagnosis, the reasoning and every reason in that language, keeping commands, paths, unit names and quoted output as they are. The runner's own sentences (closing reasons) keep their stable codes, which the web interface words (ADR-0144).

### Markdown the interface draws

The prompt tells the AI what the interface draws: `**bold**`, `` `inline code` ``, lists and paragraphs, nothing else. The incident page and the approval queue draw exactly that, with the `ai` variant of the `Markdown` component of text panels (ADR-0166): React elements only, never HTML, no `dangerouslySetInnerHTML`. Unlike the panels, the AI's words get no links (its text is untrusted) and no headings, and a single `*` or `_` stays as written, since paths and globs (`/var/log/*.gz`, `__pycache__`) use them far more often than italics would. No library was added.

### Rejected

- **`sudo` for listed commands, or a sudoers line per tool** (`apt-get clean`, `journalctl --vacuum-size=*`). Sudo's wildcards match spaces and options, the arguments would come from the AI, and each new tool would need a new rule on every host. A signed, typed request to one helper checks everything in one place.
- **A shell command with `sudo` that approvers vet.** It would let a compromised central server, which can also approve, run anything as root on every host.
- **Read-only tier for the two automatic actions.** Simpler in the runner, but the audit log and the approval queue would call a deletion read-only.
- **A per-customer language.** No such setting exists, and customers of one MSP are often served by the same team. A per-incident language picked from the person who opened it does not exist either: the loop opens incidents by itself.
- **A Markdown library.** The interface needs four constructs, which the existing component draws; a library brings HTML handling to configure safely and weight to the bundle.

## Consequences

- On a new Linux install with AI commands on, the incident of the context is fixed without a person: the AI looks, clears the package cache and vacuums the journal, and the sensor confirms.
- A central server that is taken over can, where hosts consented, clear package caches and vacuum journals at will, and queue Docker prunes and service restarts that wait for approval. With the approval of an approver it controls, it can restart services and remove unused Docker images. It still cannot run any other command as root.
- The prompt is longer on Linux servers that accept root actions.
- `servers.command_kinds` gains `root_action`; the server's page lists it among the kinds a server accepts or refuses.
- Existing Linux servers need the install command run again for the sudo line and the consent; until then their incidents say so.
- Follow-ups: Windows counterparts through the update service; more actions (rotating a log the AI names, clearing a configured cache directory as root) as amendments here.
