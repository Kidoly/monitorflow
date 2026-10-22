# ADR-0034: Relay mirror sync

Date: 2026-10-24
Status: accepted

## Context

Architecture §7: relays sit in sites with little or no internet access and serve a local copy of the package mirrors, so the agents behind them install packages from the relay. The server publishes its mirrors over plain HTTP with the fixed layout of ADR-0030 (`/apt/<mirror>/<snapshot|current>/`, `/dnf/<mirror>/<snapshot|current>/`, `/keys/monitorflow-archive.asc`), and integrity comes from the server's GPG signatures on apt `Release` files and dnf `repomd.xml`. A chained relay (ADR-0028) may not reach the server at all, only its parent relay.

Questions: how a relay learns what to copy, over which transport, how it copies a tree of tens of gigabytes over a slow link without ever serving a half-copied one, and how it serves the result.

## Decision

**The upstream publishes an index, `/index.json`**, on the mirror listener of the server and of every relay. It is built on each request from the served tree, not from the database: for each family directory, every mirror directory with a valid name whose `current` symlink names an existing snapshot.

```json
{"version": 1, "mirrors": [
  {"family": "apt", "name": "debian", "current": "20261023T090000Z", "distributions": ["bookworm"]},
  {"family": "dnf", "name": "rocky9", "current": "20261023T091500Z", "distributions": []}
]}
```

`distributions` lists the directories under `dists/` of the apt snapshot, since HTTP gives no directory listings and the relay must know where the `Release` files are. Built from the tree, the index of a relay describes exactly what that relay serves, so a downstream relay uses the same code against its parent, and the server change is limited to its listener. A list of mirrors in the relay configuration was rejected as the source: the administrator would repeat on every relay what the server already knows, and would still need the current snapshot name from somewhere. The relay configuration can narrow the index (`mirror.mirrors`, empty means all).

The index is not signed. A forged index can only point the relay at other paths of the same upstream or make it skip mirrors; it cannot change the content checks below.

**Transport: plain HTTP to the upstream's mirror listener**, `mirror.upstream_url`, by default `http://<host of upstream_url>:7480`, which for a chained relay is its parent. Not the mTLS gRPC channel, because:

- the content is public and authenticated by signatures that the agents check (ADR-0030); the transport adds nothing to integrity;
- bulk transfers of gigabytes would share the gRPC connection and the upstream's tasks with monitoring streams, and would need a new file-transfer RPC with its own flow control and resumption, where HTTP already has them;
- the same URLs work through a caching HTTP proxy at the site boundary, and a relay serves the same layout to its children with the same code.

What a network attacker gains, as for agents (ADR-0030): seeing which packages a site downloads, and, by altering content, making the relay reject a sync (an availability attack it could mount by dropping packets anyway). The listener can sit behind a TLS proxy; `mirror.upstream_url` may then be https.

**What a sync copies.** For each mirror of the index, the snapshot that `current` names. Snapshot names are unique per mirror and published snapshots never change (ADR-0030), so a snapshot already complete on the relay is not fetched again; only `current` may move.

**Checks.** The relay does not verify GPG signatures: it has no OpenPGP implementation, and the key at `/keys/` comes over the same channel, so checking against it would prove nothing; agents check signatures against the key they received with a pinned fingerprint (ADR-0032). The relay copies the signatures unchanged and checks every file against the checksums of the signed metadata, rooted at the files the signature covers:

- apt: `dists/<suite>/Release` gives the size and SHA-256 of every index file under `dists/<suite>/`; each `Packages` file listed (plain, `.gz`, `.xz`, `.bz2` or `.zst`) gives the `Filename`, `Size` and `SHA256` of every package under `pool/`. `InRelease` and `Release.gpg` are copied as they are. A file listed in `Release` that the upstream does not have (a compression it did not generate) is skipped, as apt itself does. Every `Packages` variant the upstream serves is copied and read, so each package a published list names is fetched; a missing package is an error.
- dnf: `repodata/repomd.xml` gives the size and checksum (SHA-256 or SHA-512) of every metadata file; the `primary` metadata (`.xml`, `.gz`, `.xz`, `.bz2` or `.zst`, read with pure-Rust decoders) gives every package's `location`, size and checksum. `repomd.xml.asc` is copied as it is.

Paths from the metadata must be relative, without `..`, empty or dot-prefixed segments, or backslashes; any other path fails the sync. A download that grows past the expected size is cut off.

**Staging and swap.** A snapshot is built in `<mirror_dir>/<family>/<mirror>/.sync-<snapshot>/`, hidden from the listener by its dot. Each file is downloaded to `<path>.part`, checked, then renamed. Once every file is in place the relay writes `.manifest` (path, size and SHA-256 of each file), renames the staging directory to `<snapshot>`, and swaps `current` atomically with a new symlink and a `rename`, like the server (`common::mirror::swap_current`). Any failure leaves `current` and the published snapshots untouched.

**Incremental and resumable.** Before downloading a file the relay looks for it, in this order: in the staging directory (left by an interrupted sync; re-hashed, kept if it matches), then at the same path in the manifest of a complete local snapshot of the same mirror with the same size and checksum (hardlinked, files of complete snapshots are read-only). Consecutive snapshots share almost all their packages, so a new snapshot costs its changed files. Resumption is per file; a `.part` file is restarted.

**Bounded work.** One mirror is synced at a time, with at most `mirror.max_downloads` (default 4) files in flight; each request has a timeout (`mirror.request_timeout`, default 600 s, without progress). The loop runs at start and then every `mirror.sync_interval` (default 900 s). A failure is logged with the mirror, the error and the time of the last success, and the next cycle tries again; it never stops the relay. Each success is logged with the snapshot, files fetched, linked and kept, bytes and duration. The public key at `/keys/monitorflow-archive.asc` is copied on every cycle.

**Retention.** After a successful swap the relay keeps the `mirror.keep_snapshots` (default 3, at least 1) most recently completed snapshots of the mirror, always including the one `current` names, and removes the older ones and staging directories of other snapshots. Mirrors that leave the index are kept, since agents may still use them; removing them is a follow-up with campaign-aware retention.

**Serving.** The relay runs a plain HTTP listener on `mirror_listen_addr` (default `0.0.0.0:7480`) over `mirror_dir` (default `<data_dir>/mirrors`), laid out as `apt/`, `dnf/` and `keys/`. The router of the server moves to `monitorflow_common::mirror` (feature `mirror`), so both serve the identical layout and `/index.json`, with the same rules: GET and HEAD only, no listings, any dot segment is 404. Setting `mirror.enabled = false` turns off both the sync and the listener.

## Consequences

- Agents behind a relay point at `http://<relay>:7480/apt/<mirror>/current/` (or a pinned snapshot it holds), with the same keys as for the server.
- A relay holds `keep_snapshots` snapshots of every mirror the server publishes unless narrowed; disk use is that of one snapshot plus the changed files of the others, thanks to hardlinks.
- A campaign pinned to a snapshot the relay never saw as `current` finds nothing on the relay. The index could list pinned snapshots once campaigns exist; until then, keep `current` on the pinned snapshot during a campaign.
- Not covered yet: dnf group and module metadata beyond what `repomd.xml` lists (copied like any listed file, but not parsed), apt `by-hash` paths, range requests to resume a single large file, and signature checks on the relay with a key pinned over mTLS.
- The status of the sync is in the relay's logs only; reporting it to the server through the relay's own stream is a follow-up.
