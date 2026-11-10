# ADR-0108: Root helpers verify signed commands

Date: 2026-11-10
Status: accepted; implemented in stages (see the end); amends ADR-0032, ADR-0036, ADR-0053 and ADR-0094

## Context

The agent and the relay run as unprivileged users and reach root through exact sudo lines to helpers of their own binary (ADR-0032, ADR-0036, ADR-0053, ADR-0094); on Windows the agent's service account asks the update service, which is LocalSystem, over a pipe (ADR-0049). Each helper checks the shape of what it is given, and believes the caller for the rest: "command signatures are checked by the agent before it calls the helper" (ADR-0032).

Code running as the service's user is a realistic start: a script check, a plugin, an AI shell command, or a bug in the agent. The security audit of 2026-11-09 found what that user can then do:

- **Become root.** `sources apply` accepts any repository URL and any key whose fingerprint matches itself, and `packages update` upgrades installed packages from the configured repositories. Together: point apt or dnf at one's own repository, then upgrade `bash` to a package whose maintainer script runs as root. ADR-0032 accepted its half because "it would still need a way to install packages as root", ADR-0036 its half by assuming "the configured repositories".
- **Downgrade.** The self-update helper and the Windows update service read `allow_downgrade` from the caller, so any older signed release can be installed with no administrator and no audit entry, which ADR-0053 rules out.

The helpers cannot simply check the command's signature with what the agent has: the CA that signs commands (ADR-0031) and the certificate that names the machine are in the service's data directory, which its user writes, and a change of CA reaches them there (ADR-0088). `agent.toml` is root's, but holds neither.

## Decision

### Helpers act only on a signed command

The service passes a helper the command exactly as the central server signed it, and the helper checks, before anything else: the signature against **its own** copy of the CA, that the command names this machine, its kind against the helper's, its expiry, and that it has not run before. It then acts on the signed payload and on nothing else it was given. The update specification, the snapshot name, the repositories and `allow_downgrade` are no longer the caller's to say.

A request is a 4-byte big-endian length and a protobuf `HelperRequest` holding the `Command`, on stdin, followed by what the helper already read there when the payload does not hold it (the release, for a self-update). `agent::helper_trust::authorize` is the one place that checks it.

| Helper | Kind of command | Acts on |
| --- | --- | --- |
| `sources apply` | `configure_sources` | the signed `ConfigureSources` |
| `sources revert` | `revert_sources` | nothing more |
| `packages update` | `apply_updates` | the update specification built from the signed `ApplyUpdates` |
| `snapshots take`, `snapshots delete` | `take_snapshot`, `delete_snapshot` | the signed name |
| `self-update`, agent and relay | `self_update` | the signed version, which the release must be, `allow_downgrade` and the health timeout |
| Windows update service: `install`, `reboot`, `self_update` | `apply_updates` (a reboot also needs its `allow_reboot`), `self_update` | the same |

Left as they are, because they change nothing: `snapshots inspect`, and `scan` and `status` on Windows. The Linux sudo line `shutdown -r +1` stays too: it is not a helper, the service's user can reboot the host with it and no more, and taking it away needs new sudo rules on every host.

### The helper's own trust anchor

The helper keeps the CA certificates it trusts and the machine's ID in a directory only its user writes: `/var/lib/monitorflow/helper/` for the agent, `/var/lib/monitorflow/relay-helper/` for the relay, a folder of the update service on Windows. It refuses a directory that belongs to someone else or that others can write, as the self-update helper does for its state.

The anchor is made once, the first time a helper runs:

- **From the install command.** Every enrollment token carries the SHA-256 of the CA it trusts (ADR-0007). The installer, which runs as root, writes it to `/etc/monitorflow/agent-ca.pin` or `relay-ca.pin` (the hidden `pin-ca` subcommand, whenever it is given a token; a token of another CA than the one a connected machine uses is an old one and changes nothing). The helper then takes, from the service's CA bundle, the CA of that fingerprint and no other, checks that it signed the service's certificate, and keeps the ID and role that certificate names. Whatever else the service's user put in its files is ignored.
- **Adopted,** on a machine installed before this decision, which has no pin: the helper trusts the CAs the service holds at that moment, once, and records in the anchor that it was adopted. A machine whose service user is already someone else's at that moment gains nothing; every other one is protected from then on, and none needs a visit. Running the install command again writes a pin, and the anchor is made again from it.

After that the service's files are not read again for trust. A helper asked before the machine ever connected refuses.

A change of CA (ADR-0088) reaches the anchor through the signed bundles of ADR-0107, which the service carries to the helper with each request: a stage of its own, below.

### Replays

The agent's journal (ADR-0031) is the service user's and stops nothing here. The helper records each command ID it accepted in its own directory before acting, and refuses it a second time; the record is dropped once the command has expired, as the expiry then refuses it. A command the helper accepted and failed on is not run again, as for the journal: the administrator queues another.

### Rejected

- **A root-owned version floor** for downgrades alone. It closes the smaller finding and leaves the larger one.
- **Checking the repository URL and key against a list on the host.** The list would be one more thing to install and keep current on every host, and package updates, snapshots and the next helper would each need their own rule.
- **A broker running as root** that holds the stream and verifies commands itself, the agent becoming its client. It is the cleaner split and a rewrite of the agent; helpers that verify give the same guarantee for the commands that reach root.
- **Refusing until the install command is run again** on machines without a pin. Updates, sources, snapshots and further self-updates would stop on every existing host at once.

## Consequences

- The service's user can no longer make a helper do anything the central server did not sign for that machine: not choose repositories or keys, not downgrade, not take or delete a snapshot, not run an update. It can still withhold a command, as it always could.
- ADR-0032's and ADR-0036's consequences about a compromised agent user no longer hold as written; both now rest on this decision.
- A compromised central server or CA key signs commands as before (ADR-0031, ADR-0053): this decision is about the service's user, not the server.
- Linux hosts keep their sudo rules: the command lines do not change, only what the helpers read.
- A self-update to the first release with this decision is installed by the old helper; from then on every helper call is verified. A signed downgrade to an older release brings the old helpers back, by an administrator's audited choice.
- On Windows the update service keeps running the previous executable for about a minute after a self-update (ADR-0053); a command in that minute fails and is queued again.
- A command's signature names an ID and no role (ADR-0105). On a fleet that still has an agent and a relay of one ID from before ADR-0105, one's `self_update` passes the other's helper, which installs that product's own signed release of the version named.
- An agent newer than its central server follows a change of CA only once the server sends signed bundles (ADR-0107).

## Stages

1. `agent::helper_trust`: the request, the anchor, the pin, adoption, the record of used commands.
2. The anchor follows a change of CA, from the signed bundles of ADR-0107.
3. The self-update helper of the agent and the relay, and the installer's pin.
4. The `sources`, `packages` and `snapshots` helpers.
5. The Windows update service, and the MSI's pin.
