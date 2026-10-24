# ADR-0045: The approval queue and the remediation audit log

Date: 2026-10-28
Status: accepted

## Context

The incident loop (ADR-0043) parks a command the risk rules say needs a human in `remediation_pending_commands` and moves its incident to `awaiting_approval`. It resumes when the incident's events end with `approval` (or `edit` then `approval`) or `rejection` and its state is back to `running`; nothing else in the loop changes. Architecture §8 says an approver accepts, edits or rejects each command, and §9 names four roles (viewer, operator, approver, administrator) without saying what each may decide. Approvers also need to read what happened, across incidents: the incident detail (ADR-0042) shows one chain at a time.

## Decision

**Listing.** `GET /api/remediation/approvals` lists the pending rows of incidents in `awaiting_approval`, for every role on the server's site, as the incidents API scopes them. An item carries what a decision needs: the server, the alert's series, the command, the AI's diagnosis and reasoning (untrusted text, shown as data), the rules' tier, rule and reason, the commands run against the limit, and the deadline `opened_at + max_duration_s`. It also carries `can_approve` and `can_reject`, computed with the rules below so the interface does not guess, and `snapshots: {available, reason}` from ADR-0044, because approvers are rarely the customer administrators who may read `/api/servers/{id}/proxmox`. At most 500 items, nearest deadline first: one per active incident, so no paging.

**Who decides.** Decisions follow the tier, on the incident's site, through any grant (global, customer or site):

| Decision | Role needed |
| --- | --- |
| Approve a low-impact command | approver or administrator |
| Approve a high-impact command | administrator |
| Reject any command | approver or administrator |

Viewers and operators never decide: operators change configuration, approving runs commands as the agent's user. High impact is administrator-only because §8 says those commands always need approval and they are the ones that cannot be undone; a second, stronger role makes "always" mean something. Rejecting is the safe side and stays with every approver. A pending command outside the user's sites is 404, one it reads without the role 403.

**Edits.** `approve` takes an optional `command`. When it differs from the proposal, the rules classify it again before anything is written: forbidden is 400 with the rule's reason, and the role needed is that of the higher of the two tiers, so editing never lowers the bar (a high-impact proposal edited into a restart still needs an administrator, and a low-impact one edited into a package removal too). The edit is recorded as `edit {original, edited}` before `approval {command}`. The loop classifies the approved command once more before queueing it (ADR-0043 step 4), so a rule added in between still applies. The command is 1 to 4096 bytes without NUL, the limit of the pending table.

**Rejections need a reason**, 1 to 1000 characters. The loop passes it to the AI as "rejected by an approver: <reason>", so the next proposal can account for it; the interface says so.

**One transaction, optimistic.** `remediation::approval::decide` locks the incident row (the lock the loop and the chain trigger take), then the pending row, and refuses with 409 when the pending row is no longer `pending`, the incident no longer `awaiting_approval`, or the deadline passed (by the database clock). Otherwise it appends the decision events and `state_change awaiting_approval → running`, all with the user as actor, marks the pending row `approved` or `rejected` with `decided_at` and `decided_by`, and sets the state. Two concurrent decisions queue on the lock and the second gets 409; nothing of it is written. The API adds an `audit_log` row (`remediation.approve`, `remediation.reject`, target the incident, details the pending ID, tier and whether it was edited) in the same transaction, which is the join ADR-0042 anticipated between configuration audit and remediation history. The loop's next poll (10 s) leases the incident and resumes.

The pending row is kept and marked rather than deleted: the table's `state`, `decided_at` and `decided_by` columns exist for this (migration `20261027090000`), the loop already ends rows that way (`expired`, `cancelled`), and the listing reads only `pending` rows. The chain stays the record; the row is working state that also answers "who decided" without reading events.

**The deadline applies.** A decision after `opened_at + max_duration_s` is 409 even if the supervisor has not escalated the incident yet (it does within a poll). An approval on a system that kept changing for longer than the limit would be stale, as ADR-0043 says.

**Audit log.** `GET /api/remediation/events` reads `remediation_incident_events` across incidents, newest first by `(created_at, incident_id, seq)`, scoped by readable sites, filtered by customer, site, server, incident, kind, actor kind, actor ID and a time range, with keyset pages like the other listings. Migration `20261028090000_remediation_events_time` adds the index it walks. Items carry the incident's server and series and the user name of user actors. Payloads leave out the rendered context and command output: up to 64 KiB per stream, 500 times, is too much for a timeline, and the incident detail serves them. Chain verification stays per incident, on the detail route: verifying every chain a page touches would read each chain in full on every page. The interface verifies the chain when the log is filtered to one incident. The configuration `audit_log` keeps its own table and gets no listing here; a reader for it is separate work.

## Consequences

- An approver acts through the API only; the loop is unchanged and resumes within a poll.
- Customers that want approvers to handle high-impact commands must make them administrators of the site, which also lets them enroll and revoke on customer grants. A per-tier role setting can come later if that is too coarse.
- A decision can fail with 409 for reasons the approver did not cause (the alert resolved, the deadline passed); the interface refreshes the queue then.
- Rejection reasons reach the AI provider. They are typed by humans and are not redacted; the interface warns against putting secrets there.
- The audit log listing has no full-text search and no CSV export.
