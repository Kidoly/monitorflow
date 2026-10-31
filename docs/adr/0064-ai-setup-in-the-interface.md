# ADR-0064: AI setup in the interface

Date: 2026-10-31
Status: accepted

## Context

Turning on AI remediation took about eight steps on two kinds of hosts: a key file on the server, `[ai]` in `server.toml`, a restart, then `commands.shell = true` on every agent (simplicity audit, B8). The loop was built once at start, so nothing could change without a restart, and the Approvals page looked the same whether AI was on or off.

## Decision

**Administration › AI** lets a global administrator paste an Anthropic API key, pick a model, press Test and enable AI, with no file and no restart.

- **The key is stored like notification secrets (ADR-0060):** in PostgreSQL (`server_settings.ai_api_key`), written by the API, never returned. Views say `has_key`, an empty key keeps the stored one, and the audit (`settings.ai`) records only whether it changed. It is therefore in database backups. The simplicity audit proposed `serverdata`; one way of holding interface secrets is simpler than two. A "Remove key" button forgets it and turns AI off.
- **A short list of models.** The API offers Claude Opus 5.5, the default and the configuration's default too, Sonnet 5.5 and Haiku 5.5, and refuses others. There is no provider choice, token limit or timeout: those stay in `[ai]`, whose values apply to the interface's key as well.
- **Test** sends the smallest real Messages request (`max_tokens: 1`), so missing credit shows as well as a bad key, and answers in plain words what to do. It saves nothing and is not retried.
- **Enabling tests first.** Turning AI on, or changing the key or model while it is on, runs the same test, and a failure leaves everything as it was. A wrong key would otherwise surface only as the next incident escalating.
- **The configuration takes precedence.** `[ai] enabled = true` works as before: read at start, the loop built from it, a broken key failing the start. The page then says the configuration wins, offers only Test, and the API answers 409 to changes. `enabled = false`, the default, cannot be told from an unset value and means the web interface decides. A server that must never use AI simply never gets a key.
- **No restart.** Without `enabled = true`, a supervisor polls `server_settings` every 10 seconds and starts, replaces or stops the loop. Each server process does this by itself, and the loop already resumes from its audit events, so stopping it anywhere is safe (ADR-0043). While AI is off, incidents in progress and commands waiting for approval stay as they are until it is back on; alerts older than an hour are left alone as before.
- **Approvals say when AI is off**, with a link to the page for global administrators and who to ask for others.

**The agent's opt-in stays on the agent** (architecture §8): it is what keeps a compromised server from running shell commands on hosts that never agreed. It becomes one line instead: `MF_ALLOW_COMMANDS=1` in `deploy/install.sh` adds a systemd drop-in with `MONITORFLOW_COMMANDS_SHELL=true`. A drop-in rather than an edit of `agent.toml`: it works the same on new and installed hosts without parsing TOML, and leaves the file the administrator owns alone. Later runs without the variable keep it, and `MF_ALLOW_COMMANDS=0` removes it. The AI page shows the line for the default install source, at the public address, once AI is on. An agent without the opt-in now names the variable in its refusal. There is no Windows equivalent, since Windows refuses shell commands whatever the setting (ADR-0047).

Rejected:

- **An `Option<bool>` for `ai.enabled`,** to let `enabled = false` lock AI off. One more state to explain for a case nobody has asked for.
- **Waking the supervisor from the API.** It would not reach other server processes; a 10-second delay needs no explanation.
- **Choosing per server in the interface.** The server cannot grant what the agent has to refuse on its own.

## Consequences

- Upgrading changes nothing for servers with `[ai] enabled = true`.
- A failure to build the loop after enabling, like an unreadable Proxmox secret, is logged and retried at each poll; the page still says AI is on. Proxmox settings stay in `server.toml`.
- Each server does not yet show whether it accepts commands: that needs the agent to report it, a protocol change left for later.
