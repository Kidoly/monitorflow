# ADR-0179: Editing and deleting campaigns

Date: 2026-11-22
Status: accepted; amends ADR-0033

Numbered ADR-0166 when merged, renumbered because three other ADRs had the same number. The migration `20261203090000_campaign_edit_delete.sql` still names it ADR-0166: a migration already applied cannot change.

## Context

ADR-0033 made campaigns immutable once created and never deleted. In use, that leaves an administrator stuck: a typo in a draft's name, a forgotten server or a wrong window means stopping the draft and typing everything again, and the list keeps every stopped draft and finished campaign forever. Administrators asked to edit a campaign that has not started, copy one to run it again, cancel one in progress and remove finished ones.

What must hold:

- A campaign that sent commands must keep showing what it did: its batches, servers and results are the record of an update.
- Nothing about a campaign may disappear without a trace: the audit log is append-only and must say who removed what.
- A server pointed at pinned snapshots must get its own package sources back (ADR-0038), whatever happens to the campaign.
- Everything the web interface does goes through the public API.

## Decision

**Edit until the first batch is queued.** `PUT /api/campaigns/{id}` takes the creation body and replaces the whole campaign: its fields, and its targets frozen again into batches. It is allowed while the campaign queued nothing: a `draft`, or a `scheduled` or `paused` campaign whose `current_wave` is still NULL and that has no command. The state, `created_by` and `created_at` stay. Once a batch was queued the campaign is a record and the request is 409 `cannot edit a campaign that queued commands already: duplicate it instead`; a final campaign is 409 `cannot edit a campaign that is <state>`. The edit runs under the campaign's row lock, as every other change (ADR-0033), so the runner never queues a half-edited campaign. It is audited as `campaign.update` with the same details as `campaign.create`.

A full replacement, not a patch: the form sends what it shows, and an API client reads the campaign and sends it back changed. Partial updates would need rules for every field that depends on another (agent version against package options, sources against OS updates).

**Keep the request.** Targets are frozen, so `server_ids` and `waves` could not be read back from a campaign: `{"percent": 10}` became a list of servers. Two columns, `requested_server_ids` and `requested_waves`, keep them as written, and the API returns them as `server_ids` and `wave_specs` (`waves` already holds the batches). They are NULL for campaigns made before; the web interface then falls back to the frozen batches.

**Duplicate on the client.** Copying a campaign is reading it and creating another from its fields, which the API documents; there is no copy route. The web interface opens the creation form filled with the campaign, so the administrator changes what differs before saving. A server-side copy would have to choose what to keep (the site's servers as they were, or as they are now) without asking.

**Cancel is stop.** `POST /api/campaigns/{id}/stop` already ends a campaign for good; the web interface names it "Cancel campaign" while it runs.

**Delete drafts and finished campaigns.** `DELETE /api/campaigns/{id}` removes a `draft`, `stopped`, `completed` or `failed` campaign with its batches, targets, windows and pins (cascade). A `scheduled`, `running` or `paused` one is 409: stop it first. It is also 409 while any of its commands is `queued`, `sent` or `running` (a stopped campaign lets sent commands finish), and while a server's package sources still have to be restored, the condition the runner uses to keep restoring after the end. Commands keep their history: the foreign key from `commands.campaign_id` becomes `ON DELETE SET NULL` (migration `20261203090000_campaign_edit_delete`). The deletion is audited as `campaign.delete` with the name, customer, state, number of servers and dates, in the same transaction; earlier `campaign.*` entries stay.

Rejected: a soft delete (`deleted_at`). It keeps rows nobody can see and every query must filter them; the audit log already says what was there.

## Consequences

- A campaign's details are the request as last written; the audit log holds each version.
- Commands of a deleted campaign show no campaign; their `apply_updates` payload and results are unchanged.
- The web interface shows Edit for a campaign that queued nothing, Duplicate always, Cancel while it runs and Delete for drafts and finished campaigns, with a confirmation.
