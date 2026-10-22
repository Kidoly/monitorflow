# ADR-0035: Source configuration commands

Date: 2026-10-24
Status: accepted

## Context

ADR-0032 built the agent side of package source configuration: `ConfigureSources` and `RevertSources` in `sources.proto`, the library that applies and reverts them with a backup, and the root helper `monitorflow-agent sources apply|revert`. ADR-0031 built signed commands, the agent's handler registry and the `apply_updates` handler running apt and dnf through `sudo -n`. Nothing yet sends a spec to an agent, builds one from the server's mirrors (ADR-0030), or installs the sudoers rules both ADRs rely on. The spec needs a URL the agent's host reaches the mirrors at, which is not the server's listen address, and will differ for agents behind a relay once relays serve a copy of the mirrors (ADR-0034).

## Decision

**Kinds.** Two command kinds on the existing channel, with the existing messages as payloads and no protocol change: `configure_sources` (`ConfigureSources`) and `revert_sources` (`RevertSources`, empty). They are queued, signed, dispatched one at a time per server and journaled like `apply_updates`.

**API.** `POST /api/servers/{id}/sources` with `{"mirrors": [{"name", "snapshot"?}], "mode": "replace"|"add", "ttl_s"?}` queues `configure_sources` and answers 201 with the command; `snapshot` defaults to `current`. `DELETE /api/servers/{id}/sources` queues `revert_sources` and answers 202 with the command. Both need an administrator of the server's customer, as for updates (ADR-0031), and are audited as `command.create` with the parameters: mirrors, snapshots, mode, base URL and key fingerprints, never the armored key. `mode` has no default, as `replace` sets every other source of the host aside. Results are read with the existing command routes.

**Spec construction.** The server builds the spec from the `mirrors` table: every named mirror must exist and be of one family, the family of the server's OS (`apt` for `debian`, `dnf` for `rhel`), named once, at most 32. A named snapshot must be `published`; `current` needs at least one published snapshot, so the URL resolves when the agent applies. apt repositories take the mirror's suite and components and no architectures, so apt uses the host's own and a host whose architecture was not mirrored fails the post-check instead of silently getting no packages. dnf repositories use the mirror name as repo ID (`monitorflow-<mirror>` on the host, ADR-0032) with `gpgcheck` on. The spec carries one key, `archive`: the public key from `<mirror_dir>/keys/monitorflow-archive.asc` (exported first if missing) with the fingerprint gpg reports for the one secret key in the mirror GNUPGHOME, so the agent installs the key only if it is the one that signs the mirrors. No signing key, no mirror runner in the server, or no URL answer 409.

**URL.** A new server setting, `mirror_public_url` (like `http://mirror.example.net:7480`, http or https, no userinfo, query or fragment), is the base URL written into agents' sources; it is what the agents' hosts reach `mirror_listen_addr` at, which only the operator knows (NAT, proxies, DNS). Unset, source configuration is refused rather than guessed from `server_names`, which name the gRPC endpoint.

**Relay hook.** The URL is chosen by one function, `commands::sources::base_url(public_url, site)`, which returns `mirror_public_url` for every site today. When the relay mirror sync lands, a per-site or per-relay mirror URL (for instance a nullable `mirror_url` on `relays`, chosen for the agent's site) is looked up there first, so agents behind a relay use its copy with no change to the API or the protocol. Relay serving itself is not part of this decision.

**Agent.** Handlers for both kinds run `sudo -n /usr/bin/monitorflow-agent sources apply` with the encoded spec on stdin, or `sudo -n /usr/bin/monitorflow-agent sources revert`, through the package exec helpers. The handler decodes and checks the payload's shape before running anything (a payload that is not a `ConfigureSources` is rejected); the full checks, keys included, stay in the helper (ADR-0032). The command succeeds when the helper exits 0; otherwise it fails with the helper's last stderr line as the error, and its output, at most the last 64 KiB, as the command's output.

**Deploy.** `deploy/sudoers/monitorflow-agent` holds the exact command lines of ADR-0031 and ADR-0032 for the user `monitorflow`, without wildcards, so only full upgrades work through it; an administrator who wants `--only-upgrade <packages>` adds that rule knowingly. `scripts/check.sh` validates the file with `visudo -cf` when `visudo` is installed. Installation (binary at `/usr/bin/monitorflow-agent`, the `monitorflow` user, the sudoers file) is documented in `docs/deploy-agent.md`.

## Consequences

- The armored public key travels in every `configure_sources` payload (a few KiB), and relays see it in clear; it is public.
- A server whose mirror family does not match its OS cannot be configured; mixing apt and dnf on one host is not supported.
- `current` is resolved by the host at each `apt-get update`, not when the command is queued: a campaign that needs a fixed state names a snapshot.
- Rotating the signing key needs a new `configure_sources` to every server.
- Agents behind relays fetch from `mirror_public_url` until the relay hook is filled, so those hosts need a route to it.
