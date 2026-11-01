# ADR-0074: Snapshot retention, and using a mirror without a campaign

Date: 2026-11-01
Status: accepted; amends ADR-0030, ADR-0035 and ADR-0067

## Context

ADR-0067 left two gaps. A daily sync adds a daily snapshot, and packages removed upstream stay on the disk as long as one snapshot refers to them, so a mirror grows without limit. And a campaign was the only way to use a mirror from the interface: a server could not simply be pointed at one and left there.

## Decision

### Retention

Each mirror keeps its newest 7 published snapshots, a week of daily syncs. There is no setting: the count is a constant, `mirror::retention::KEEP_SNAPSHOTS`, because nobody can pick a better number than the one the page states, and what must not go is kept whatever the number. The relay's own `mirror.keep_snapshots` (ADR-0034) is unchanged: a relay has its own disk.

On top of the 7, these stay however old they are:

- the current snapshot: the newest published, and what the `current` link names, which a crash between moving the link and recording the snapshot can leave on a snapshot that is not published;
- snapshots pinned by a campaign that has not finished, drafts included;
- snapshots a server was pointed at by name: its source commands not ended, the last one that succeeded, and the last one that succeeded outside campaigns, which a campaign sends again to put a server back (ADR-0038). A failure after those changes nothing, as it may have left the sources alone. `current` is a moving name, so it holds no snapshot. Removed servers do not count;
- the `current` a relay reports, while the relay is not removed. Agents behind it are served from its own copy, but the server stays able to name what its site runs, for one snapshot per relay and mirror.

A published snapshot in neither list goes. So does a failed one, which only holds aptly objects and packages, unless `current` names it. A snapshot named by hand is treated like any other: one rule the page can state in a sentence, and no snapshot can be named in the interface.

Doubt keeps a snapshot: a source command that cannot be decoded stops the pass.

### Freeing the disk

Removing a published snapshot frees little on its own. For apt, `aptly publish drop` and `aptly snapshot drop` remove the metadata and the hardlinks, and only `aptly db cleanup` deletes the pool files nothing refers to any more. For dnf the snapshot directory is removed, and a package goes with its last hardlink, since `reposync --delete` already dropped it from the working tree.

`aptly db cleanup` deletes every pool file no mirror or snapshot refers to, which includes what a running `aptly mirror update` of any mirror has downloaded and not recorded yet. So a pass needs every mirror still. Every job now also holds a shared advisory lock, `monitorflow.mirrors`, on the connection of its mirror lock, and a pass takes it exclusively with a waiting `pg_advisory_lock`. Postgres queues later shared requests behind a waiting exclusive one, so jobs started meanwhile are refused like any busy job, the scheduler tries them again a minute later, and the wait is bounded by the jobs running at that moment. It needs no state in the scheduler, and it covers syncs started from the CLI in another process.

A pass runs every hour and takes the lock only when there is something to remove. In a transaction it locks the published rows, then reads what is in use, then marks what goes `pruning`. A campaign being created takes a share lock on the snapshots it pins, so either it commits first and is seen, or it finds them no longer published. A command naming a snapshot by hand has no such lock: at worst it reaches a snapshot just removed, its post-check fails and the server goes back to its sources (ADR-0032).

`pruning` is recorded before anything is deleted, `pruned` once aptly no longer lists the snapshot and the pool is clean. A pass that stops halfway is finished by the next one: aptly's drop commands fail on what is already gone, so success is read from `aptly snapshot list` and `aptly publish list`, not from their exit codes. The error of a failed pass is stored on the rows and shown on the mirror's page.

Rows stay, as runs and finished campaigns refer to them, and `pruned` ones leave the listings. A removal is audited as `mirror.prune`, unlike scheduled syncs, because it cannot be undone. `monitorflow-server mirror prune` runs a pass at once; it is what the smoke test uses, with real aptly and dnf tools, on an upstream that replaced its package.

Rejected:

- **Taking every mirror's lock.** One connection per mirror of a pool of 16, and any running job would mean starting over.
- **A setting on the Settings page.** It would offer a choice whose safe value is the default, and a wrong value only costs disk or history.
- **Keeping by age.** A mirror synced weekly would drop to one snapshot; a count works at every interval.

### Using a mirror

A server's Updates tab and the site page say where its packages come from and offer the way there and back. No mirror is chosen: the agent reports `debian 12`, `ubuntu 24.04` or `rocky 9.4`, which names a distribution of the catalog (ADR-0067), and that means all of its mirrors at `current`, the snapshot-following name, so a server stays up to date without further clicks.

The way there is built from what campaigns use: the same spec, the same choice of the relay's copy when it holds everything (ADR-0037), signed commands in the same audit. The way back is `revert_sources`, which puts the exact originals back (ADR-0032). The state comes from the server's source commands: the last that succeeded says where it stands, a command not ended says it is moving, and a later failure is shown with its error. These commands live a day rather than an hour, so a server offline for a night still takes them.

The mode is always `replace`. A mirror next to the Internet sources is not really used, and a site without Internet fails its post-check as soon as an original source is unreachable. Third-party repositories are set aside with the rest and the confirmation says so; a server that needs them stays on its own sources, or `add` remains in the API and in campaigns.

`POST /api/sites/{id}/sources/use-mirror` and `DELETE /api/sites/{id}/sources` handle each server alone and answer what was sent and what was skipped, with the reason. There are no batches as in a campaign: the post-check undoes a change that does not work on that server, and nothing is installed.

A server whose distribution the mirrors do not carry keeps its sources, and its page says so in a sentence: not in the catalog ("MonitorFlow has no mirror for debian 11."), in the catalog but not added, with a link to the Mirrors page, or still being copied. While a campaign holds a server's sources no change is offered, as its restore would undo it.

A mirror added with `mirror add` under another name is not matched: a name says nothing about a distribution. Such mirrors stay reachable through campaigns and `POST /api/servers/{id}/sources`.

## Consequences

- Disk use of a mirror levels off at about 7 days of change, plus what is in use. The free space shown when adding a mirror stays useful, as ADR-0067 says, but no longer has to be watched.
- A pass delays syncs until it ends, and "Sync now" says the mirror is busy while one waits or runs. `aptly db cleanup` on a large pool takes minutes.
- A snapshot older than a week cannot be returned to unless something holds it. Pinning it in a campaign is what holds it.
- A relay offline for good holds one snapshot per mirror until it is removed.
- The catalog is amd64 only and agents do not report their architecture, so an arm64 server is offered the mirror: the post-check fails, the server goes back to its sources, and the error says why.
