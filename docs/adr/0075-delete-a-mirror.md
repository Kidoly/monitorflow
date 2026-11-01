# ADR-0075: Deleting a mirror

Date: 2026-11-01
Status: accepted; amends ADR-0030, ADR-0034, ADR-0038 and ADR-0074

## Context

ADR-0030 left mirrors undeletable until campaigns existed, and ADR-0074 kept it that way. A distribution added by mistake holds hundreds of GiB on the server and on every mirroring relay, and the only way out was the database and a shell.

Deleting must not pull packages out from under anything. It also cannot be one quick transaction: freeing the disk needs `aptly db cleanup`, which needs every mirror still (ADR-0074), so it may wait hours for a sync, and the mirror most worth deleting is the one on its first sync of 100 GiB.

## Decision

A global administrator, like every mirror route, removes a mirror from its page. `DELETE /api/mirrors/{id}` answers 202 with the mirror, or 409 saying what uses it. `monitorflow-server mirror delete --name` does the same for the CLI and waits for the end. The screen word is "Remove", as for servers and relays.

### What uses a mirror

| User | Handling | Why |
| --- | --- | --- |
| A campaign not finished, drafts and paused ones included, pinned to it | Refused, naming the campaign | Its next wave needs those packages. Stopping it is the administrator's call. |
| A server pointed at it, at any snapshot, `current` included | Refused, naming the server | Its `apt-get update` would fail. Moving it back rewrites its sources, which a person decides; retention ignores `current`, as it holds no snapshot, but a mirror's `current` goes with it. |
| A server with a source command not ended, or that a campaign would put back on it (`pin::previous`) | Refused, the same | The same three commands per server as ADR-0074. A failure after them changes nothing. Removed servers do not count. |
| A finished campaign | Not a user | Its history stays, see Rows. |
| A running sync or snapshot of the mirror | Stopped | Refusing would leave no way to cancel a first sync, and its result is about to be deleted. |
| A relay holding a copy | Handled by itself | A copy is not a use: what uses it is a server behind the relay, which counts above. The relay deletes it, see Relays. |

Nothing is reverted automatically. Sending commands to servers as a side effect of deleting something else is too much for one click, and the way back is already one click on each server or site.

The refusal says what uses the mirror, and `GET /api/mirrors/{id}` returns the same as `used_by`, so the page shows the list with links instead of a button that fails. A source command that cannot be decoded refuses, as doubt keeps a snapshot in ADR-0074. A server that was switched off for good holds the mirror until it is removed, which is what its page offers.

A server changed by hand is not known here; that was already so for retention.

### One transaction, then the pass

The request takes the row locks in the order of retention, reads what is in use, then in one transaction sets `mirrors.deleting_at`, turns the published snapshots `pruning`, and audits `mirror.delete`. A campaign being created holds a share lock on the snapshots it pins, so it either commits first and is seen, or finds them no longer published. From then on the mirror cannot be chosen: `store::by_name`, the catalog check, campaigns and the scheduler all skip it, and `Mirrors::begin` refuses after taking the lock. A command naming it by hand in that instant has the fallback of ADR-0074: its post-check fails and the server goes back.

The rest is `removal::finish`, at the end of each pass of retention, under its exclusive lock, which keeps everything about jobs and `aptly db cleanup` in one place. The API starts a pass at once, and the hourly pass picks up what a restart interrupted. Each step can be repeated, so a failure stops nothing: it lands in `mirrors.delete_error`, the page shows it with "Try again now", and the next pass starts over. Rows go last.

1. The name goes in `.removed`.
2. For apt, everything aptly lists under the mirror's prefix goes: publications, snapshots, aptly mirrors. Listing rather than walking our rows also catches what a failed job left. Drop commands fail on what is gone, so success is read from the lists afterwards, as in ADR-0074.
3. The served directory is renamed to `.removing-<name>`, which the listener hides, then deleted. A half-deleted tree is never served.
4. For dnf, the working tree and the mirror's part of dnf's cache go. The cache is matched exactly: `rocky-9` must not take `rocky-9-baseos`.
5. `aptly db cleanup` frees the pool, once, after every apt mirror of the pass.
6. Runs, snapshots and the mirror go in one transaction, audited `mirror.deleted`.

Two audit entries, as they are different facts: who asked, and that it happened, which `mirror.prune` already records for a removal that cannot be undone.

### A running job

A job looks at `deleting_at` while each tool runs, every 5 seconds, from whichever process runs it, so a sync started from the CLI stops too. Dropping the tool's future kills it, here or through the sidecar's socket (ADR-0067). The check is around tools and not the whole job, because a `spawn_blocking` step cannot be stopped and would go on writing into a tree being deleted; those are short, and the lock waits for them. The job records its run as failed like any other and releases its lock.

### Rows

A finished campaign still shows what it pinned. `campaign_snapshots` gains `mirror_name` and `snapshot_name`, its key becomes `(campaign_id, mirror_name)` and its references `ON DELETE SET NULL`, and the views read the names. Keeping `mirrors` rows with a flag instead would leave every query filtering them and block adding the name again. The audit log is untouched, and old source commands keep the names in their payloads.

### Relays

Relays learned of mirrors by what `/index.json` lists. Treating a mirror that is no longer listed as deleted is the obvious design and a dangerous one: backups leave repositories out (ADR-0058), so a restored server lists nothing, and a chained parent narrowed by `mirror.mirrors` lists less, and either would wipe the copies its sites install from. A relay also cannot tell a mirror deleted from one between two states of its `current`.

So deletion is said explicitly. `.removed`, one mirror name per line in the family's served directory and hidden by its dot, is the other half of `.pinned`. The index gains `removed: [{"family", "name"}]`, omitted when empty, so the format stays version 1 and older relays ignore it. It is built from the tree like the rest, and a name that is published is left out: adding the mirror back overrides it with no step, and it cannot be listed while `current` still serves it. The entries stay, as a relay may be offline for months, and cost a line each.

At the start of a cycle a relay writes the list to its own `.removed`, which is how chained relays hear of it, then for each name renames `<family>/<name>` to a hidden directory and deletes it, staging included, and sweeps what a crash left. It applies whatever `mirror.mirrors` narrows to: the origin deleted it. It then reports what it serves.

The cycle is one task and copies one mirror at a time, so no sync of the relay's own is running at that moment. A cycle that read the index before the deletion fails on the first 404, or finishes on files already fetched; either way the next cycle removes the result, and a sync cannot recreate a mirror the index it read says is gone.

Rejected:

- **Refusing while a relay reports a copy.** A relay offline for good would block deleting, and copies are the thing being cleaned up.
- **A command to relays on their stream.** A relay offline misses it unless the server queues it for every relay and chain, while the index is already read by each of them, and is how `pinned` travels.
- **Refusing during a sync.** Simple, but the administrator waits hours to cancel a mistake.
- **Reverting servers automatically.** See above.
- **Hiding the mirror from the index first.** The relays would have to delete before the server does; nothing gains from that order.

## Consequences

- A removal cannot be undone. Adding the mirror again downloads everything again; the confirmation says so.
- The disk comes back when the pass ends, which waits for jobs of other mirrors, as in ADR-0074. The page says it is being removed until then, and holds new jobs back like any pass.
- A relay frees its disk at its next cycle, 15 minutes by default, or when it comes back. The page says "at their next sync"; nothing waits for relays.
- A relay older than this ADR keeps its copy until it updates itself, then deletes it, as the entry stays.
- `/index.json` is plain HTTP (ADR-0034), so whoever can rewrite it can make a relay delete a copy. That is an availability risk of the same size as a forged `current`, which retention then prunes after: a relay fetches the mirror again, and packages stay checked by agents. `removed` names pass the same path checks as every other name.
- A server restored from a backup taken before a deletion has lost `.removed`, and holds the mirror's row without its files. Relays keep their copies until it is removed again, which works, as every step tolerates what is already gone.
- `relay_mirrors` keeps naming a deleted mirror until the relay's next report. Nothing reads it for a mirror that is gone, and a mirror added back under the name may be served by a relay's old copy for one cycle. Packages are signed by the same key.
- The real tools are covered by the compose smoke test; unit and integration tests use the fake tools of ADR-0074.
