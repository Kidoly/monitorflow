# ADR-0040: Incident context and secret redaction

Date: 2026-10-26
Status: accepted

## Context

Flow step 2 of architecture §8: when an incident opens, the server gathers the context the AI will diagnose from (sensor history, recent logs, server inventory, recent changes). That context leaves the network for an external API (ADR-0003), so §8 "Threats addressed" applies: secrets are redacted before sending, a request carries data of a single customer (MSP mode, ADR-0026), and logs or command output may hold text crafted to steer the AI, so the context must be passed as clearly delimited data. Its size must also be bounded: provider requests have token limits and cost, and one noisy server must not make a request huge or slow.

What exists to read: incidents and their threshold snapshot (ADR-0017), the server, site, customer and relay rows, installed and upgradable packages (ADR-0031), the commands journal with output truncated to 64 KiB (ADR-0031), campaigns and their targets (ADR-0033), series in VictoriaMetrics (ADR-0013, ADR-0018), and the stream events of each peer in VictoriaLogs, the only logs the server stores today (ADR-0013, ADR-0024). There is no server group model and no package change history.

## Decision

**Module.** `remediation::context` builds a typed `Context` with `gather(backends, trigger, limits, now)`; `Trigger::of_incident` reads the breach of an incident. `remediation::redact` holds the redactor. The AI prompt (a later task) embeds `Context::render()`.

**Sections and bounds** (`Limits`, defaults in brackets). Each section is read concurrently under its own timeout [10 s]; a backend that is not configured, fails or times out makes its section `unavailable` with a fixed reason, never the whole context. Only a missing server or a failed inventory read is an error.

| Section | Source | Bounds |
| --- | --- | --- |
| incident | the incident row | threshold text 512 bytes |
| inventory | server, site, customer, relay rows; package count; upgradable packages by name | 30 packages |
| history | the breached series, `max_over_time` (or `min_over_time` for `<` thresholds) per step, with the `peer`/`site` matchers and `extra_label`s of ADR-0018 | 6 h, 72 points, 4 series |
| logs | VictoriaLogs stream `{peer, site}` of the server, newest first | 24 h, 40 lines of 400 bytes |
| changes | commands of the server (package changes are `apply_updates` commands, parameters decoded as in the API); campaigns targeting it | 14 days, 10 commands with the last 2 KiB of stdout and stderr, 5 campaigns |

Everything dropped is said with a `[TRUNCATED: …]` marker: how many rows, points, bytes or older lines. Every rendered section is also capped at 16 KiB, cut at a line end. The whole render is therefore at most about 100 KiB.

VictoriaLogs is read with the existing `VmQuery` client through a new `get_raw`, which keeps its timeout, size cap, no redirects and URL-free errors; LogsQL answers JSON lines, not the Prometheus envelope.

**One customer.** Every read is for one server: SQL by `server_id`, VictoriaMetrics and VictoriaLogs by the server's `peer` and `site`. Rows carry what identifies their owner (the command's `server_id`, the campaign's `customer_id`, the series' and log lines' `peer` and `site` labels as answered), and `Context::check_owner` asserts all of it against the server's customer. Campaigns are deliberately not filtered by customer in SQL, so the assertion has something to check. `gather` and `render` both fail with `ForeignData` on any mismatch: a context with another tenant's data is a bug and nothing is sent.

**Redaction.** Every free-text field (hostnames, names, versions, instances, log lines, command parameters, output and errors, campaign names and errors) goes through one `Redactor` while the context is built; such fields have the type `Redacted`, which only the redactor creates, so the struct cannot hold unredacted text. Redaction runs before truncation, so a cut cannot leave half of a secret unmatched. Matches become `[REDACTED:<kind>]`; counts per kind are kept in `Context::redactions` (serialized as `{"kind": n}` for the audit log) and rendered in a last `redactions` block. Rules, most specific first:

1. PEM blocks (`private_key` when the label says PRIVATE, else `pem_block`), also truncated or with literal `\n`.
2. Known formats: JWT, AWS access key IDs, GCP API keys and OAuth tokens, Azure client secrets and SAS signatures, GitHub and GitLab tokens, Slack tokens.
3. `scheme://user:password@`, `Authorization: Basic`, `Bearer` values of 16 characters or more.
4. Connection strings: `Password`, `Pwd`, `AccountKey`, `SharedAccessKey`, `SharedAccessSignature` up to the next `;`.
5. Key-value pairs: a key ending in `password`, `passwd`, `passphrase`, `pwd`, `secret`, `token`, `api_key`, `access_key`, `private_key`, `auth_key` or `credential(s)`, then `=` or `:`, then a quoted or bare value. Empty and masked values and references (`$VAR`, `${VAR}`, `%(name)s`) are kept.
6. High entropy: runs of at least 32 characters of `[A-Za-z0-9+/_-]` that mix upper case, lower case and digits, do not start with `/`, have at least 4.0 bits of Shannon entropy per character and change character class between at least 40 % of adjacent pairs. Mixed case keeps hex hashes, UUIDs and Git IDs out; the class changes keep long CamelCase identifiers out, whose entropy alone is as high as random text. This catches about 99 % of random 32-character base62 strings. Hex secrets without a telling key are not caught; base64 digests and SSH fingerprints are redacted.

The rules over-redact by design. A corpus of positive and negative cases (`remediation/redact_corpus.txt`) is the test; a false positive or a miss found later becomes a line there.

**Rendering.** Deterministic: the same context renders the same bytes. Each section is a block `=== BEGIN DATA <name> <tag> ===` … `=== END DATA <name> <tag> ===`, the tag being the first 8 bytes of SHA-256 over the name and body. Data cannot forge the closing line, which would need a body containing its own hash, and control characters in values are escaped, so data cannot start a line of its own either. Multi-line values are indented with `| `.

## Consequences

- The prompt task can embed the blocks as they are and tell the model that everything between the markers is data. Risk tiers still apply whatever the model says.
- The audit task records `Context::redactions` with the rendered text it sent.
- Logs are only stream events today; when agents forward log lines (ADR-0024's follow-up), they flow into the logs section and its redaction unchanged, filtered by the same stream fields.
- Server groups do not exist; the inventory names site and customer. A groups section is added with the model.
- There is no package history, only the latest report. Package changes come from `apply_updates` commands; changes made outside MonitorFlow show only as a new report time.
- Redaction is rule based: a short password outside a key-value, URL or connection-string shape is not recognised. New shapes are added as rules with corpus lines.
