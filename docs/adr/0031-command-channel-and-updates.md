# ADR-0031: Command channel, package updates and package reports

Date: 2026-10-23
Status: accepted; privileged update command lines amended by ADR-0036

## Context

`Command` and `CommandResult` have been in `agent.proto` since phase 1, but nothing sent or ran them. Phase 4 needs them first for updates: an administrator (later a campaign) asks a server to apply updates, the agent runs the package manager and reports what is installed and whether a reboot is needed (architecture §7). Phase 5 will send approved AI commands down the same channel, and the agent-sources task adds a `configure_sources` kind next to `apply_updates`.

Constraints from earlier decisions:

- Commands go down the agent's existing stream; relays forward envelopes unchanged and never buffer anything going down (ADR-0025, ADR-0027).
- A compromised relay can forge upstream data for the agents of its subtree, but must not be able to inject commands (architecture §4). mTLS alone does not give that: the relay terminates the agent's TLS.
- ADR-0027 left `CommandResult` unbuffered: a result sent while a relay's upstream is down is dropped, and "the command execution task will decide whether a late result is still wanted".
- The agent runs as an unprivileged user; privileged operations go through sudo rules the packager generates (architecture §4).

## Decision

### Protocol

Additive changes; `PROTOCOL_VERSION` becomes `1.1`.

- `Command.agent_id` (7): the agent the command is for.
- `CommandResult.status` (7), a `CommandStatus` enum: `RUNNING` (an interim report, the agent started it), then one final status: `SUCCEEDED`, `FAILED` (it ran and failed), `EXPIRED` (it arrived after `expires_at`, not run) or `REJECTED` (not run: bad signature, another agent's command, unknown kind, invalid payload). `UNSPECIFIED` is read as final, succeeded when `exit_code` is 0. `CommandResult.error` (8) says why in one line for anything but success.
- `Command.payload` holds a serialized message typed by `kind`. `proto/monitorflow/v1/packages.proto` defines `ApplyUpdates` (`packages`, empty for all; `security_only`; `allow_reboot`, false by default) and `PackageReport` (installed `packages` with name, version, arch and the upgradable version if any; `reboot_required`; `collected_at_unix_ms`).
- `Envelope.package_report` (15), agent to server.

### Signed commands

Every command is signed by the server; the agent verifies before anything else and rejects a command that fails. The signature is ECDSA P-256 with SHA-256, ASN.1 DER, **made with the CA key**, over:

```
"monitorflow-command-v1\0"
u32 BE length + agent_id, u32 BE length + command_id, u32 BE length + kind,
u32 BE length + payload, i64 BE issued_at_unix_ms, i64 BE expires_at_unix_ms
```

`common::command` builds these bytes and verifies against the CA certificates the agent already stores and pins (ADR-0007), so there is no new key to distribute, rotate or pin, and a relay renewal path (ADR-0028) cannot swap it. Everything a relay could change is signed: the target agent (a command for one agent cannot be replayed to another of the relay's site), the kind and payload, and the expiry (a relay cannot keep a command alive).

The CA key otherwise only signs certificates and CRLs, whose to-be-signed bytes are DER and start with `0x30`; the prefix starts with `m`, so a command signature can never pass as a certificate or CRL signature and the other way round. The key is already online in the server process for renewals.

Rejected:

- **mTLS and relay policy only, an empty signature.** A compromised relay could then run any command as root on every agent of its site, which architecture §4 rules out.
- **A dedicated command key.** Agents would need its public key: in the enrollment answer (not for agents enrolled before), or in a CA-signed certificate sent with each command (a second chain verification on the agent). It is the right step once the CA key moves offline or into an HSM; until then it adds a key to manage for no security gain, as whoever holds the server process holds both.

### Server: storage and dispatch

Table `commands` (migration `20261023120000_commands`): `id`, `server_id`, `kind`, `payload`, `state`, `issued_at`, `expires_at`, `sent_at`, `started_at`, `finished_at`, `exit_code`, `stdout`, `stderr`, `error`, `requested_by` (a user, NULL for the system) and `campaign_id` (no foreign key until campaigns exist). States: `queued`, `sent`, `running`, `succeeded`, `failed`, `expired`, `cancelled`. A partial unique index allows **at most one `sent` or `running` command per server**: package managers hold a global lock, and running one command at a time keeps results unambiguous. Later kinds that are safe to run in parallel can lift this with a per-kind rule.

`server::commands` runs one dispatcher task:

- It dispatches to an agent when its stream opens, when a command is enqueued (the API notifies it), when a final result arrives, and every 10 s for all connected agents, which also catches anything missed.
- For one agent: if a command is `sent` or `running` and was not sent on the current connection, it is **sent again** (below); if it was, nothing happens. Otherwise the oldest unexpired `queued` command becomes `sent` (the update is guarded by the state and the unique index), is signed and goes on the stream. The task remembers in memory which command went on which connection; after a server restart it sends in-flight commands once more, which the agent answers from its journal.
- Results are matched by command ID **and** by the agent of the stream, so a relay can only report for its own agents' commands. `RUNNING` moves `sent` to `running`; a final status sets the final state, the exit code, `stdout` and `stderr` (each truncated to 64 KiB, lossy UTF-8) and `error`. A final result is accepted on `sent`, `running` and `expired`: an agent only starts a command before its expiry, by its own clock, so a late result is the truth and replaces `expired`. Final states never change again.
- The same 10 s tick expires commands: `queued` ones past `expires_at`; `sent` ones 5 minutes past it, to let a delivered command report `RUNNING`; `running` ones 2 hours after they started become `failed` with "no result from the agent".
- Final results are audited (`command.finish`, actor the peer, with state and exit code; output stays in `commands` only, as it may hold secrets).

Results are not buffered by relays (ADR-0027) and no longer need to be: a result lost with a stream is recovered because the command stays `sent` or `running`, is sent again on the next connection, and the agent answers it from its journal.

### Agent: dispatcher and journal

`agent::commands` holds a registry from kind to handler (`Handlers::register(kind, handler)`); a handler gets the payload bytes and returns an exit code, output and an optional error. The dispatcher checks, in this order, and answers `REJECTED` or `EXPIRED` with an error text without running anything: signature and `agent_id`, then the journal, then expiry, then kind.

Idempotency on `command_id` uses a journal of one small protobuf file per command in `<data_dir>/commands/`, written before the handler starts and again with the final result:

- A command already finished is answered with the stored result, never run again.
- One running in this process is answered `RUNNING`.
- One started but not finished before the agent restarted is answered `FAILED` "interrupted by an agent restart": running it again could repeat half an upgrade.

Entries are kept until a day after the command's expiry, so a replay is refused by the journal while it is unexpired and by the expiry after; at most 1,024 are kept. Commands run one at a time; another command arriving meanwhile waits behind it. Results wait in a queue of 64 while the stream is down.

### Agent: packages

`agent::packages` puts a `PackageManager` trait over apt (Debian family) and dnf (RHEL family), chosen from the OS family the agent detects. Privileged commands go through `sudo -n` with these exact arguments, so the packager can generate sudoers rules (`DEBIAN_FRONTEND=noninteractive` through `env_keep`):

| Family | Command |
| --- | --- |
| apt | `/usr/bin/apt-get -q update` |
| apt | `DEBIAN_FRONTEND=noninteractive /usr/bin/apt-get -q -y -o Dpkg::Options::=--force-confdef -o Dpkg::Options::=--force-confold upgrade` |
| apt | the same with `install --only-upgrade <packages>` instead of `upgrade` |
| dnf | `/usr/bin/dnf -q -y upgrade`, with `--security` for security-only, followed by `<packages>` when given |
| both | `/usr/sbin/shutdown -r +1`, only when the command allows a reboot and one is required |

Unprivileged reads: `dpkg-query -W` and `apt-get -s -q dist-upgrade` (upgradable versions and their origin) on apt; `rpm -qa --queryformat`, `dnf -q -C check-update` and `dnf -q needs-restarting -r` on dnf; `/var/run/reboot-required` on apt. apt has no security-only switch: the agent upgrades the packages whose candidate comes from a `-security` suite in the simulation. Package names from the server are checked against the distributions' name rules before reaching a command line; a sudoers rule with a wildcard after `--only-upgrade` or `upgrade` is as narrow as that check, so administrators who want no wildcard leave that rule out and only full upgrades work.

The agent sends a `PackageReport` after every `apply_updates`, on connect when the last one is older than the interval, and every `[packages] report_interval` (default 6 h). Upgradable versions are as fresh as the host's last metadata refresh; only `apply_updates` refreshes it.

The server keeps the latest report only: `server_packages` is replaced on each report, and `servers.reboot_required` and `servers.packages_reported_at` are set.

### REST API

`POST /api/servers/{id}/commands` (administrators of the server's customer, as for revocation in ADR-0029; `apply_updates` only for now; audited `command.create`), `GET /api/servers/{id}/commands`, `GET /api/commands/{id}` and `GET /api/servers/{id}/packages` (paginated, `?upgradable=true`), with the access helpers of ADR-0026.

## Consequences

- A relay sees commands and their payloads in clear but cannot change or forge one; command payloads must never carry secrets until end-to-end encryption exists.
- Rotating the CA rotates the command key; agents follow with the CA certificate they already update.
- A command may run once and report twice (journal answers); the server ignores repeated final results.
- An agent that restarts during `apply_updates` reports it as failed, and an operator checks the host before queuing another one.
- The sudoers file is part of the agent packages (follow-up in `deploy/`); without it every privileged command fails with sudo's "a password is required", reported as `FAILED`.
