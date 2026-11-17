# ADR-0147: Subscription tiers, their license key and the tested channel

Date: 2026-11-17
Status: accepted; extends ADR-0100

## Context

ADR-0100 made the license key a signed file that lets additional central servers start: the Enterprise edition. `docs/product.md` also sells subscriptions for support and for an update channel of releases tested longer, in tiers: Community (free), Basic, Standard, Premium and Enterprise. Nothing in the product knew about those tiers yet.

The owner's decisions, which bind this design:

- Open core. Everything is free and unlimited (servers, users, features) except the Enterprise edition: high availability, more than one central server. Support is paid.
- Never a nag screen. A missing or expired subscription never stops monitoring, alerts or running central servers.
- Prices belong to the website, not to the product. The proposal at the time of writing, per managed server per month, billed yearly: Basic 2 EUR, Standard 4 EUR, Premium 7 EUR; Enterprise is Premium plus 2,500 EUR per central server per year.
- No billing or payment in the product yet.

## Decision

### One key for every tier

The license key of ADR-0100 becomes the key of every subscription. Same blocks, same signature (Ed25519 over the bytes of the JSON, `MONITORFLOW LICENSE` and `MONITORFLOW LICENSE SIGNATURE`), same key pair of its own, same offline check against the public keys compiled in from `ee/license-keys.pem`. A Community install has no key, and never needs one.

New keys are **format 2**:

| Field | Meaning |
| --- | --- |
| `format` | 2 |
| `id` | A UUID, for the audit log and support |
| `customer` | Who it is for |
| `tier` | `basic`, `standard`, `premium` or `enterprise` |
| `managed_servers` | Managed servers the subscription covers |
| `server_instances` | Central servers allowed on the database, the first one included. 1 for every tier but `enterprise` |
| `support` | Where to ask for support: an email address or a URL. Optional |
| `issued`, `expires` | Dates (`YYYY-MM-DD`, UTC), valid through the whole day of `expires` |

Format 1 keys, issued under ADR-0100 for high availability, keep working unchanged: they read as tier `enterprise`, with `managed_servers` not stated. A key with a format the server does not know is refused with "this license needs a newer version of MonitorFlow", so a server older than this ADR says that of a format 2 key, rather than misreading it. The API keeps the field `edition` (`enterprise` when the key includes the Enterprise edition, `core` otherwise) for scripts written against ADR-0100.

Premium's "Enterprise edition: option" in `docs/product.md` means upgrading to the Enterprise tier, which is Premium plus central servers: only an `enterprise` key may allow more than one, and the server refuses a format 2 key of another tier that says otherwise, as malformed.

The verification stays in `ee/`, under the Enterprise license: the AGPL build (`--no-default-features`) reads no key, shows "not included" on the License page as today, and follows the regular channel. Subscriptions are sold with the official build, which includes `ee/`.

### What a key changes, and what it never changes

- **High availability** (ADR-0100) is unchanged, except that the license must be `enterprise` to add a central server. A Basic, Standard or Premium key is refused with "the Standard subscription does not include the Enterprise edition, which adds central servers", and how to get one.
- **The update channel** of the central server: a valid key of any tier makes it follow the tested channel (below). Without a key, or once the key has expired or stops verifying, it follows the regular channel, as before this ADR. Nothing else.
- **Managed servers above the number covered** change nothing but a calm note on the License page: "MonitorFlow manages 112 servers and the subscription covers 100. Everything keeps working; your support contact can cover the others at renewal." No banner, no email, no limit. The count is the servers whose agent is not revoked.
- **Nothing else.** No feature, user, server or alert depends on a tier. Expiry of a key is never announced outside the License page, except the notice of ADR-0100 to global administrators of an install with added central servers, whose growth it stops.

### Administration › License

The only place that mentions subscriptions. It says, in plain words: the tier, for whom, until when, what the tier includes (update channel, support by tier, and for Enterprise the central servers), the support contact, and how many managed and central servers the key covers against how many there are. Without a key: "Community edition: every feature, for any number of servers and users, free. Support from the community." with where to ask, and the install form. Nothing is shown to anyone but global administrators, as today.

### The tested channel

**What it is.** A release follows the regular channel as soon as it is published (`releases/latest`), as today. The owner promotes a release to the tested channel once it has run long enough, typically a few weeks on reference servers and the showcase site without a regression. Subscribers' central servers then see it as their latest release; until then they see the previous tested one.

**The channel file.** `tested.json`, published with the releases, next to `SHA256SUMS`:

```json
{"channel": "tested", "version": "0.7.0", "promoted": "2026-12-08"}
```

and `tested.json.sig`, the raw Ed25519 signature of exactly those bytes by the **release signing key** (ADR-0053), as `SHA256SUMS.sig`. The channel file is not in `SHA256SUMS`: it changes after the release is signed. The release key is the right key: the file says which release servers install, which the release key already decides, and servers already trust it; the license key never enters CI (ADR-0100).

**Where it lives.** At a fixed address for the server: `releases/latest/download/tested.json`, so the latest release always carries the current channel file.

- **Promotion** is a manual run of `release.yml` (`workflow_dispatch` with `promote: true`) on the tag of the release to promote: `gh workflow run release.yml --ref v0.7.0 -f promote=true`. Its `promote` job runs in the `release` environment, so only a `v*` tag can start it and only the owner's approval releases the key (ADR-0128): the key passes through no one else. It checks that the tag is a published release, writes and signs `tested.json`, verifies the signature with `deploy/release-keys.pem`, and uploads both files to the promoted release and to the latest release, replacing older ones.
- **A new release** carries the channel file forward: its `release` job copies `tested.json` and its signature from the release that was latest before it, verified, into its own files. A first release, or one after no promotion yet, publishes none; servers on the tested channel then see no release at all, and the License page says so.
- **A self-hosted release directory** (`release_url`, ADR-0056) holds `tested.json` next to the other files when the administrator copies it there; the release it names must be the one the directory holds.

**How the server reads it.** The update check (the button, and the daily check of ADR-0061) asks the License status which channel to follow. On the tested channel it downloads `tested.json` and its signature from the release source, verifies them against the release keys built in, and fetches that release's files from `releases/download/v<version>/` with the version expected, verified as today. Anything that does not verify is an error of the check, shown as today, never a fall back to the regular channel. An attacker who can only replay an older channel file can only hide newer releases, as with `releases/latest` today. The updater is unchanged: it already installs a given version.

The check result and `GET /api/server-update` say which channel was used (`channel`: `regular` or `tested`), additively; the Update page itself does not mention subscriptions.

### Issuing keys

`scripts/license-sign.sh` gains `--tier`, `--managed-servers` and `--support`, writes format 2, and keeps refusing what does not make sense (more than one central server without `--tier enterprise`). It runs on the owner's machine with the private key in a file the owner gives it, as ADR-0100 says; `docs/releasing.md` documents it next to the release key. Tests make throwaway key pairs and never touch the real one.

## Consequences

- One key for every subscription, checked offline: networks without a way out are covered.
- Old servers refuse format 2 keys in plain words ("needs a newer version"); an Enterprise customer on an old server can be issued a format 1 key with `--format 1` until they update.
- Promoting is one manual workflow run and one approval, per release promoted.
- The tested channel only works with the official release source or a directory that holds the channel file; a server without one, on the tested channel, finds nothing to install and says why.
- Billing and payment stay out of the product; the roadmap keeps them as their own task.
