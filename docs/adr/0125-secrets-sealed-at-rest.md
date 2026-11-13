# ADR-0125: Outbound secrets are sealed at rest

Date: 2026-11-12
Status: accepted; amends ADR-0058, ADR-0060, ADR-0064, ADR-0081, ADR-0100 and ADR-0102; implemented in four stages (see the end)

## Context

A security audit (part 5, web, deploy, CI and integrations) found that the secrets the server keeps to call out are stored in plaintext:

| Column | Holds |
| --- | --- |
| `notification_channels.secret` | SMTP passwords, webhook signing secrets, chat webhook URLs, plugin secrets (ADR-0060, ADR-0077) |
| `user_contacts.secret` | A person's own chat webhook URL (ADR-0081) |
| `integration_endpoints.secret` | Signing secrets of integration webhooks (ADR-0102) |
| `server_settings.ai_api_key` | The AI provider's key (ADR-0064, ADR-0096) |

The API never returns them, but anyone who can read the database gets all of them: a stolen dump, the PostgreSQL volume, a replica, a database administrator, or an SQL injection that reads. Since ADR-0100 an added central server's machine can reach the database as well. Severity is low, as defence in depth: these secrets give access to other systems, not to MonitorFlow, and each one can be changed at its source.

## Decision

### The seal

Each of these values is stored as `mfs1:<key id>:<base64url(nonce | ciphertext | tag)>`.

- **Format 1** is AES-256-GCM from `ring`, which the server already uses for TLS. No new dependency.
- **The nonce** is 96 random bits for each value. The number of values sealed with one key stays tiny next to the 2^32 limit of random GCM nonces.
- **The associated data** is the column's name (`notification_channels.secret`, ...). A value copied into another column does not open, so a key sealed for one purpose cannot be reused for another. It does not bind the row: someone who can write the database can already change where a channel sends, which the seal does not try to prevent (below).
- **The key id** is the first four bytes, in hex, of a SHA-256 of the key with a fixed label. It names the key in values and logs without revealing it, and it is what makes a later rotation possible.

A value that is not in this format, that names a key the server does not have, or that does not open, is refused. It is never read as plaintext.

### The key

The key lives in `<data_dir>/secrets/keys` (`/var/lib/monitorflow/server/secrets/keys` in the compose deployment), never in the database. The directory has mode 0700 and the file mode 0600. The server refuses a key file that group or others can read or write, like the CA key.

The file is text: comment lines, then one base64 key of 32 bytes per line. **The first key seals and the others only open.** Today the server writes one key. A rotation will add a new first line, seal every value again, then drop the old line, with no change of format.

**The first central server makes the key** on its first start, written to a temporary file then linked to its name, so an existing key is never replaced. **An added central server never makes one** (`MONITORFLOW_ADDITIONAL_INSTANCE`). Without the key it does not start, and it says how to get it: run `install-server.sh --add-server` on the first server, then `--join` here. It checks this before it is admitted, so it takes no license place. Two central servers with keys of their own would each seal values the other cannot open.

The keyring is loaded once at startup and kept in the process (`secrets::install`). The loaders that read channels, endpoints and the AI settings use it wherever they are called, as the built-in release keys are used.

### Sealing what is already stored

A new table, `sealed_columns`, lists the columns whose values are all sealed. At every start, the server locks that table. For each column it seals that is not listed yet, it seals every value in the column and adds the column to the list, all in one transaction. A value that already opens with the keyring is left as it is.

The migration that creates the table is additive. The key cannot live in SQL, so the server does the sealing, not the migration. The list lets each stage seal its own columns, and makes sure a value is never sealed twice. A central server serves nothing before its own pass is done, and a second server waits on the lock, then finds the columns listed.

### When a secret does not open

That secret fails alone, never the server. A channel or an endpoint whose secret does not open fails each delivery with the reason ("sealed with key 1a2b3c4d, which this server does not have: restore its secrets key, or enter the secret again"), recorded as its last error like any other failure. A webhook is never sent unsigned and an email never without its password. The AI test answers with the same reason.

Monitoring, alerts and every other channel keep working. Entering the secret again in the web interface seals it with the current key. This is the rule of ADR-0100: a lost key must not stop monitoring.

### Backups, restores and added servers

- **Backups** (ADR-0058) already archive the whole data volume, owners and modes kept. The key is in every backup with the database it opens, and `deploy/restore.sh` puts it back. A backup taken before this ADR has a plaintext database and no key. Once restored, the server makes a key and seals the database, as it does on an upgrade.
- **A backup is still a full secret.** It holds the key next to the values it seals, as it already holds the CA's private key. The seal protects the database and its own dumps, not the backup file, which must be kept like the CA key. Separating them would mean a backup that cannot be restored alone, which ADR-0058 rules out.
- **The join file** (ADR-0100) already carries `server/ca`. It now carries `server/secrets` too, so an added server has the first one's key. The installer refuses to update a central server added before this ADR until it has the key. It says to run `--add-server` on the first server and `--join` with the new file, once, like the database role of ADR-0109. Nothing is changed on that machine before that.

### What this does not protect

Someone who can read the server's data volume or a backup, or who controls the server process, gets the key with the values. Someone who can write the database can redirect a channel (its host, its URL) and so get the secret at the next delivery. The seal stops the most common leak: a database or a dump read without the server's files.

## Consequences

- The four columns hold sealed values only, and nothing else in the database tells what they hold.
- The data volume now holds a second key that matters. Losing it loses the stored secrets, never the monitoring.
- Servers added before this ADR need a new join file once.
- Tests load a random key with `secrets::install_for_tests()`.

## Stages

1. The `secrets` module: the format, the key file, loading it at startup, and an added server refusing to start without it.
2. `sealed_columns`, the pass that seals stored values, and `notification_channels.secret` and `user_contacts.secret`.
3. `integration_endpoints.secret` and `server_settings.ai_api_key`.
4. The join file carries the key, the installer checks added servers, and a test of a backup and restore with sealed values.
