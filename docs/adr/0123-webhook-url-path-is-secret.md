# ADR-0123: The path and query of a webhook or integration URL are secret

Date: 2026-11-12
Status: accepted; amends ADR-0060, ADR-0078 and ADR-0102

## Context

A security audit (part 5) found that `GET /api/notifications/channels` returned a webhook channel's `settings` as stored, `{"url": ...}`, and `GET /api/integrations` returned an endpoint's `url`. Both list what covers a customer to that customer's administrators (ADR-0078). So when an MSP global administrator set up a webhook or a PSA endpoint covering customer A, A's administrators read its full URL. PSA ingestion URLs and generic hook services often carry a token in the path or the query, so the token crossed the MSP/customer boundary.

The rest of the code already treated those paths as secret: a chat channel's URL is stored in `secret` and never read back (ADR-0077), and the audit keeps only a webhook's or an endpoint's host.

## Decision

Views show a webhook channel's or an integration endpoint's URL as its origin only: scheme, host and port. A channel's `settings` is `{"origin": "https://psa.example:8443"}` and an endpoint has `origin` in place of `url`. This holds for every reader, global administrators included: like a chat URL or a signing secret, the URL is written, never read back.

On `PUT`, an absent or empty URL keeps the stored one. A URL given is compared in full, so the existing rules stay exact:

- a channel's signing secret stays only with the same URL; a new URL, even with the same origin, needs it typed again (ADR-0078);
- a customer administrator keeps a global administrator's private address choice only for the same URL (ADR-0102), which keeping the stored URL is;
- a destination changed below global administration is limited to public addresses, and keeping the URL is not a change.

### Not a move to the `secret` column

Storing the URL in the `secret` column, as for chat, was the other option. For webhooks and endpoints that column already holds the signing secret, so the URL would need another column, a data migration, and changes to the dispatcher and the deliverer. `secret` is not encrypted at rest either, so moving the URL would protect nothing more: what leaked was the API's view, and masking it there closes the leak for existing rows without a migration. The origin is computed in the views' SQL, from URLs that are normalized and carry no user or password when saved.

## Consequences

- The web interface shows the origin with an empty URL field when editing; leaving it empty keeps the URL.
- API clients that read `url` from an endpoint or `settings.url` from a webhook channel read `origin` instead. A channel's `settings` sent back as read is refused (`origin` is not a setting) rather than silently pointing the webhook at the origin's root.
- Someone who needs the full URL again reads it from where it was issued, the PSA or the hook service, not from MonitorFlow.
