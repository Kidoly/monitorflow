# ADR-0078: Notification routing per customer and site

Date: 2026-11-02
Status: accepted; on-call schedules cover places the same way (ADR-0081), and follow-ups (ADR-0082); a channel can be kept for follow-ups only

## Context

ADR-0060 let a channel be for every customer or for one, left sites out, and kept channels to global administrators. An MSP's customer could not look after its own channels, and a site could not have its own. Alert grouping, escalation and on-call will build on who hears about what, so that needs one small, clear answer.

ADR-0060 could keep webhook URLs and mail servers unchecked because "only global administrators set them, and they already control the server". Letting anyone else set them ends that argument, so this decision has to replace it.

## Decision

**Coverage.** A channel covers everything, one customer or one site. `notification_channels` gains `site_id` next to `customer_id`, with `CHECK (customer_id IS NULL OR site_id IS NULL)`, the shape of a grant in `user_roles` (ADR-0026). `notify::Coverage` is the one place that decides, in `includes(customer, site)`:

- Everything takes every event.
- A customer takes events about that customer, so all its sites.
- A site takes events about that site.

An event with no customer reaches only channels for everything, and one with a customer but no site reaches no site channel. Coverage never widens when something is missing.

**The event's site** comes from a stored generated column on `notification_outbox`, `(details #>> '{site,id}')::uuid`. Every writer already puts the site in `details` for the webhook payload, so the triggers do not change, rows waiting during the upgrade get it too, and the routing and the payload cannot disagree.

**Where each event goes:**

| Event | Everything | Its customer | Its site |
| --- | --- | --- | --- |
| `incident.opened`, `incident.escalated`, `incident.resolved`, of a server or relay, offline ones included | yes | yes | yes |
| `approval.waiting` | yes | yes | yes |
| `update.failed` of a server (`apply_updates`, `self_update`) or a relay (`self_update`) | yes | yes | yes |
| `update.failed` of a campaign that names a site | yes | yes | yes |
| `update.failed` of a campaign that names no site | yes | yes | no |
| `update.failed` of an automatic rollout | yes | no | no |
| `backup.failed` | yes | no | no |
| `update.available` | yes | no | no |

A campaign without a named site spans its customer's sites and reports once (ADR-0060), so it has no one site to name; its sites' channels do not hear of it. A rollout, a backup and a release concern the central server, which belongs to no customer. "Send test" goes to the channel it was pressed on and names no place.

**Who manages which channel.** The rule is one sentence: **an administrator manages the channels of what they administer.**

| Channel covers | Who sees, creates, changes, tests and deletes it |
| --- | --- |
| Everything | Global administrators |
| A customer | Administrators of that customer, and global ones |
| A site | Administrators of that site, of its customer, and global ones |

Operators and viewers have no access: a channel holds addresses and webhook URLs, which are configuration of a higher order than thresholds. A site administrator can manage their site's channels though ADR-0029 refuses them enrollment: enrollment adds an identity to the customer, while a channel only tells someone about a site they already read. A customer administrator sees that customer's channels and those of all its sites, and a site administrator only those of that site, since a customer channel also carries other sites.

Enforced in `api::notifications`, not the web interface. A user with no administrator grant gets 403. An administrator gets 404 for a channel they do not manage, as for a site they do not read (ADR-0026). Creating, or moving a channel, to a coverage they do not manage is 403, decided before any lookup, so the answer is the same for an ID that exists and one that does not. Moving a channel needs both the old and the new coverage, so a customer administrator cannot hand a channel to another customer or widen it to everything. Changes lock the row, so they apply to what was authorized.

**A channel's customer or site is removed: the channel goes too** (`ON DELETE CASCADE`, what customers already did). `SET NULL` would make it a channel for everything, which is the leak to avoid. Keeping it, stopped, would need a state that the interface must explain and someone must clean up, for a mail password nobody needs any more. No API removes customers or sites today; the path that adds one must say so and audit it, since a cascade does not.

**Destinations chosen below global administrators reach only public addresses.** A customer administrator must not be able to aim the central server at its own network: a webhook is a `POST` to any URL, and VictoriaMetrics, unauthenticated on that network, deletes every customer's series on a `POST`. `notification_channels.public_only` records that someone other than a global administrator chose the destination. Delivery then refuses loopback, private, link-local and metadata addresses, IPv6 forms wrapping an IPv4 address, and names resolving only to those. The check is on the addresses actually connected to (a resolver for webhooks, the connection address for SMTP, with the certificate still checked against the name), so a name cannot change after the check. No proxy is used for these requests, since a proxy would resolve the name itself. There is no separate check when saving: saving sends a test, which shows the refusal in plain words.

- A global administrator's save clears the flag: that is them vouching for the destination, as ADR-0060 assumed.
- Anyone else changing the host, port, username or URL sets it. Changing only the recipients or the coverage leaves it, so a customer administrator can maintain a channel that uses the MSP's relay on a private address, but not re-aim it.
- Existing channels have it off and deliver as before.

**A saved secret stays with its destination.** ADR-0060's "an edit with an empty secret keeps it" would let a customer administrator change the mail server on a channel the MSP set up and receive the MSP's password at the next test. A change of host, port, username, URL or kind now needs the secret typed again; otherwise 400 with a sentence saying so. This holds for everyone: one rule, and it also protects a global administrator from a typo. Secrets are still never returned. A customer administrator does see a channel's settings, including a webhook URL and a mail username, so a global administrator should not put a shared token in a channel scoped to a customer.

**Existing channels.** Those without a customer stay that way and are "Everything". Those with a customer keep it; making them "Everything" would send them other customers' alerts. Neither changes behaviour, and pending outbox rows are delivered as they would have been.

**API, additive.** Requests take `site_id` next to `customer_id`, and both at once is 400. Views gain `site_id` and `site_name`; for a site channel `customer_id` and `customer_name` are the site's, so a consumer that reads a null `customer_id` as "everything" stays right. The audit details gain `site_id`.

**Interface.** The form's "Notify about" lists what the user may choose, widest first: Everything, All of acme, acme / paris, with a line saying what the choice means. It does not ask when there is one answer. An install with one customer never hears of customers, and one with one site sees no field, as before. Administration › Notifications shows to every administrator.

Rejected:

- **A list of targets per channel, or rules on severity or event.** More to explain, and the same destination can be added twice. ADR-0060's "few settings" stands.
- **Giving site channels a `customer_id` as well, with a composite foreign key.** Two columns to keep true for no gain: sites do not move between customers (ADR-0026), and the site alone decides.
- **Replacing the trigger functions to write `site_id`.** Four functions rewritten to copy a value they already store.
- **Letting only global administrators change destinations.** Customer administrators could then only look, which is not managing their channels; `public_only` removes the reason.
- **Checking private addresses for every channel.** A mail relay on the local network is the usual setup for a global administrator, and ADR-0060 allows it.

## Consequences

- An MSP's customer can run its own channels, and alerts of one customer cannot reach another's, in the API or at delivery. The tenancy test covers every channel route.
- This replaces ADR-0060's consequence that only global administrators set webhook URLs, and its rule that an edit always keeps the secret; "customer administrators managing their own channels" is no longer rejected.
- Later tasks reuse `Coverage::includes` and `notification_outbox.site_id`.
- A customer needing an internal webhook asks a global administrator to save it.
- A site channel hears nothing of a campaign that spans its customer without naming the site.
- Customer administrators could send many tests to a public address. They are audited only when a channel changes, not per test.
