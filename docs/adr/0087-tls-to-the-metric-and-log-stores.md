# ADR-0087: TLS to the metric and log stores

Date: 2026-11-03
Status: accepted; amends ADR-0051 and ADR-0056; implemented in two stages (see the end)

## Context

ADR-0051 left the links from the server to VictoriaMetrics and VictoriaLogs in plain HTTP on the `storage` network, an exception to "no plaintext link" because the traffic never leaves the Docker host. Anyone with access to that host's networking could read every metric and log line, and write fake ones, since the stores asked for no credentials. Their clients trusted public CAs only, so a private certificate could not be verified.

Customers must not have to do anything for this to change, so new installs and existing ones must switch by themselves. An update from the web interface changes images, not the compose file (ADR-0056), and both stores read their TLS settings from command-line flags in that file.

## Decision

### Certificates from the server's CA

The built-in CA (ADR-0004) issues the stores' certificate, through the same `CertificateAuthority` code as every peer certificate. A replaced or rotated CA key therefore applies to the stores too. A new method, `issue_tls_server`, signs a certificate with `serverAuth` only and no `monitorflow://` URI. The CA's own client verifier refuses it, so a stolen store key cannot log in on the agent listeners. Its names are the hosts of `victoriametrics_url` and `victorialogs_url` (`victoriametrics` and `victorialogs` in the compose file), so nothing is hard-coded. One certificate covers both stores: they share a volume, so separate keys would protect nothing.

It lasts 90 days and is issued again with 30 days left, like the server's own certificate (ADR-0007). It is also issued again when it is missing, misses a name, does not match its key, or comes from another CA. These files belong to the server alone, so a restored or replaced CA takes over without a manual step, where `server_cert` refuses a foreign certificate instead.

### Who writes the files, and how the stores pick them up

A new setting, `storage_tls_dir`, names a directory the server shares with the stores. The compose file sets it, so it is not a choice an operator makes. A new volume, `storagetls`, holds `cert.pem`, `key.pem` and `password`.

- **Before the stores start**, a one-shot service, `storage-certs`, runs `monitorflow-server storage-certs`. It uses the server image and its user, has no network, and mounts `serverdata` (the CA) and `storagetls`. Both stores depend on it completing, as PostgreSQL depends on `db-certs`.
- **While the server runs**, it runs the same check every hour and renews the certificate.
- **The stores read the new files without a restart.** Their open source builds re-read `-tlsCertFile` and `-tlsKeyFile` every second (`-help` of both images). The compose smoke test proves it: it removes the certificate, issues a new one, and sees the new serial served within seconds.

Both stores run with `-tls`, the two files, `-tlsMinVersion=TLS13`, and basic auth. The server's clients (`RemoteWrite`, `JsonLines`, `VmQuery`) trust the CA alone, with public roots off, and send the password. With `storage_tls_dir` set, both URLs must be https or the server does not start, so the password never travels in plain text.

### Authentication: basic auth, as mTLS is not open source

Both images list `-httpAuth.username` and `-httpAuth.password`, which can be read from a file (`file:///tls/password`). Neither lists `-mtls`: VictoriaMetrics documents it as an Enterprise feature. So the server authenticates with a password and the stores with their certificate. The password is 32 random bytes in hex, created once by whichever of the one-shot or the server runs first, mode 0600, and kept across renewals. The stores run as root and read it; the server reads it as its own user.

A client certificate would have been one secret fewer. The password stays inside the volume shared by the three containers, so it adds no exposure beyond the key.

### Ports and other clients

Nothing changes on the host: neither store has a published port, and the `storage` network stays `internal`. Only the server talks to the stores. The web interface goes through the API, there is no Grafana, and backups (ADR-0058) hold the database and `serverdata`, not the stores' volumes. The updater recreates the stores for retention (ADR-0069) without reaching them. `storagetls` is not in backups: a restore issues its contents again from the restored CA, with a new password.

`monitorflow-server storage-check` queries both stores as the server does and says whether each answered. The smoke test and the updater use it.

### Existing installs: the compose file follows the release

The updater image now carries the files of `monitorflow-server-compose.tar.gz` (`docker-compose.yml`, `docker/pg_hba.conf`, `restore.sh`, `.env.example`). The image digest is in the signed `images.json`, so the release signature covers them. This reverses one line of ADR-0056 ("the updater changes images, not the compose file"), for the reason ADR-0056 gave as its consequence: a release that needs a new compose file should not need a terminal.

When the updater starts, it checks the last update. If that update put the updater's own release in place at the web interface's request, and the updater has not looked at it yet, it:

1. records that it looked, so this happens once per update;
2. leaves the files alone if they already match, if the directory is a git checkout (it has `docker/server.Dockerfile`; a checkout follows git), or if the project runs a compose file of another name;
3. copies the current files into its own volume (`compose-previous/`), which marks a change under way;
4. writes the release's files, each atomically;
5. runs `docker compose up -d --wait` for every service but itself, with `--pull missing` and no build. Compose recreates what changed: `storage-certs` runs, then the stores and the server start with TLS. Data stays in `vmdata` and `vldata`;
6. runs `storage-check` in the server;
7. reports the outcome as a step of that update, which the update page shows.

The first update to this release goes like this. The previous updater installs the images and replaces itself (ADR-0056, step 8). At that point the new server runs on the old compose file: `storage_tls_dir` is unset and the URLs are http, so it talks to the stores as before. Then the new updater starts and switches the stack to TLS. Between the stores restarting and the server restarting, writes fail and are retried from the server's queues (ADR-0013).

Updates run from the command line are left out: the installer (`update --dir`) and the offline path of ADR-0056. The installer puts the compose files in place itself right after, and a second writer would race its `docker compose up`. The offline operator is already at a terminal and runs the installer or `docker compose up` with the release's files. The updater does not recreate its own container. A change to its own service in the compose file applies when the next update replaces it.

### When the switch fails half way

- **The new files do not start, or the server cannot reach a store.** The updater puts the saved files back, runs `up` again, and the update page says: "warning: the compose file of this release did not work (…); the previous one is back". The stack is back in plain HTTP on the previous file and keeps working, and the next update tries again. No volume is ever removed, so no data is lost.
- **The previous files do not start either.** The page says so too. The data is in the volumes, and running the installer again brings the stack up on the release's files.
- **The updater is killed in the middle**, as by a reboot. `compose-previous/` is still there, so at its next start the updater applies the release's files and checks them again, and puts the previous ones back if that fails, whatever step it had reached.
- **The certificate cannot be issued.** `storage-certs` fails, the stores do not start, the check fails, and the previous files come back. The server itself issues what is missing at start, and does not start if it cannot. It never runs with https URLs and no password.

A known gap: a compose file that needs an image `.env` does not pin cannot be applied by the updater. One example is the mirror image on an install from before ADR-0067, which only the installer pins. `--pull missing` then fails, and the previous file comes back with that reason on the update page. Running the installer again fixes it.

### Development

`deploy/docker-compose.dev.yml` keeps plain HTTP on loopback ports: a developer server leaves `storage_tls_dir` unset and talks to it as before. Making the dev stores use TLS would need the dev server's CA before the containers start, and is left for later.

## Consequences

- No plaintext link remains between the server and its stores. The ADR-0051 exception is gone, and the stores refuse a request without the password.
- New installs and installer runs get TLS from the compose file. Installs updated from the web interface switch at that update, with no step on the host.
- `storagetls` joins the volumes. `--uninstall --purge` removes it with the others.
- `serverdata`, with the CA key, is mounted by one more short-lived container, `storage-certs`. It has no network.
- The updater can now change the compose file, which makes it a bigger part of the deployment to review (ADR-0056). It only writes files its own image carries, which the release signature covers.
- An operator who edited `docker-compose.yml` by hand loses those edits at the next update from the web interface, as with the installer. `docker-compose.override.yml` is the place for local changes. The previous file stays in the updater's volume until the change ends.
- When the CA is rotated, the server issues the stores a new certificate at its next hourly check or start. Its clients trust the CA certificate they loaded at start, so a rotation that replaces that certificate must restart the server. CA key rotation has its own task in Phase 12.

## Stages

1. The certificates, the clients, the compose file and the smoke test. New installs and installer runs get TLS. An install updated from the web interface keeps plain HTTP until the installer runs again, and works as before.
2. The updater puts the release's compose files in place after an update from the web interface, and the update test in CI starts from a compose file without TLS.

### Not done

- Client certificates for the server (`-mtls`) if the open source images ever offer it.
- TLS for the development compose.
- Pinning images a new compose file needs from the updater, as the installer does.
