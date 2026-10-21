# ADR-0030: Mirror orchestration and serving

Date: 2026-10-23
Status: accepted

## Context

Architecture §7: the central server hosts full apt and dnf mirrors, with snapshots so an update campaign can pin an exact repository state, built with `aptly` for apt and `reposync` plus `createrepo_c` for dnf rather than our own implementation of the formats. Relays will sync a copy (next task) and agents will point their sources at a mirror (ADR-0032), so the URL layout must be fixed now. A mirror sync takes minutes to hours and fails for reasons outside our control (upstream down, disk full, bad signature); it must not take the server down, and two syncs of one mirror must never run at once, including a sync started from the CLI while the server runs one.

## Decision

**Mirrors are global**, not per customer. A mirror is a copy of public upstream content (Debian, Rocky…), the same for every tenant; one copy per customer would multiply tens of gigabytes for nothing, and mirror content carries no customer data. Which mirror a site's servers use is a per-site choice for the source configuration task. Managing mirrors needs a **global administrator** role: a customer administrator must not change what every other customer installs.

**Schema** (migration `20261023090000_mirrors`):

- `mirrors (id, name UNIQUE, family apt|dnf, upstream_url, distribution, components[], architectures[], sync_interval_s, auto_snapshot, created_at)`. `name` is 1 to 64 of `[a-z0-9._-]` starting with a letter or digit: it is a URL segment and part of aptly object names. `distribution` is the apt suite (`bookworm`, `noble-updates`), empty for dnf; `components` at least one for apt, none for dnf. `architectures` empty means the upstream's (apt) or every one (dnf). `sync_interval_s` NULL is manual only, else at least 900. `upstream_url` is http or https **without userinfo**: credentials would land in this table, in the logs and in aptly's database. Authenticated upstreams (RHEL's CDN with client certificates) are a follow-up.
- `mirror_snapshots (id, mirror_id, name, state creating|published|failed, size_bytes, error, created_at, published_at)`, `UNIQUE (mirror_id, name)`. `name` is 1 to 64 of `[A-Za-z0-9._-]`, not starting with a dot and not `current`; it defaults to the UTC time `YYYYMMDDTHHMMSSZ`. `size_bytes` is the apparent size of the published tree, counted while walking it anyway; files shared by hardlinks count in every snapshot.
- `mirror_runs (id, mirror_id, kind sync|snapshot, trigger schedule|manual, requested_by, state running|succeeded|failed, snapshot_id, output, error, started_at, finished_at)`. One table for both kinds, as both run tools under the same lock and are what an administrator looks at when something fails. `output` is the commands and their combined stdout and stderr, at most the last 64 KiB; `error` one line. A partial unique index allows one `running` run per mirror.

Mirrors are not deleted for now: removing published content that agents or a campaign still use needs the campaign model first.

**One job per mirror at a time**, across processes. A run takes a PostgreSQL session advisory lock on the mirror (`pg_try_advisory_lock`) on a connection it holds until it ends; a busy mirror answers "busy" at once (409 in the API) rather than queueing. A process that dies releases its lock with its connection, so a `running` row found by the next holder of the lock is stale and is marked `failed` ("interrupted") before the new run starts. Snapshots take the same lock as syncs: a dnf snapshot hardlinks the working tree, which must not change under it.

**Runner trait.** The `mirror` module runs every tool through `Runner::run(Invocation)`: program, arguments (never a shell), working directory, a timeout (`mirror.sync_timeout`, default 12 h, per command), output captured with the 64 KiB tail. `CommandRunner` is the real one on `tokio::process::Command` with `kill_on_drop`, a cleared environment (`PATH`, `HOME`, `LC_ALL=C`, `GNUPGHOME`, and the `http_proxy` family passed through) and the tool paths from `[mirror]` in the configuration. Tests point those paths at shell scripts that behave like the tools; CI never needs the real ones.

**apt with aptly.** The server owns an aptly root `<mirror_dir>/aptly` through a generated `-config` file. Each component is its own aptly mirror (`mf:<name>:<component>`, and `mf:<name>:<component>:<snapshot>` for its snapshots; `:` appears in neither names nor components), because an aptly snapshot of a multi-component mirror merges the components into one. A sync is `aptly mirror create` the first time, then `aptly mirror update` per component, verifying the upstream `Release` against `<mirror_dir>/upstream-keyring.gpg`, which the administrator fills with the distribution's archive keys (documented in §7). A snapshot is `aptly snapshot create` per component, then one `aptly publish snapshot -component=c1,c2,… -distribution=<suite>` to the prefix `<name>/<snapshot>`, signed with the server's key. aptly's own pool hardlinks make published snapshots cheap.

**dnf with reposync and createrepo_c.** A sync is `reposync --repofrompath --delete --download-metadata --norepopath` into `<mirror_dir>/dnf-work/<name>` (not served), then `createrepo_c --update` there, so the working tree is always a valid repository. A snapshot is a **hardlinked** copy of the working tree in `<mirror_dir>/dnf/<name>/.creating-<snapshot>`, renamed into place when complete: a full mirror is tens of gigabytes, so a copy per snapshot multiplies the disk use, while package files are named by name, version, release and architecture and never change upstream. `repodata/` is copied rather than linked (it is small, and `createrepo_c` replaces it on the next sync). The linked package files are made read-only, so a tool that rewrote one in place would fail instead of changing a published snapshot; `reposync --delete` only unlinks. Across file systems the link fails with `EXDEV` and the file is copied. `repodata/repomd.xml` of the snapshot is signed by the server key (`repomd.xml.asc`), so clients can turn on `repo_gpgcheck` in addition to the vendor signatures on each package.

**`current`** is a symlink next to the snapshots (`<name>/current` → `<snapshot>`), swapped atomically (new link, then `rename`) after a snapshot is published; it always names the newest published one. A sync with `auto_snapshot` (the default) ends with a snapshot, so scheduled syncs move `current`; turning it off leaves `current` to explicit snapshots.

**Signing key.** One OpenPGP key in `<mirror_dir>/gnupg` (mode 0700), created by `monitorflow-server mirror key generate` (RSA 4096, sign only, no passphrase since publishing is unattended; for wider apt and rpm compatibility than Ed25519) or imported with `mirror key import --file`. Both refuse when a secret key exists already; rotation is a follow-up. The public key is exported to `<mirror_dir>/keys/monitorflow-archive.asc`. The private key never leaves that directory and is never logged.

**Serving.** A plain HTTP listener, `mirror_listen_addr` (default `0.0.0.0:7480`), GET and HEAD only, serves files from `mirror_dir` with this fixed layout:

| URL | Content |
| --- | --- |
| `/apt/<mirror>/<snapshot>/` | aptly publish of the snapshot (`dists/`, `pool/`) |
| `/apt/<mirror>/current/` | the newest published snapshot |
| `/dnf/<mirror>/<snapshot>/` | the snapshot repository (`repodata/`, packages) |
| `/dnf/<mirror>/current/` | the newest published snapshot |
| `/keys/monitorflow-archive.asc` | the public signing key |

No directory listings, and any path segment starting with a dot is 404, which hides snapshots being built. This is the one plaintext listener of the project, an exception to "every link is mTLS": apt and dnf clients authenticate content by signatures (apt `Release` files signed by our key, dnf packages by the vendor and `repomd.xml` by our key), not by the transport; package managers on every server would otherwise need our CA or client certificates, and relays and caching proxies serve the same bytes. The content is public. What TLS would add, confidentiality of which packages a server downloads, is not worth that for now; the listener can move behind a TLS proxy without changes. The key at `/keys/` is a convenience for fetching it: whoever installs it on a host must check it against something authenticated (its fingerprint from the API, or a copy sent over the agent's mTLS stream; the source configuration task decides), never trust this URL alone.

**Scheduling.** A task in the server wakes every 60 s and starts a sync of each mirror whose `sync_interval_s` has passed since its last sync run started, at most `mirror.max_concurrent_syncs` (default 2) at a time. A failure is recorded in the run and logged; the next one is tried at the next interval.

**Interfaces.** CLI `monitorflow-server mirror add|list|sync|snapshot|key`: `sync` and `snapshot` run in the CLI process under the same lock and print the run's output. REST `GET /api/mirrors`, `GET /api/mirrors/{id}` (with snapshots and the last runs), `POST /api/mirrors/{id}/sync` and `POST /api/mirrors/{id}/snapshots` (`{"name"?}`), which answer 202 with the run, started in the server. Runs are audited (`mirror.create`, `mirror.sync`, `mirror.snapshot`).

## Consequences

- An install with mirrors needs `aptly`, `dnf-plugins-core` (`reposync`), `createrepo_c` and `gpg` on the server host, and the disk for full mirrors; the server image gains them in packaging.
- Snapshots accumulate: retention, and deleting snapshots or mirrors, wait for campaigns, which know which snapshots are in use.
- The plain listener exposes which packages a host downloads to the network path; a TLS proxy in front fixes that when it matters.
- Tool invocations are tested against fake binaries only: the command lines follow the tools' documentation and need a check against real `aptly` and `reposync` before the first release.
- Relays sync from the same URLs, so the layout above cannot change without a new ADR.
