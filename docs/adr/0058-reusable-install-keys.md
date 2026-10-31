# ADR-0058: Reusable install keys per site

Date: 2026-10-29
Status: accepted; relays use install keys too since ADR-0066

## Context

Enrolling an agent takes a single-use token (ADR-0007) created for one peer ID, so adding twenty hosts means twenty visits to the tokens page, twenty IDs typed, and twenty different commands. The simplicity audit (`docs/design/simplicity-audit.md`, M1 and M2) found this to be the main obstacle after the server install. Operators expect one command per site that they paste on every host or put in a deployment script, with each host appearing under its own name.

A reusable secret changes the trust model of ADR-0007, where a token proves an administrator chose one identity:

- **Replay.** Whoever holds the key can enroll hosts until it stops working. It may be copied into scripts, chat or a wiki.
- **Identity.** The ID can no longer come from the administrator. It comes from the host, which is untrusted input.
- **Duplicates.** Two hosts can have the same name, or a host can be reinstalled.
- **Volume.** A leaked key can be used to create many fake servers.
- **Revocation.** It must stop new enrollments without affecting the hosts it already enrolled.

## Decision

**Same format, separate table.** An install key is a string in the token format `mfe1.<secret>.<ca>` of ADR-0007, so `MF_TOKEN`, `MONITORFLOW_ENROLLMENT_TOKEN`, the MSI's `ENROLLMENT_TOKEN` and `enrollment_token` in `agent.toml` take it unchanged, and the client pins the CA as before. Only `SHA-256(secret)` is stored, in `install_keys`, with the site, the creator, `expires_at`, an optional `max_hosts`, `host_count`, `require_approval` and `revoked_at`. The key is shown once at creation. Storing it so that it could be shown again would make a database dump enough to enroll hosts. Losing a key costs one click to create a new one. Install keys enroll agents only. Relays keep single-use tokens, which carry their server names (ADR-0025).

**Defaults.** A key works for 30 days (1 hour to 1 year) on any number of hosts, without approval. The form shows these as its initial values. The page leads with the site's install command. Single-use tokens move under "Advanced", which opens by itself when the relays page links to it.

**The host name becomes the ID.** `EnrollRequest` gains `hostname` (field 3, additive). Agents send the kernel's host name, or the DNS host name on Windows. Relays send nothing. The server tries single-use tokens first, so their behaviour does not change. Otherwise it looks up an install key by the same hash, locks the key's row, and checks that the key is not revoked, not expired and not used up. The host name is turned into a peer ID: lowercase, every run of other characters becomes `-`, at most 64 characters, and `host` if nothing is left. The CSR is still ignored apart from its key.

**Never an existing ID.** If any server row, agent certificate or agent token (used or pending, revoked or not) already has that ID, compared without case, the server tries `-2`, `-3` and so on up to `-1000`. Under a transaction-scoped advisory lock, two concurrent enrollments cannot pick the same ID. So a key cannot impersonate or take over an existing server, and a reinstalled host appears as `web-01-2` next to the offline `web-01`. Re-enrolling under the old name is the "Remove a server" task, which revokes the old server first.

**Recorded like a token.** Each enrollment through a key inserts a used `enrollment_tokens` row for the new ID, in the key's site, with `install_key_id` set. Its hash is that of a random secret that is thrown away. The site of the first Hello (ADR-0008), the relay check for agents without a row yet (ADR-0028) and the audit trail therefore work as for tokens, without changes. The key's `host_count` goes up in the same transaction as the certificate, and the `peer.enroll` audit row names the key. `GET /api/tokens` leaves these rows out.

**Approval.** With `require_approval`, the enrollment also writes a `pending_hosts` row. The certificate is issued as usual, because the installer waits for the identity files. The agent's Hello is refused with `UNAVAILABLE` "waiting for an administrator to approve this host", which agents already retry with backoff, so no agent change is needed. Until then the host has no server row and appears in no list or metric. The tokens page lists waiting hosts with their host name, given ID and source address. Approve deletes the row, and the next retry creates the server. Reject deletes the row and revokes the host's certificates through the CRL publisher, as revocation does (ADR-0029), so the ID is never used again.

**Rate limiting.** The per-source failure limiter of ADR-0007 still counts unknown, expired, used-up and revoked keys, which all get the same `PERMISSION_DENIED`. A valid key also enrolls at most 30 hosts a minute. More get `RESOURCE_EXHAUSTED`, which agents retry, so a large rollout only slows down. This counter lives in the database, under the key's row lock, so it is exact across servers and restarts. A key without the host name, from a relay or an agent older than this field, gets `INVALID_ARGUMENT`, which tells the operator to use a single-use token.

**Revocation.** `DELETE /api/install-keys/{id}` sets `revoked_at`. The row stays, because enrollments reference it. Enrollment waits on the row lock and then sees the revocation. Hosts the key enrolled keep their certificates. Revoking one of them is a separate action, as for tokens.

**Administration.** Creating, listing and revoking keys, and approving and rejecting hosts, need an administrator role on the site's customer (ADR-0029). Every action is audited (`install_key.create`, `install_key.revoke`, `pending_host.approve`, `pending_host.reject`), and the tenancy test has a case for each route.

## Consequences

- One command per site enrolls any number of hosts. A leaked key lets anyone who can reach port 7445 add servers to that site until the key expires, is used up or is revoked. Those servers can send metrics and receive the site's sensor configuration, but they cannot impersonate existing hosts or read other sites' data. Approval, a host limit and a short validity reduce the exposure for sites that want it.
- On Linux the installer removes the key from the host once the agent is enrolled. The Windows MSI writes it into `agent.toml` as it does for tokens, and it stays there until removed, now with a longer validity. A later change can have the agent remove it after enrollment.
- Server IDs follow host names. Renaming a host does not rename its server.
- Agents older than this change, and relays, cannot use install keys. Single-use tokens remain for them and for strict environments.
- There is no CLI command for install keys yet. Scripts on the server host keep using `token create`.
