# ADR-0053: Signed releases and agent self-update

Date: 2026-10-27
Status: accepted; implemented in stages (see the end); the per-site policy amended by ADR-0055 (automatic updates)

## Context

ADR-0052 publishes agent releases and makes re-running the installer the upgrade path. On a fleet that means a shell on every host for every release, which is what MonitorFlow exists to avoid. Operators want to update agents from the web interface: one server, or a rollout over many.

An agent that replaces its own binary runs code as root on every host it manages, so the question that decides everything else is who may produce that code. Today the files' integrity rests on HTTPS to the release host: `SHA256SUMS` comes from the same place as the binaries, so whoever controls that place, or a site's self-hosted copy, controls what is installed (ADR-0052, Integrity). Agents also sit behind relays and on sites without Internet access (ADR-0025, ADR-0034), and the Linux agent runs unprivileged, doing privileged work through exact sudo rules and root helpers that check their input (ADR-0032, ADR-0036).

## Decision

### Signed releases

**Signature.** The release workflow signs `SHA256SUMS` with an Ed25519 key and publishes the raw 64-byte signature as `SHA256SUMS.sig`, as `openssl pkeyutl -sign -rawin` writes it. A new file, `VERSION`, holds the release's version and a newline and is listed in `SHA256SUMS`, so the signature covers the version as well as every file. `deploy/release-files.sh` writes `VERSION`, signs, and checks the signature it made.

**Trust is compiled in.** `deploy/release-keys.pem` holds the public keys, PEM SubjectPublicKeyInfo as `openssl pkey -pubout` writes them. `crates/common`'s build script embeds that file (a test build may name another with `MONITORFLOW_RELEASE_KEYS`), and `common::release` checks, in this order and before reading anything else: the signature against any of these keys, `SHA256SUMS`' format (plain file names, one line each), `VERSION` against its line, the version's syntax, then each file against its line, and that a binary is an executable for the machine (ELF class, byte order and machine, or a PE image for x86-64), so files swapped between architectures in a signed release do not reach the service. The agent installs nothing that fails one of these checks, wherever the files came from: GitHub, a self-hosted directory, the server's or a relay's cache. The source is therefore untrusted, and nothing else about it needs to be.

**Downgrades** are refused: the target version must be newer than the running one (Semantic Versioning order, a pre-release before its release), the same version is a no-op, and an older one needs `allow_downgrade`, which only an administrator sets, per command, and which is audited. An attacker who controls a source can therefore serve an old genuine release only to agents told to downgrade, and can at worst withhold a new one.

**Why not trust the server.** Commands are already signed by the server's CA key (ADR-0031), and the server could sign binaries the same way. But that key, the server process and the database are online and reachable from the web interface. If the server's key were enough, a compromised server, or its CA key, would run arbitrary code as root on every agent host at once, with no step on the agents to stop it. With an offline release key, a compromised server can still queue the commands it could queue before, but it cannot introduce a new agent binary: it can only ask agents to move to releases the project published. The server's mirror signing key (ADR-0030) is also online and already lets a compromised server serve packages to hosts that use its mirrors; this decision adds no second path of that kind, and protects sites that do not use the mirrors at all.

Rejected:

- **minisign.** Same Ed25519, but its default format prehashes with BLAKE2b and adds a trusted comment and a global signature, which means another verifier in the agent, and few hosts have the `minisign` tool for the installer. OpenSSL 3 is on every current distribution the agent supports (Debian 12, Ubuntu 22.04, RHEL 9 and later) and verifies the raw format directly.
- **Sigstore / cosign keyless.** Verification needs the transparency log and the certificate authority online, which air-gapped sites cannot reach, and a large verifier.
- **GPG**, like the mirror key. A large format to parse in the agent for one signature, and the same online-key problem if the server signed.
- **Authenticode** on the Windows files is still worth having for SmartScreen and administrators, but it needs a paid certificate; the agent checks `SHA256SUMS.sig` on Windows too.

**The private key** never touches a server or a host. The maintainer generates it offline, stores it as the repository secret `RELEASE_SIGNING_KEY`, which only the publishing run of `release.yml` reads (a `v*` tag), and commits the public half to `deploy/release-keys.pem`. The release job refuses to publish without the secret, and `release-files.sh` refuses a signing key whose public half is not in `release-keys.pem`, since agents would never accept that release. Other runs of the workflow sign with a throwaway key and publish nothing. The steps are in `docs/releasing.md`.

**Key rotation.** Agents accept any key in their list. To rotate: release N adds the new public key to `release-keys.pem` and is signed with the old key; once agents run N, release N+1 is signed with the new key and drops the old one. An agent older than N must go through N, or be reinstalled. If the old key is compromised, an attacker can sign for every agent that still trusts it, so the recovery is the same rotation done quickly, plus reinstalling hosts that cannot be updated through N; the installer of the new release carries only the new key.

**The installer** gets the keys from `release-keys.pem` at release time, in place of a marker line, and, with OpenSSL 3 present, verifies `SHA256SUMS.sig` before it checks any checksum; a missing signature or one that verifies against no key stops it before anything changes. Without OpenSSL 3 it warns and checks the checksums only, unless `MF_REQUIRE_SIGNATURE=1`; an installer run from the repository, without keys, warns too. `MF_ALLOW_UNSIGNED=1` installs an unsigned release (0.1.0) with checksums only. This protects a site that fetches the installer from GitHub and the files from a mirror; an installer fetched from a compromised place can of course be rewritten, as ADR-0052 says of the one-liner itself.

### Linux: the `self-update` helper

**Command.** A new kind `self_update` with payload `SelfUpdate`: the target `version`, ordered `base_urls` to download from, `allow_downgrade`, and `health_timeout_seconds` (default 300, 30 to 3,600).

**Download, as the agent.** The unprivileged agent downloads `SHA256SUMS`, `SHA256SUMS.sig`, `VERSION` and its own asset from the first base URL that has them, into memory (a binary is about 20 MB). It checks everything as above before going further, and that `VERSION` is the version asked for, so a URL serving another version moves on to the next one. HTTPS URLs are checked against the system's and the bundled web roots; plain HTTP is accepted for the mirror listeners of the server and relays, which already serve public, signed content that way (ADR-0030, ADR-0034): the signature, not the transport, protects it.

**Install, as root.** The agent then runs `sudo -n /usr/bin/monitorflow-agent self-update` and writes on its stdin one JSON header line (`SHA256SUMS`, the signature, `VERSION`, the command ID, the timeout, `allow_downgrade`) followed by the binary. The sudo rule allows exactly that line. The helper trusts nothing it reads: it verifies the release again with the keys built into itself, the installed binary, and checks the version against its own. It streams the binary into `/usr/bin/.monitorflow-agent.new` (root, 0755, created exclusively), hashing as it writes, checks the hash and the executable header, syncs it, copies the running binary to `/var/lib/monitorflow/self-update/previous` (root, the directory 0755 and never writable by the agent's user), renames the new file over `/usr/bin/monitorflow-agent` and syncs the directory. A crash at any point leaves either the old or the new binary in place, never a partial one. A lock file serialises helpers, and a second update is refused while one is being watched.

**Restart without killing itself.** The helper runs inside the agent's service, so `systemctl restart` from it would kill it halfway. It starts a transient unit instead, `systemd-run --unit monitorflow-agent-self-update --collect`, running the saved previous binary, known to work, as `self-update watch`; the helper then returns, and the agent waits. The watchdog runs outside the agent's cgroup: it restarts `monitorflow-agent` and waits for the new agent to prove itself.

**Health and rollback.** An agent writes `<data_dir>/self-update/connected` (its version, its process ID and the time) each time the upstream accepts its stream. The watchdog waits until that file names the target version and a time after the restart. If it does not within the timeout, the watchdog puts the previous binary back the same atomic way and restarts the service. Either way it records the outcome in `/var/lib/monitorflow/self-update/status.json` (root, readable by all): `succeeded` or `rolled_back`, with the versions, the command ID and a reason.

**The command's result.** The old agent is stopped while its handler waits, so the command stays unfinished in its journal (ADR-0031). When the server sends it again on the next connection, to the new agent or to the old one after a rollback, the dispatcher asks the self-update module about this unfinished entry before declaring it interrupted. With an outcome in `status.json` for that command it answers: succeeded ("updated from 0.2.0 to 0.3.0") or failed ("rolled back to 0.2.0: 0.3.0 did not connect within 300 s"). While the watchdog still waits, the command is reported running and the answer comes once the status file changes. A host rebooted during the watch has no watchdog any more: past the deadline, an agent running the target version and connected reports success, one running another version reports failure.

**Existing hosts** need the new sudo line once. Re-running the installer installs it with the new binary; until then a `self_update` fails with sudo's "a password is required" and nothing changes.

### Windows: through the update service

The MSI installs `MonitorFlowUpdate` (ADR-0049) next to the agent's service, in the component that owns the binary they share, so new installs and MSI upgrades get it; ADR-0050 left it manual. The agent downloads and checks the release as on Linux, stages it in `<data_dir>\self-update\staged\` and sends `self_update` on the pipe with its command ID, data directory and timeout. The service, as LocalSystem, reads the staged files once into memory and checks those bytes with the keys built into itself, so what the agent's account changes afterwards does not matter. It writes the new executable next to the installed one, renames the running `monitorflow-agent.exe` to `monitorflow-agent.previous.exe` (Windows allows renaming a running image; the service itself runs it) and the new one into place, records the status in `%ProgramData%\MonitorFlow\update\self-update\status.json`, which the agent's account can read but not write, and answers. A thread then restarts `MonitorFlowAgent` through the service control manager and watches for the same `connected` file; a rollback renames the new executable aside and the previous one back. Once the new agent is kept, the service exits with an error, and its failure actions restart it on the new binary a minute later. Windows Installer still records the version the MSI installed; a later MSI upgrade replaces the files as usual, and a repair of the older MSI puts the older binary back. Swapping the executable was chosen over running `msiexec` on the new MSI because a major upgrade stops and removes the update service that runs it, and its own rollback cannot go back to an older version.

### Distribution

The server keeps a cache of releases under `<mirror_dir>/agent/<version>/` with the six files the agents need (the three release files and the Linux and Windows executables), served by its mirror listener and listed in `/index.json`. `monitorflow-server agent-release fetch [--version V]` downloads a release from `agent_download_url`, and `agent-release import DIR` copies one from a directory, for air-gapped sites; the server checks the signature before it keeps anything. A background check, every `agent_release_check_interval` (6 hours, 0 for none), fetches a newer release when `agent_download_url` has one. Relays that mirror copy the agent releases of the upstream's index like snapshots (ADR-0034), checking them the same way, and keep the newest two.

A `self_update` command lists the base URLs in this order: the relay's mirror URL when the agent's relay reports one (ADR-0037), the server's `mirror_public_url`, and the release on `agent_download_url`. The agent tries them in that order; any of them may be missing, stale or hostile without harm.

Rejected: **sending the binary over the command stream**. Relays forward envelopes without interpreting them and do not buffer what goes down (ADR-0027), so a fleet update would carry the binary once per agent across every link, with chunking, flow control and resumption to add to the protocol, while the mirror listeners already cache and serve files near the agents.

### Server and web interface

- `POST /api/servers/{id}/commands` accepts `self_update` with a `version` from the cache, `allow_downgrade` and `health_timeout_s`, for administrators of the server's customer like other commands; audited as `command.create`.
- `GET /api/agent-releases` lists the cached versions and the latest. The fleet and server pages show each agent's version, from its `Hello`, against the latest, and an "Update agent" action.
- The status of an agent's last `self_update` is derived from the command: `pending` (queued or sent), `running` (downloading, verifying, installing or waiting for the restart; the output says which), `updated`, `rolled_back` (failed, with the rollback reason), `failed`, and `expired` or `cancelled`.
- Rollouts reuse update campaigns (ADR-0033): a campaign with an `agent_version` sends `self_update` instead of `apply_updates`, with the same targets, waves, windows, failure thresholds and controls, and without package options or pinned snapshots.
- A per-site `agent_auto_update` policy, off by default, makes the server queue `self_update` to the latest cached version for that site's online agents that are older, at most once per server and version, so a rollback is not retried in a loop. Queuing is audited as `agent.auto_update`.

### Relays

Relays are installed by hand and have no installer or helper yet; their self-update is a follow-up once they are packaged.

### Stages

1. Signed releases: `common::release`, `VERSION` and `SHA256SUMS.sig`, the release workflow, the installer's verification, `docs/releasing.md`.
2. The agent's `self_update` command, the Linux helper, watchdog and rollback.
3. The server's release cache, the API, the agent versions and the "Update agent" action in the web interface, and the end-to-end test.
4. Campaigns with `agent_version` and the per-site policy.
5. Relays copying releases.
6. Windows: the update service in the MSI and the executable swap.

## Consequences

- No binary reaches an agent host unless the project's release key signed it, even when the server, a relay or a download mirror is compromised; the price is a key the maintainer must keep offline and rotate through a release.
- `v0.1.0` is unsigned and has no `VERSION`: agents can self-update only from the first release that has this code, and to signed releases after it. Hosts on 0.1.0 re-run the installer once.
- Until `deploy/release-keys.pem` holds a key, agents refuse every self-update and the release workflow refuses to publish.
- Hosts upgraded by self-update keep the unit and the sudo rules they had; a release that changes either still needs the installer, and its notes must say so.
- The installer needs OpenSSL 3 to check signatures, and only warns without it.
- The agent gains an HTTP client to download releases.
