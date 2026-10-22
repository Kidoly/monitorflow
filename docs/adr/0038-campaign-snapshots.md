# ADR-0038: Campaigns pinned to mirror snapshots

Date: 2026-10-25
Status: accepted

## Context

Architecture §7 has an administrator pick a repository snapshot for an update campaign, so that the pilot and every later wave install the same package versions even if the mirrors sync in between. ADR-0033 left this as a follow-up: campaigns send `apply_updates` against whatever sources a host has. ADR-0035 gave the server `configure_sources` and `revert_sources` commands built from the mirrors table. ADR-0034 relays copy only the snapshot `current` names, so a snapshot pinned for a week would often be missing on the relay agents fetch from.

Questions: what the campaign stores, how the runner sequences the source commands around the update while keeping one command in flight per server (ADR-0031), what "restore" means for a host already pointed at the mirrors, and how relays learn what to keep.

## Decision

**Model.** Migration `20261025090000_campaign_snapshots`, add-only: `campaigns.sources_mode` (`replace` or `add`, NULL when nothing is pinned), `campaign_snapshots (campaign_id, mirror_id, snapshot_id)` referencing `mirror_snapshots`, and two columns on `campaign_targets`: `sources_command_id` and `restore_command_id`. `command_id` still names the update, so ADR-0033's progress and statuses keep their meaning.

**API.** `POST /api/campaigns` takes an optional `"sources": {"mode": "replace", "mirrors": [{"name": "debian-12", "snapshot": "20261023T090000Z"}]}`; the campaign views return the same object, or `null`. Several mirrors may be pinned, of one or both families: each server uses the pinned mirrors of its OS family, as one `configure_sources` spec. At creation every mirror must exist with a `published` snapshot of that name (`current` is refused: the host resolves it at each `apt-get update`, which is what pinning avoids), a mirror is named once, at most 32, and every OS family among the targets needs a pinned mirror (400 otherwise). Without `mirror_public_url`, mirrors or a signing key the answer is 409, as for `POST /api/servers/{id}/sources`. `mode` has no default, for ADR-0035's reason. Each server in CampaignDetail gains `sources_command_id` and `restore_command_id`; its `status` and the `failed` count include a restore that failed after the update, with `error` saying so. The audit of `campaign.create` records `sources`.

**Sequence per server.** Three campaign commands, each queued only after the previous one ended, so ADR-0031's one-in-flight rule needs nothing new:

1. `configure_sources` for the pinned snapshots, with the base URL from `commands::sources::base_url`: the mirror URL of the server's relay when it holds every pinned snapshot (ADR-0037), else `mirror_public_url`, queued when the window is open, with the update's TTL rules;
2. once it succeeded, `apply_updates`, inside the window as before;
3. a restore, once the update ended whatever its result, or once it will not come now: the configuration failed, the campaign is paused, stopped or failed, or the window closed before the update could be queued.

A configuration that fails, or cannot be built, sets the target's `error`, so it counts as failed and gets no update. A server is done for its wave only once its restore ended, and failed if that restore failed after its update. A server restored before its update (pause, closed window) gets a new `configure_sources` when the campaign queues again.

**Restore is "as before the campaign".** ADR-0032 keeps one backup across applies of the same family and mode, so `revert_sources` after pinning a host that was already pointed at the mirrors would remove the MonitorFlow sources altogether. The restore therefore sends again the server's last `configure_sources` that succeeded outside campaigns (same payload, newly signed when dispatched), and `revert_sources` when that was a revert or there was none. Restores are queued by the runner whatever the campaign's state (it also looks at final and paused campaigns that still have servers to restore), are never cancelled by pause, stop or failure, live 7 days (the longest command TTL) since putting a host back is right even late, and are audited as `campaign.restore_sources`. Revoked servers get none.

**Relays.** `/index.json` entries gain `pinned`: `[{"name", "distributions"}]`, the snapshots other than `current` to hold, omitted when empty, so the format stays version 1 and older relays ignore it. The index is still built from the tree: each listener reads `.pinned` (one snapshot name per line, hidden from HTTP by its dot) in the mirror's directory and lists the named snapshots that exist. On the server the campaign runner rewrites these files every tick from the snapshots of campaigns not yet final (drafts included, so relays copy ahead of the window). A relay copies each pinned snapshot with the same checks, staging, size bound and hardlinks as `current`, without moving `current`, writes the list it holds into its own `.pinned` for its children, and its retention keeps pinned snapshots in addition to `keep_snapshots`. Once the campaign ends the snapshot leaves the index and is pruned at the next successful sync. Listing every retained snapshot was rejected: relays would copy far more than any campaign needs.

## Consequences

- Every wave of a pinned campaign installs from the same snapshot, on the server's mirrors and on relays alike.
- Each server takes three commands instead of one; an agent offline between them stays pointed at the pinned snapshot until its restore runs (up to 7 days of TTL).
- Two pinned campaigns overlapping on one server interleave their source commands; the last restore wins. Avoid overlapping pinned campaigns on the same servers.
- Hand-made changes to a host's sources between the campaign's configuration and its restore are overwritten by the restore, as by any `configure_sources`.
- The server never deletes snapshots today; a retention on the server must keep those `campaign_snapshots` references while their campaign is not final.
- Disk use on relays grows by one snapshot per distinct pinned snapshot, mostly hardlinks.
