# ADR-0100: Enterprise edition and licensed high availability

Date: 2026-11-07
Status: accepted; the join file carries the server's database role, not the owner's, since ADR-0109, and the secrets key since ADR-0125; the count of central servers stated as honour-based (2026-11-12, see "Not DRM")

## Context

Until now every feature lived in the AGPL core and revenue was meant to come from subscriptions for support and a tested update channel, the Proxmox model (`docs/product.md`). The owner has decided to sell some features as well, open core style like GitLab CE/EE: an **Enterprise edition** on top of the free core. High availability of the central server (Phase 12, ADR-0089) is the first Enterprise feature.

Most of the high-availability work is already public under the AGPL. ADR-0089 made the server correct with several instances on one database: the leader lease, the shared stream registry, revocations synced through the database, and the client side that tries each address of the server's name. It shipped on `main` and the next release will carry it; v0.1.0 and v0.2.0 are published. None of that can be taken back, and this ADR does not try. What is missing is the part that makes a second central server something a customer can run: installing it with one command (`install-server.sh --join`, increment 1 of ADR-0089), letting it in, and supporting it.

Two principles bind the design. As simple as possible to install, run and use: a single server must not get heavier. And one product for a single company and an MSP alike: a company with one server should never see Enterprise wording it has no use for.

## Decision

### What is free and what is Enterprise

Everything that exists today stays AGPL-3.0-or-later and free, and so does every future feature unless an ADR moves it to the Enterprise edition. The Enterprise edition is a short list of features for large or critical installs. Its first and, for now, only item is **running additional central servers on one database**: `install-server.sh --join`, and the server code that admits an additional instance.

What stays AGPL, explicitly:

- Every release already published, and every commit of the repository before this ADR, under the AGPL as published. Nobody loses a right they had.
- The multi-instance core of ADR-0089 (`crates/server/src/cluster.rs` and what it touches): the leader lease, `server_instances`, `peer_streams`, the revocation sync, the clients' address rotation. It stays in the core and keeps being maintained there, because a single server runs it too.
- The web interface, including the Administration › License page, which only shows what the API says.
- The database schema, Enterprise tables included, so a database is the same with or without the Enterprise code, and backups and restores do not depend on the edition.

Anyone may still start a second instance by hand from AGPL code, as ADR-0089's tests do, or write their own join under the AGPL. That is the deal the AGPL makes and we keep it. What we sell is the Enterprise code that does it in one command, checked and supported, and the subscription that comes with it.

### Where the Enterprise code lives

In `ee/` at the root of the repository, the only place with Enterprise code:

- `ee/LICENSE`: the MonitorFlow Enterprise License (below).
- `ee/README.md`: what is in the directory and how it is licensed.
- `ee/license-keys.pem`: the public keys that verify license keys (below).
- `ee/crates/server-ee`, the crate `monitorflow-server-ee`: verifying license keys and admitting additional instances. It depends on `monitorflow-common`, never the reverse, and has no listener of its own.

Everything outside `ee/` is AGPL. A file is Enterprise because of where it is, not because of a header, so a reader never has to guess.

### How the build includes it

`monitorflow-server` has a Cargo feature `enterprise`, on by default, that adds `monitorflow-server-ee`. Official releases and their images are built with it, so there is one download and one image for everyone: a single server runs the Enterprise build without a key and sees no difference. `cargo build -p monitorflow-server --no-default-features` builds a server from AGPL code only: it does everything except admit an additional instance, which it refuses in plain words ("This build of MonitorFlow does not include the Enterprise edition, which adds central servers").

The core calls the Enterprise crate in two places: at startup, when the server is set up as an additional instance, and in the API of the License page. Each call is behind `#[cfg(feature = "enterprise")]` in one small module (`crates/server/src/edition.rs`), so the AGPL build stays readable and tested in CI.

Combining AGPL code with code under another license is possible here because the copyright holder licenses both; the CLA (`CONTRIBUTING.md`) is what lets them do it for contributed code. `ee/README.md` states the permission to combine the core with `ee/` and convey the result, as an additional permission under section 7 of the AGPL. This is to be reviewed by a lawyer with the Enterprise license, before the first Enterprise release.

### The Enterprise license

`ee/LICENSE` is a source-available commercial license written for MonitorFlow in plain terms: the code is public; anyone may read, change, build and test it, and use it for development, testing and evaluation; running it in production needs a valid subscription and its license key; contributions are welcome under the same CLA as the core. It is marked as a draft until a lawyer has reviewed it, before the first Enterprise release.

### License keys

A license key is a file, not a code to type, delivered with the subscription and checked offline: the server never calls home, so it works in networks with no way out, which the core promises (ADR-0096's local models are the same idea).

**What it holds**, as signed JSON:

| Field | Meaning |
| --- | --- |
| `format` | 1. A server refuses a format it does not know, with "this license needs a newer MonitorFlow" |
| `id` | A UUID, to name the license in the audit log and in support |
| `customer` | Who it is for, shown on the License page |
| `edition` | `enterprise` |
| `server_instances` | How many central servers may run on the database, the first one included: 2 allows one additional server |
| `issued`, `expires` | Dates (`YYYY-MM-DD`, UTC). It is valid through the whole day of `expires` |

**Format.** Two PEM-like blocks of text, so it can be pasted as well as uploaded: `MONITORFLOW LICENSE` holds the JSON in base64, `MONITORFLOW LICENSE SIGNATURE` the raw Ed25519 signature of exactly those bytes, as `openssl pkeyutl -sign -rawin` writes it. No canonical JSON is needed, since the signature covers the bytes as they are.

**Signing keys.** A key pair of its own, never the release signing key (ADR-0053): a leaked license key must not sign releases, and the release key must never sit where licenses are made. The public keys are built into the server from `ee/license-keys.pem`, or from the file `MONITORFLOW_LICENSE_KEYS` names at build time, the way release keys are (`crates/common/build.rs`); a file with several keys allows a rotation. The private key never enters the repository, CI or a server: `scripts/license-keypair.sh` makes the key pair on the owner's machine and `scripts/license-sign.sh` signs a license there. Tests make throwaway key pairs. Until the owner adds a public key, a build trusts none and refuses every license, which is safe.

**How it is checked.** The signature against every built-in key, then the format, the edition and the dates. When an administrator installs one, a key that does not verify is refused with the reason, and nothing is stored. The installed key is checked again each time it is used, never trusted because it is in the database.

**Where it lives.** One row in `license`, the key's text as given; installing another replaces it, removing it deletes it. The audit log keeps the history.

### Without a key, or after expiry

The rule: **a missing or expired license only stops adding central servers.** It never stops monitoring, alerts, notifications, updates, remediation or any running instance.

- **The first central server always runs**, with or without a key, before and after expiry. A single server never needs a key and sees no notice; only the License page mentions licenses.
- **An additional central server** is one installed with `install-server.sh --join`, which sets `MONITORFLOW_ADDITIONAL_INSTANCE`. At its first start it needs a valid license with room for one more: the number of central servers already added, plus the first, plus itself, at most `server_instances`. It is then recorded as added (`server_instance_admissions`, by the name of its host) and starts. Without a license, or without room, it does not start, and says why in plain words and how to get a license.
- **Once added, a central server keeps starting**, after an update, a reboot or a crash, even when the license has expired or was removed. Expiry stops growth, not what runs.
- An administrator removes an added server from the License page (or it is replaced), which frees its place.
- **Notices** go to global administrators only, and only once a central server has been added: from 30 days before expiry ("The license expires on 12 March: central servers already added keep running, but no other can be added"), and after expiry or removal. The page says the same. Nothing is shown to anyone else.
- **Audited**: `license.install` (id, customer, server count, expiry), `license.remove`, `server_instance.add` and `server_instance.remove`, with the host's name.

The CA change keeps refusing to start while several central servers run (ADR-0088, ADR-0089), with the same message.

### Joining

`install-server.sh` gains two options, both Enterprise code paths that check the license first:

- On the first server, `--add-server` checks that the license has room, publishes PostgreSQL, VictoriaMetrics and VictoriaLogs on this host's address, each with TLS and a password as today (ADR-0051, ADR-0087), and writes a join file: the certificate authority, the database's CA and password, the stores' password and the public name. The file holds the CA key, so it is written for root only, and the installer says so.
  Publishing the stores is the one exception to ADR-0051's stores without a published port, and only on a first server that ran `--add-server`; the guide asks for a firewall rule that lets only the added servers reach them.
- On the new machine, `--join FILE` installs the server and web services only (`deploy/docker-compose.joined.yml`), pointing at the first host's stores under their usual names, so their certificates verify as they are, and starts with `MONITORFLOW_ADDITIONAL_INSTANCE`. Backups, updates from the web interface and package mirrors stay on the first host; running the installer again on an added server brings it to the release it is given.

Without a valid license both refuse before sharing or installing anything: "Adding a central server needs a MonitorFlow Enterprise subscription. Install its license key under Administration › License; to get one, see https://github.com/Kidoly/monitorflow#licence." The details belong to the increment that adds them.

### Not DRM

The fence is the license of the Enterprise code plus a license key, not technical protection. The code is public; anyone can find the check and remove it, or build without the feature. Doing so in production breaches the Enterprise license, as using any commercial software without paying does. We do not obfuscate, call home, watermark, count servers behind the customer's back, or degrade anything that runs. Customers pay for a supported feature from the people who make it; the key states what they bought and lets the product say plainly when it is not covered.

The count of central servers is honour-based too, and the code says so rather than pretend otherwise. Two things it trusts, by design:

- **The flag.** Only a server started with `MONITORFLOW_ADDITIONAL_INSTANCE`, which `install-server.sh --join` sets, asks for a place. One started without it runs as a first server and is never counted, as anyone starting a second instance by hand from AGPL code may (above).
- **The host's name.** Places are recorded by the name of the host (`HOSTNAME`, or the kernel's). Several servers under one name take one place.

Enforcing the count in the database (a persistent identity per instance, live instances counted at every start) was considered after the security audit of November 2026 and refused: the check would still be one line to delete in public code, the AGPL build could not run it, and it would let a first server refuse to start after a restore or a move while the old host is still up, which breaks the rule that the first central server always runs. Running more central servers than the license allows breaches the Enterprise license, whichever way it is done; `SECURITY.md` lists it as out of scope.

## Consequences

- `docs/product.md` changes its business model from "everything in the AGPL core" to open core: the AGPL core, the Enterprise edition (high availability first), and the subscriptions around both.
- Phase 12's high-availability item moves to Phase 14, as "Enterprise edition: licensed high availability".
- Increment 1 of ADR-0089 (`--join`) is Enterprise code; increments 2 to 6 are decided when they come, each saying whether it is core or Enterprise.
- A new key pair to keep safe, on the owner's machine, apart from the release key.
- Contributors can read and change the Enterprise code, under its license and the same CLA.
- Before the first Enterprise release: the lawyer's review of `ee/LICENSE` and of the permission to combine, the owner's public license key in `ee/license-keys.pem`, prices, and the page that sells subscriptions.
