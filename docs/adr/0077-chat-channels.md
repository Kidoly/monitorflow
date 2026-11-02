# ADR-0077: Slack, Teams and Discord channels

Date: 2026-11-02
Status: accepted

## Context

ADR-0060 left chat services to the generic webhook: its `text` field shows in Slack and Mattermost. Discord rejects that body, Teams needs a card, and Slack shows one bare line. It also rejected dedicated integrations "unless asked for"; Phase 10 asks for them. Administrators should paste one URL, press the existing test button and see a real message, with nothing to read first.

The URLs of these services are credentials, more than a webhook's: anyone holding one can post to the channel. And the generic webhook shows its URL back in the form, which is wrong for them.

The formats below were checked against each service's documentation on 2026-11-02.

## Decision

**Three new channel kinds**, `slack`, `teams` and `discord`, next to `email` and `webhook`. A generic webhook that points at Slack stays a generic webhook: the migration only widens the check on `kind` and changes no row, and nothing converts a channel.

**The URL is the secret.** It goes in the `secret` column and the API's `secret` field, and is handled like the existing secrets: never returned (`has_secret`), the audit keeps the host only. `settings` holds `{"name", "host"}`: the host, taken from the URL, because the URL cannot be shown, and an optional name, because every Slack channel has the same host. reqwest puts the URL in its errors, which reached `last_error` and the logs, so errors are stripped of it for every webhook, the generic one included.

**With ADR-0078's rules.** Chat channels are channels like the others:

- They cover everything, a customer or a site, and are managed by the administrators of what they cover.
- ADR-0078 keeps a saved secret only with its target, and for a chat channel the target is the URL, which is the secret. So `same_target` compares the service and the host, and the API counts any new URL as a new target even on the same host. An edit of the name or of the coverage keeps the URL. A change of service, or to or from email or a webhook, needs the secret typed again, so a stored URL never becomes a signing key or the reverse.
- A save by someone other than a global administrator sets `public_only` as for any other destination.

**URLs are checked per service**: `https`, default port, no credentials, and:

| Service | Hosts | Path |
| --- | --- | --- |
| Slack | `hooks.slack.com`, `hooks.slack-gov.com` | `/services/…/…/…` |
| Discord | `discord.com`, `discordapp.com`, each also with `ptb.` and `canary.` | `/api[/vN]/webhooks/{id}/{token}`, with no `/slack` or `/github` after |
| Teams | under `api.powerplatform.com`, `logic.azure.com`, and Microsoft's government and China equivalents (`api.gov.powerplatform.microsoft.us`, `api.high.powerplatform.microsoft.us`, `api.appsplatform.us`, `logic.azure.us`, `api.powerplatform.partner.microsoftonline.cn`, `logic.azure.cn`) | contains `workflows`; Microsoft varies the rest |

**Teams means Workflows webhooks**, from the "Send webhook alerts to a channel" template. Office 365 connectors were retired in May 2026, and `*.webhook.office.com` and `outlook.office.com` now answer 200 to anything, so a test would pass and every message would vanish. They are refused by name, with what to do instead. Slack workflow triggers (`/triggers/`) and Discord's Slack-compatible path are not supported: they take other formats.

**Formats.** Each carries a title, the severity for incidents, customer, site, server or relay, and a link. Long text continues under a title cut to 150 characters.

- **Slack**: Block Kit, a `header` and `section`s, with `text` as the notification fallback.
- **Teams**: `{"type": "message", "attachments": [...]}` with an Adaptive Card 1.2, wide, and an `Action.OpenUrl`.
- **Discord**: one embed with a color per tone, posted with `?wait=true`, without which Discord reports no error for a message it did not save.

**Event text is never markup.** It holds server names and errors from machines, and the AI's commands. Slack gets it in `plain_text` objects, with `&`, `<` and `>` escaped and `parse: none` on the fallback. Teams gets it in `RichTextBlock` text runs, which render no Markdown, because `TextBlock` and `FactSet` do and have no documented escape; `{{` gets a zero-width space, as `{{DATE(…)}}` is a function in card text. Discord has no plain text, so every ASCII punctuation mark gets a backslash, and `allowed_mentions` is empty. Control characters become spaces. Limits are counted after escaping, in UTF-16 units so that an emoji cannot overflow them, and a cut never splits an escape: title 150, text 2800 to 3000, facts 200 to 400. A Teams message stays under 24 KB of its 28 KB.

**Links** are in the description on Discord and in a `mrkdwn` link on Slack, not Discord's embed `url` or a Slack button: those are validated, a refusal loses the whole message, and the interface's address is often private.

**Delivery** keeps ADR-0060's rules: three tries, 15-second timeout, no redirects, the error in plain words as the channel's status. No `X-MonitorFlow-*` headers go to chat services; they would not read them. Added for rate limits:

| Service | Pace | Per pass |
| --- | --- | --- |
| Slack | 1 s (documented: 1 per second per webhook) | 20 |
| Discord | 2 s (not documented, which says not to hard code; reports say 30 a minute per channel) | 15 |
| Teams | 12 s (documented: 25 cards per 5 minutes per connection on the action workflows post through) | 5 |

- Beyond the per-pass count the rest are **gathered** into one last message that counts them all, lists the first 15, and links to the incidents. Delivery is at most once and a pass cannot wait for 100 messages at 12 seconds each, so waiting was not an option; one message that says how many is also more use in an outage.
- A 429's `Retry-After`, or Discord's `retry_after` in the body, sets the wait before the next try, up to 60 seconds. After three limited tries the channel gets nothing more that pass: the rest are lost, as they would be anyway, and Discord blocks an IP that keeps hitting limits. The status says it is being limited.

Rejected:

- **Slack, Teams or Discord apps with OAuth.** More power than a notification needs, and an app to register per installation; a URL takes a minute.
- **Showing the URL back, or its last characters.** The Discord and Slack path is the credential; the name does the job.
- **Allowing any host, for proxies or self-hosted relays.** The generic webhook covers those, with a URL the administrator chose to show.
- **Rewriting Slack URLs in generic webhooks into Slack channels.** Their receivers may be relying on the JSON body, and the migration must not alter them.
- **A separate address check for chat channels.** The host list already is one: only those public names pass, never an IP address in any form (`127.0.0.1`, `[::1]`, `2130706433`), and redirects are not followed. ADR-0078's public-address client and its check of addresses written in a URL still apply to a `public_only` chat channel, so a row written outside the API meets the same rule.

## Consequences

- An administrator gets a real message from three services in a minute, and the form says where each URL is.
- **A Teams test proves less.** A workflow answers 202 when it accepts the request and may still fail afterward, so "Test sent" there means accepted. The screen says to check that it arrived. Microsoft may also turn off a flow after 14 days of failures, or after 90 days without a trigger on some plans, so a quiet Teams channel needs an occasional test.
- Failures keep the existing retries even when final, like a Slack `no_service` or a Discord 404; Discord asks not to reuse a webhook after a 404, which a channel would keep doing until someone edits it. The status names the problem.
- A chat channel can hold its pass, and so the next pass of every channel, for up to about 48 seconds on Teams, and for minutes if a service keeps asking to wait. Messages already claimed in that pass go out to other channels at once.
- Pacing lives in the pass, so two passes in a row may send two messages closer than the pace, and several channels on one URL do not share it. A 429 corrects both.
- A server rolled back to a version without these kinds fails to read the channel list while such a channel exists, and sends no notifications until it is deleted.
- The Teams host list and the card have not been tried on a real tenant, and government hosts come from Microsoft's table of allowed hosts, not from seen URLs.
