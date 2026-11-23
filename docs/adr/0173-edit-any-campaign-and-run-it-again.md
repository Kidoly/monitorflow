# ADR-0173: Editing any campaign, running it again, and AI commands on every server

Date: 2026-11-23
Status: accepted; amends ADR-0179 and ADR-0098

## Context

ADR-0179 let a campaign be edited only until its first batch is queued: after that, it is a record of an update, and changing it means duplicating it. In use, the owner found this a dead end. A campaign that failed on two servers out of sixteen could not have its window moved, its reboot allowed or its failed servers run again without typing a copy, and the page offered only Duplicate and Delete. They asked for Edit on every campaign.

What must still hold, from ADR-0179:

- What a campaign did stays readable: its batches, servers and results are the record of an update.
- The runner never sees a half-edited campaign, and a server's package sources are always put back (ADR-0038).
- Everything the web interface does goes through the public API.

Separately, AI commands are turned on per server or per site (ADR-0098). An administrator with many sites asked for one action for the whole fleet.

## Decision

### Edit at any stage

`PUT /api/campaigns/{id}` stays the whole request, and what it may change depends on where the campaign is:

- **Nothing queued yet** (a draft, or scheduled or paused before its first batch): everything, as in ADR-0179.
- **A run under way** (a batch was queued, not finished): its name, windows and failure threshold. They apply to what is still to be queued: the runner reads them at each step. Every other field must be as the campaign holds it, or the answer is 409 naming what differs ("the servers or batches", "the update", "the pinned snapshots", "how long a command may wait"). The batches in flight are what they are; changing their servers or update half way would leave a record that matches neither version. For a campaign made before its request was kept (ADR-0179), the servers are not compared, since they cannot be read back as written.
- **Finished** (stopped, completed or failed): everything, for its next run. The run that ended moves to the history (below), the servers are frozen again into new batches, and the campaign becomes a draft, started when the administrator is ready. A change of the name alone renames it and leaves the rest as it is. Refused while some of its commands are in flight or package sources are still to be restored, the conditions of a deletion.

The web interface always shows Edit. While a run is under way the form disables the locked groups and says why ("Fixed while the campaign runs."), and sends the locked fields exactly as the campaign holds them. For a finished campaign it says that saving prepares the next run. While commands of a finished run still run, it says to wait for them.

### Runs and their history

A run is one pass of a campaign over its batches. A finished campaign's run is archived when it is edited or run again, in three tables (migration `20261212090000_campaign_runs`):

- `campaign_runs`: number (1, 2, ...), final state, error, start and end, and the settings it ran with as JSON (the request: name, servers, batches, update, windows, threshold, snapshots).
- `campaign_run_waves` and `campaign_run_targets`: the rows of `campaign_waves` and `campaign_targets` as they ended, with the run number, moved out of the live tables.

The live tables keep holding the current run only, so the runner, the pinned snapshots logic (ADR-0038) and every query of the current run are unchanged. Commands keep their `campaign_id`, so a past run's servers read their command's state, exit code and error as before. A finished run that queued nothing (a draft stopped before starting) is not archived: there is nothing to keep.

`GET /api/campaigns/{id}/runs` lists the past runs, newest first, each with its settings, counts, batches and servers in the shape of the campaign detail. The campaign answer gains `previous_runs`, their number. Deleting a campaign deletes its runs (cascade); the audit log keeps every `campaign.*` entry.

Rejected: a run number on the live tables. Every query of the runner, the pins and the API would need a filter on it, for a history that is only read.

### Run again

`POST /api/campaigns/{id}/relaunch` with `{"servers": "all"}` (the default) or `{"servers": "failed"}` runs a finished campaign again with its current settings: the last run is archived, the targets are planned again, and the campaign is `scheduled`, queued at the next window, as after a start. With `failed`, the targets are the servers of the last run whose update failed, expired or was cancelled, or that were skipped with an error, still enrolled, in one batch; the campaign's own request (`server_ids`, `wave_specs`) is untouched, so a later full run covers every server again. Refused with 409 before the campaign is finished, while commands are in flight or sources are to be restored, and when no server failed. Audited as `campaign.relaunch` with the archived run's number.

The web interface offers "Run again…" on a finished campaign, and "Run the N failed servers again…" when some failed, each with a confirmation, and lists the earlier runs under the batches.

### AI commands on every server

`POST /api/remediation/ai-commands` with `{"on": bool}` does what `POST /api/sites/{id}/ai-commands` does, for every site the caller administers at once: each server not already as asked is queued or skipped with why, in one transaction, each switch audited and notified as for one server. "On" reaches only the servers whose machine allowed AI commands to be turned on from the web interface. A caller administering no site gets 403. It is under `/remediation/` so API keys need the `remediation` area, as for the other AI commands routes.

Settings > AI offers "Turn on on every server…" and "Turn off on every server…" with a confirmation and the list of servers that could not be switched. The empty approval queue's second step links there for global administrators, who are the ones who see that page; others keep the per-site and per-server actions.

## Consequences

- No campaign is a dead end: it is renamed, rescheduled or run again where it is, and duplicating is for a different campaign.
- A campaign's detail is its current or last run; the earlier ones are one request away, with what they ran with.
- Editing a finished campaign changes its state back to draft. The audit log records the edit with the number of the run it archived.
- Turning AI commands on for a large fleet is one action, with the same consent rule on each machine as before.
