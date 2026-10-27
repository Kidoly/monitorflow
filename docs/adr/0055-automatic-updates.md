# ADR-0055: Automatic agent and relay updates

Date: 2026-10-27
Status: accepted; implemented in stages (see the end)

## Context

ADR-0053 lets an administrator update agents from the web interface: one server, a campaign, or every agent of a site whose `agent_auto_update` policy was turned on. That policy is off by default, so a fleet stays on the version it was installed with until someone acts, and relays cannot update themselves at all: they have no installer, no privileged helper, and they are installed by hand.

Operators want agents and relays to follow releases on their own, as browsers and phones do, without a release turning into an outage. Three things make that hard:

- **One bad release reaches everything at once.** A self-update rolls back a binary that does not connect, but a release that connects and then misbehaves, or that cannot start on one distribution, would still be tried on every host.
- **Versions must keep understanding each other.** The protocol is additive (`proto/`), but prost drops fields it does not know when a relay decodes and encodes an envelope again, so a newer agent behind an older relay can lose what it sends; and a newer peer can send the server things it does not know.
- **Relays carry other peers.** Restarting one interrupts every agent behind it, and an agent that is being updated while its relay restarts can miss its health check and roll back for nothing.

## Decision

### Settings

Automatic updates are **on by default** for agents and relays. One row, `auto_update_settings`, holds the server-wide values, changed by global administrators under Updates › Automatic updates (`PUT /api/auto-update`, audited as `auto_update.settings`):

- `enabled`, the default of every site that does not override it; on.
- `paused`, which stops every automatic update at once, whatever the sites say, without touching updates already queued; off.
- `failure_threshold`, the number of failed updates of one release that halts its rollout (below); 2.

`auto_update.enabled` in `server.toml` (`MONITORFLOW_AUTO_UPDATE_ENABLED`) wins over the row, like other settings that come from the configuration, and the web interface then shows the switch locked.

A site overrides `enabled` with `sites.agent_auto_update`: `null` inherits, `true` or `false` wins. The migration that introduces this makes the column nullable and sets every existing site to `null`, so a site turned off under ADR-0053 now inherits "on" too: automatic updates are meant to be the norm, and the site's administrators can turn them off again (`PUT /api/sites/{id}/agent-auto-update` with `{"enabled": null | true | false}`, audited as `site.agent_auto_update`).

### Which release, which peers

**Target.** The newest **stable** release the server holds (ADR-0053's cache), and never one newer than the server itself; pre-releases are never installed automatically, only by hand.

**Peers.** Unrevoked agents connected now, in sites where automatic updates apply, that report a version older than the target. Never a downgrade, never the same version, never a peer that reports something that is not a version (a development build), never one that already has a `self_update` in flight.

**One try per peer and version.** As in ADR-0053, `agent_auto_updates` records the update queued for a server and a version, so an update that failed or rolled back is not queued again for that version: the peer waits for an administrator or for the next release. A command that expired before any agent ran it (24 hours without connecting) was never a try and is forgotten, so the peer is tried again later; one an administrator cancelled is a try.

### Staged rollout

**Canary per site.** In each site the first update to a version goes to one peer alone. Only once it succeeded, that is once the new version connected upstream and the watchdog kept it (ADR-0053), do the others follow, a quarter of the site's peers at a time (at least one). A site whose canary failed stops there for that version.

**Halt.** A rollout, one per kind of peer and version in `auto_rollouts`, halts everywhere once `failure_threshold` of its updates failed (rolled back, refused or failed otherwise). Halting is recorded with its reason, audited as `auto_update.halted`, and logged as an error; Updates › Automatic updates shows it as a banner with the failures. A global administrator who fixed the cause resumes it (`POST /api/auto-update/rollouts/{kind}/{version}/resume`, audited as `auto_update.resume`): failures count again from then, and each blocked site gets a new canary among the peers not tried yet. A newer release starts a new rollout regardless.

**Alerting.** The alert engine evaluates metrics (ADR-0017) and has no notifier beyond its log yet, so a halt is an error in the server's log, an audit event and the banner; a notification channel, when one exists, will carry it too.

**Maintenance windows.** Sites and servers have none today, only campaigns (ADR-0033). An automatic update restarts the agent or relay, not the host, and is checked within minutes, so it is queued at any time. Hosts that need a window turn automatic updates off for their site and use a campaign with windows.

**Pace.** The server looks every 5 minutes (`auto_update.interval`, at least 10 seconds). A fleet of one site of 40 agents takes a canary then four batches, about 25 minutes when all goes well; ten such sites move in parallel.

### Version compatibility

Three rules, from the protocol up:

1. **Protocol.** The server refuses a stream whose `protocol_version` has another major (unchanged).
2. **Oldest supported release.** `MIN_PEER_VERSION` in `crates/common` is the oldest agent or relay release the server accepts; it refuses an older one at the handshake with `FAILED_PRECONDITION` and "reinstall it". Builds that report no version, or something that is not a version, are development builds and are accepted. The minimum is raised only by a release whose notes say so, and never past a version automatic updates could have reached since the previous minimum.
3. **Upstream first: server ≥ relay ≥ agent.** Automatic updates never move a relay past the server's version, nor an agent past the server's version or the version of the relay it connects through. Relays are therefore updated before the agents behind them; an agent behind a relay that runs an older version waits, and Updates › Automatic updates says so. An agent update does not start while its relay's update is in flight, and a relay update does not start while an agent behind it is being updated, so neither restart spoils the other's health check.

An administrator can still update anything by hand, including past these rules; that is a decision, audited as a command.

### Relays

Relays get what agents have (ADR-0052, ADR-0053):

- **Releases.** The release workflow builds `monitorflow-relay-linux-x86_64` and `-aarch64` statically like the agent, listed in the same `SHA256SUMS`, signed by the same key. The server's cache and the relays' copies keep the relay binaries too.
- **Installer.** `install.sh` installs a relay with `MF_ROLE=relay`, doing exactly the steps of the relay guide (`docs/deploy-relay.md`): the binary, a system user `monitorflow-relay`, its directories, `relay.toml`, the sudo rule of its helper and the unit `monitorflow-relay`.
- **Self-update.** The relay accepts, on its own stream, a signed `self_update` command addressed to itself and runs it through the agent's command dispatcher (ADR-0031's signature, target, expiry and journal checks), which downloads and checks the release with `common::release` and hands it to `sudo -n /usr/bin/monitorflow-relay self-update`. The relay links the agent's library for this: the dispatcher, the download, the root helper, the swap and the watchdog take a *product* (the agent or the relay: binary, unit, watchdog unit, root's state directory `/var/lib/monitorflow/relay-self-update`, release file), so there is one implementation of each step. The watchdog's proof is the relay's own stream being accepted upstream again, which the relay records in `<data_dir>/self-update/connected`; without it within the timeout the previous binary is put back. Results go up on the relay's stream; a result the restart interrupted is answered from the status file when the server sends the command again, as for agents.
- **Server side.** The server records each relay's version from its `Hello` (`relays.relay_version`), shows it with the relays, and keeps relay updates in `relay_commands`, one in flight per relay, signed when sent like agents' commands. Administrators of the relay's customer queue one with `POST /api/relays/{id}/update` (audited as `relay.update`); automatic updates queue them with the policy above (`relay_auto_updates`, audited as `relay.auto_update`), relays before agents in each pass. The relay downloads from the server's cache and then `agent_download_url`, which carry the relay binaries of the release.

Agents behind a relay keep working while it restarts: their streams drop and reconnect with the usual backoff (ADR-0009), and they keep their samples in memory until then, as for any upstream outage.

### Rejected

- **Moving the self-update code to `crates/common`.** It depends on the agent's dispatcher, journal and sudo runner; moving them all would touch most of the agent for no change in behaviour. The relay depends on the agent's library instead, which costs build time, not binary size.

- **Campaigns as the automatic engine.** Campaigns belong to one customer and freeze their targets at creation, so automatic updates would need one campaign per site and release and new ones for every server enrolled later, filling the campaigns page. The policy above shares their ideas (waves, a failure threshold, pause) without their bookkeeping; campaigns remain for scheduled, windowed rollouts.
- **Fleet-wide canary.** A few canaries for the whole fleet would leave sites with distinct distributions or networks untested until their whole fleet moves; one per site costs a few minutes per site and isolates them.
- **Minimum version enforced by relays.** Relays forward Hellos without reading the version (ADR-0027); the server sees every Hello and is the one place to check it.

## Consequences

- New installations and existing ones without an override follow releases on their own once the server holds them. Today an administrator imports each release (`monitorflow-server agent-release import`); the background check ADR-0053 describes is not built yet, and is what will make the whole chain hands-off.
- Upgrading the server is what lets the fleet move: a server on 0.3.0 holds agents and relays at 0.3.0 even when it caches 0.4.0.
- A bad release reaches at most one peer per site before it shows, and at most `failure_threshold` failures fleet-wide before it stops.
- A relay that cannot update (installed before its helper existed) holds its agents back until it is reinstalled; Updates › Automatic updates shows which.

## Stages

1. Settings, site overrides, the policy, rollouts and their halt, the minimum peer version, the API.
2. Updates › Automatic updates in the web interface.
3. Relay releases, installer and guide.
4. Relay self-update: the helper and watchdog for the relay's product, signed commands on the relay's stream.
5. Relay versions, relay updates by hand and automatic, relays before their agents, end-to-end tests.
