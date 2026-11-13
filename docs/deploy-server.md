# Installing the server

## Getting started

1. On a Linux machine, as root:

   ```
   curl -fsSL https://github.com/Kidoly/monitorflow/releases/latest/download/install-server.sh | sudo sh
   ```

   It installs Docker if it is missing, asks for the name browsers and agents use (and suggests one), generates the secrets, starts everything, and prints the address and a one-time link. Nothing to clone, build or edit.

2. Open the link and choose the administrator's name and password. You land on a site `main`, ready for servers.

3. Under Administration › Add servers, copy the install command and paste it on each server to monitor. They appear in the fleet within seconds.

Everything else happens in the web interface: Administration › Settings gathers what can be changed, and each page says what it needs.

The rest of this page is reference. The server runs with Docker Compose: the server, the web interface behind Caddy, PostgreSQL, VictoriaMetrics and VictoriaLogs on one machine[^0051], and the updater that installs new releases from the web interface[^0056]. For agents, see [Installing the agent](deploy-agent.md).

## Requirements

- 2 vCPU and 4 GB of RAM for a few dozen agents, and disk for metrics and logs, kept 90 and 30 days by default (Administration › Settings changes that).
- Inbound ports: 443 and 80 for browsers, 7443 and 7445 for agents and relays (7445 for their first connection), 7480 (package mirrors) for servers that update from the central server. The installer stops and names a port that is taken.
- For updates from the web interface: outbound HTTPS to `github.com` and `ghcr.io`, or to your own copy of the releases (see [Offline servers](#offline-servers)), and disk for the backups taken before each update.
- The `updater` service mounts the Docker socket, `/var/run/docker.sock`, which gives it root on the machine. It has no listener and only installs releases signed by the project's key. With rootless Docker, set `DOCKER_SOCKET` in `deploy/.env` to your socket.

The installer installs Docker with [Docker's official script](https://get.docker.com), which covers Debian, Ubuntu, Fedora, RHEL, CentOS and Rocky. On another distribution, install Docker Engine 23 or later and its compose plugin first, then run the installer.

## The installer

Everything lives in `/opt/monitorflow/deploy`: the compose file, and `.env` with the settings and the two database passwords. Commands on this page that start with `docker compose -f deploy/…` or `deploy/restore.sh` run from `/opt/monitorflow`.

- **Without a terminal**, as under automation, it uses `MF_PUBLIC_NAME`, or its guess: the machine's full name if that resolves to a public address, else its IP address. To choose: `curl … | sudo MF_PUBLIC_NAME=monitor.example.com sh`.
- **Run it again at any time.** Settings and secrets stay, the stack comes back up, and a newer release is installed through the updater, with a backup and a rollback. The compose file follows the release. After editing `.env`, run it again too.
- **Moving to a new machine:** `curl … | sudo sh -s -- --restore monitorflow-backup-….tar.gz` installs, then restores the backup. Give the old server's name, so agents find it.
- **Removing it:** `curl … | sudo sh -s -- --uninstall` removes the containers and, on a terminal, asks whether to delete the data. Without a terminal, or if you press Enter, the data stays in Docker volumes and `.env` stays too, and running the installer brings all of it back. `--uninstall --purge` deletes them without asking. Docker stays installed.
- It checks `SHA256SUMS.sig` against the release keys it carries, with OpenSSL 3, and so the compose files and the image digests of `images.json`[^0056]; a machine without OpenSSL 3 gets a warning and checks the checksums only. `MF_VERSION=0.2.0` installs a given release, and `MF_DOWNLOAD_URL` a copy of the release files on your own https server, which then also serves updates.

On the first start the server creates its CA and its own certificate for `SERVER_NAMES` in the `serverdata` volume, and `db-certs` the certificates of the database link in `dbcerts`. `storage-certs` has that CA issue the certificate of VictoriaMetrics and VictoriaLogs, and creates their password, in `storagetls`; the server renews the certificate by itself[^0087]. A name added to `SERVER_NAMES` later gets a new server certificate at the next start.

## Setup link

The installer prints the one-time setup link, and so does the server in its log while no user exists[^0071]. It works once and for 24 hours. To print a new one, run the installer again or, from `/opt/monitorflow`:

```
docker compose -f deploy/docker-compose.yml exec server monitorflow-server setup-link
```

The same command line creates users and single-use tokens, for scripts: `monitorflow-server user add` and `token create`, with `--help`.

## Agent releases on the server

The server serves agent releases to servers at `https://<DOMAIN>/agent/<version>/` and `https://<DOMAIN>/agent/latest/`[^0054], and to self-updating agents[^0053]. It holds only the releases you import. In the web interface, open Administration › Server update, then choose every asset of a release downloaded from its GitHub page, or give the URL of a directory holding them on a web server of your site. The import checks the release signature against the keys built into the server, then every file, and needs no Internet access.

From a shell, for scripts, copy the files to the central server first:

```
docker compose -f deploy/docker-compose.yml cp ./monitorflow-0.3.0 server:/tmp/release
docker compose -f deploy/docker-compose.yml exec server monitorflow-server agent-release import --dir /tmp/release
docker compose -f deploy/docker-compose.yml exec server monitorflow-server agent-release list
```

An update of the server from the web interface imports its release this way by itself ([Updating](#updating)).

Then choose "This server" as the default install source under Administration › Settings, or set `AGENT_INSTALL_SOURCE=central` in `deploy/.env`. Releases stay in the `mirrors` volume.

The web image's Caddy forwards `/agent/*` to the server like `/api/*`. With a reverse proxy of your own, forward both paths to the API listener.

## Browser certificate

The installer chooses from the name, says which it chose, and needs nothing from you[^0068]:

- **A name that resolves to a public address** gets a public certificate by ACME, which needs ports 80 and 443 reachable from the internet. If no certificate comes within two minutes, the installer says so and uses the server's own CA for now. Open the ports and run it again.
- **An IP address, an internal name like `.lan`, or a name with no public address** gets a certificate from the server's own CA. Open `http://<name>/trust/` once on each device: the page has the certificate and the steps for each system. It arrives unencrypted, so check that it shows the fingerprint the installer printed.

The choice is `WEB_CERTIFICATE` in `deploy/.env`. Running the installer again chooses again, so a name that gets public DNS later moves to ACME. Port 80 serves the ACME challenge and that page, and sends everything else to HTTPS.

## Data and backups

Volumes are prefixed with the project name, like `monitorflow_serverdata`.

| Volume | Holds |
| --- | --- |
| `serverdata` | The built-in CA, the server's certificate, the command signing key, the secrets key. Without it every agent and relay must be installed again, and the passwords and webhook URLs of notifications, integrations and AI entered again |
| `mirrors` | Package mirrors and their signing key, and the agent release cache. Tens to hundreds of GB once in use |
| `pgdata` | Inventory, configuration, campaigns, incidents, audit log |
| `vmdata`, `vldata` | Metrics and logs |
| `dbcerts` | Certificates of the database link, created again when the volume is empty |
| `storagetls` | Certificate, key and password of the links to VictoriaMetrics and VictoriaLogs, created again when the volume is empty |
| `caddydata` | Caddy's certificates and CA. Not in backups: after a restore on a new machine, devices trust the new CA from `/trust/` |
| `backups` | Backups: daily, on request and before each update, the 7 newest kept; `BACKUP_DIR` in `deploy/.env` puts them in a directory of the machine instead |
| `update`, `updater` | Update requests and status, and the updater's own state |

The updater backs up the database and `serverdata` every day and keeps the 7 newest backups[^0058]. **Administration, Backups** shows the last one, takes one now and downloads any of them. A backup holds the CA keys and the secrets key, which opens the passwords, webhook URLs, signing secrets and AI key stored in the database[^0125]: whoever has the file can impersonate the server and reach those services, so keep copies somewhere safe. A copy of the database alone does not reveal them. Metrics, logs and the mirror repositories are not in backups; the mirror signing key is.

To restore one, on a fresh install or on this one: `deploy/restore.sh <file>`. The server's database role and its rights are not taken from the backup: they are created again when the server starts, as at every start[^0109]. A backup taken before the certificate authority changed brings the old one back, and the restore says so: servers and relays that moved cannot connect to it.

The certificate authority in `serverdata` lasts 10 years. **Administration, Certificate authority** changes it in one click, and the server starts a change by itself six months before it expires; every server and relay moves to the new one over its own connection, with nothing to do on them[^0088]. Use **Emergency change** if its key may have leaked. Install commands made before a change stop working after it: copy new ones. With several server instances, stop all but one before changing it, then copy `serverdata/ca` to the others.

## Updating

### From the web interface

The server looks for a new release once a day. When it finds one, global administrators see a line above every page that leads to this update, and channels that cover everything get one notification. On a server without Internet access nothing happens and nothing complains.

In **Administration, Server update** (global administrators):

1. **Check for updates.** The server fetches the latest release from GitHub, or from `RELEASE_URL` when set (else `AGENT_DOWNLOAD_URL`), checks its signature and shows the version, the release notes and what changes: the server, the web interface, the updater and the agent release.
2. **Update to …**, then confirm. The updater:
   - verifies the release again with the keys built into it, and refuses anything that is not newer than the running server;
   - pulls the release's images by digest;
   - takes a backup (`monitorflow-backup-<time>-update.tar.gz`), as on the Backups page;
   - pins the images in `deploy/.env` (`SERVER_IMAGE`, `WEB_IMAGE`, `UPDATER_IMAGE`);
   - applies database migrations with the new version, as the role that owns the database (the `db-migrate` service)[^0109];
   - recreates `server` and `web` and waits until the server is healthy and reports the new version;
   - adds the release's agents to the server's cache, so agents can follow[^0053];
   - replaces itself last;
   - when the release changes `docker-compose.yml`, the new updater puts it in place, restarts what it changes, and checks that the server reaches its metrics and logs. If that fails, it puts the previous file back, and the page says so[^0087].

The update is audited (`server.update`). The page follows it, keeps the last status while the server and the web interface restart, and shows the result; reload the page afterwards to get the new web interface. Agents and relays reconnect by themselves.

A deployment built from a checkout ([below](#from-a-checkout)) updates the same way: the first update switches it to the published images.

The compose file follows the release, data kept; local changes belong in `docker-compose.override.yml`. A checkout keeps its own: `git pull`, then `docker compose -f deploy/docker-compose.yml up -d --wait`.

### The server's database role

The server runs on `monitorflow_app`, a PostgreSQL role that owns no table and is not a superuser: it can add to the audit log and the remediation history and read them, not change or delete them, nor switch off what protects them[^0109]. `monitorflow`, the role that owns the database, is used by the one-shot `db-migrate` service before each start, and by backups and restores. `deploy/.env` holds a password for each: `POSTGRES_PASSWORD` and `POSTGRES_APP_PASSWORD`. To change the second, edit it and run the installer again: the role follows.

A server installed before this gets there by itself. The first update from the web interface ends with the updater adding `POSTGRES_APP_PASSWORD` to `deploy/.env` and putting the new compose file in place, which creates the role and restarts the server on it; the installer does the same when run again. If that last step fails, the updater puts the previous compose file back and the server keeps running as before, on the owner role, with a warning in its log at each start. A checkout needs the line added by hand: compose stops with "set POSTGRES_APP_PASSWORD in .env" until then. Added central servers need a new join file, once: see [Adding a central server](#adding-a-central-server).

### When an update fails

If the new version does not become healthy, the updater puts the previous images back in `deploy/.env` and restarts them. Nothing else is needed when no migration ran: the page says that the previous version runs again with the same data.

Migrations are forward-only: a server refuses a database migrated by a newer version. If the new version had already migrated the database when it failed, the previous version cannot start on it, and the page says so and names the backup. Restore it as below. Data written between the backup and the failure is lost.

### Restoring after a failed update

Download the backup named on the page from **Administration, Backups**, then on the central server: `deploy/restore.sh <file>`. That backup is kept until the next update, whatever its age.

### Offline servers

Without Internet access, Check for updates says that no update can be checked. Update by hand with the same updater, which still verifies the release, backs up the database and rolls back:

1. On a machine with access, download the release files into a directory: at least `SHA256SUMS`, `SHA256SUMS.sig`, `VERSION` and `images.json`, and the agent files for the agent cache (`https://github.com/Kidoly/monitorflow/releases/download/v<version>/<name>`).
2. Pull each image of `images.json` for the server's platform and save them:
   ```
   for ref in $(jq -r '.images[]' images.json); do docker pull --platform linux/amd64 "$ref"; done
   docker save --platform linux/amd64 -o monitorflow-images.tar $(jq -r '.images[]' images.json)
   ```
3. Copy the directory and the archive to the server, load the images, and run the update:
   ```
   docker load -i monitorflow-images.tar
   docker compose -f deploy/docker-compose.yml run --rm -v "$PWD/release:/release:ro" updater update --dir /release
   ```

Loaded images keep their digests only with Docker's containerd image store, the default of new installations since Docker Engine 29 (`docker info -f '{{.DriverStatus}}'` lists `io.containerd.snapshotter.v1`). With the older store the updater cannot find them by digest and tries to pull them, which fails offline; the update then stops before changing anything.

To serve releases to several offline servers, copy the release files to a web server of the site and set `RELEASE_URL` in `deploy/.env` (http or https: the signature protects the files). The images still come from the archive.

### From a checkout

To run your own builds instead of the published images, you need Docker with the compose and buildx plugins (the images use BuildKit features):

```
git clone https://github.com/Kidoly/monitorflow.git && cd monitorflow
cp deploy/.env.example deploy/.env && chmod 600 deploy/.env
# Set DOMAIN, SERVER_NAMES, POSTGRES_PASSWORD and POSTGRES_APP_PASSWORD
# (openssl rand -hex 24, once for each).
docker compose -f deploy/docker-compose.yml up -d --build --wait
```

The first build compiles the server and the web interface, which takes several minutes. To update, `git pull` and run the last command again.

The `db-migrate` service applies database migrations before the server starts. Remove the `*_IMAGE` lines from `deploy/.env` first if an update had pinned published images; going back to a build older than the release you ran is a downgrade, which the database refuses.

## Adding a central server

*Enterprise edition (ADR-0100).* A second central server on the same database keeps MonitorFlow working when one machine stops: agents, relays and browsers reach either under the same name, and alerts, notifications and updates go on from the other (ADR-0089). A single central server needs none of this, and no license key.

1. Install the license key of your Enterprise subscription under Administration › License. It says how many central servers it allows, the first one included.
2. On the first central server:

   ```sh
   curl -fsSL https://github.com/Kidoly/monitorflow/releases/latest/download/install-server.sh | sudo sh -s -- --add-server
   ```

   It checks that the license has room, publishes PostgreSQL, VictoriaMetrics and VictoriaLogs on this machine's address (ports 5432, 8428 and 9428, each with TLS and a password; `MF_SHARE_ADDRESS` picks another address), and writes `/root/monitorflow-join.tar`. That file holds the keys of the certificate authority, the secrets key and the password of the server's database role, not the database owner's: copy it to the new machine over SSH, and delete both copies once done. Allow only the added central servers to reach those three ports in your firewall.
3. On the new machine, with the file in the current directory:

   ```sh
   curl -fsSL https://github.com/Kidoly/monitorflow/releases/latest/download/install-server.sh | sudo sh -s -- --join monitorflow-join.tar
   ```

   It runs the server and the web interface only, on the first machine's database and stores. It never changes the database's schema: the first central server does, which is why that one is updated first. The server starts only if the license has a place for it; otherwise both commands say why and change nothing.
4. Add the new machine's address to the public name in your DNS, so the name has one address per central server. Agents and relays try each address; nothing changes on them.

Once added, a central server keeps running and restarting even after the license expires or is removed: expiry only stops adding others, and administrators see a notice. Administration › License lists the central servers added; removing one there frees its place for another.

A central server added before the server had a database role of its own[^0109] was given the database owner's password, which PostgreSQL no longer accepts from another machine. After updating the first central server, run `--add-server` on it again, copy the new file to each added machine and run `--join monitorflow-join.tar` there once. Until then the added central server cannot connect to the database, and the installer run on it with no option says so and changes nothing. Then change `POSTGRES_PASSWORD` on the first central server, since the old one was on those machines: `docker compose -f deploy/docker-compose.yml exec postgres psql -U monitorflow -c "ALTER ROLE monitorflow PASSWORD '<new>'"`, the same value in `deploy/.env`, and the installer again.

A central server added before the stored secrets were sealed[^0125] does not have the secrets key, and never makes one of its own. After updating the first central server, run `--add-server` on it again, copy the new file to each added machine and run `--join monitorflow-join.tar` there once. Until then the installer run on it with no option says so and changes nothing.

What stays on the first central server: backups, updates from the web interface, package mirrors and the metric and log stores. After updating it, run the installer again on each added central server, with no option, to bring it to the same release. Changing the certificate authority refuses while several central servers run: stop the added ones, change it, then on each run the installer with `--uninstall --purge`, and `--join` with a new join file.

## Other settings

Administration › Settings changes these with no file and no restart, each change in the audit log. Each page's default suits most servers.

| Setting | Default | Locks it in `deploy/.env` |
| --- | --- | --- |
| Metrics kept | 90 days | `METRICS_RETENTION` |
| Logs kept | 30 days | `LOGS_RETENTION` |
| Sign out after | 12 hours | `SESSION_LIFETIME`, in seconds |
| Sign out when idle | 30 minutes | `SESSION_IDLE_TIMEOUT`, in seconds |
| Package address | `http://DOMAIN:MIRROR_PORT` | `MIRROR_PUBLIC_URL` |
| Install source | GitHub | `AGENT_INSTALL_SOURCE`, `AGENT_DOWNLOAD_URL` |

A locked value shows as locked, with the line to remove[^0069]. After editing `.env`, run the installer again. Retention takes effect by restarting VictoriaMetrics and VictoriaLogs for a few seconds, through the updater; a shorter value deletes older data.

Every other server setting has a `MONITORFLOW_*` variable (see `deploy/config/server.example.toml`). For sections that need files, like `[ai]` with its `api_key_file`, which then takes precedence over the web interface, or `[[proxmox.clusters]]`, mount your own `server.toml` at `/etc/monitorflow/server.toml` and the files it names in a `docker-compose.override.yml`. The variables the compose file sets still apply on top.

## Limits

- VictoriaMetrics and VictoriaLogs are reached over TLS with a certificate of the built-in CA, and a password[^0087], on an internal Docker network that is never published[^0051]. PostgreSQL is reached over TLS, by a role that cannot change the schema or rewrite the audit log[^0109]; its owner role is accepted only from the Docker networks of the database container.
- The API listens in plain HTTP on the `front` network, reached by the web service only; its `X-Forwarded-For` is believed from that service alone, so a local user who reaches the API directly is counted as their own address by the sign-in limits[^0110].
- The server image trusts the release keys of `deploy/release-keys.pem` at build time; imports of releases signed with another key are refused.
- Whoever has root on the machine, the Docker socket or `deploy/.env` has the database owner's rights and can rewrite the audit log. The server's database role only bounds what the server process and its SQL can do.

[^0051]: [ADR-0051: Production compose and the server's storage links](adr/0051-production-compose.md)
[^0053]: [ADR-0053: Signed releases and agent self-update](adr/0053-agent-self-update.md)
[^0087]: [ADR-0087: TLS to the metric and log stores](adr/0087-tls-to-the-metric-and-log-stores.md)
[^0054]: [ADR-0054: Agent install sources](adr/0054-agent-install-sources.md)
[^0056]: [ADR-0056: Updating the central server from the web interface](adr/0056-server-update.md)
[^0058]: [ADR-0058: Daily backups and restore](adr/0058-backups.md)
[^0068]: [ADR-0068: Browser certificate without a terminal](adr/0068-browser-certificate.md)
[^0069]: [ADR-0069: Settings page](adr/0069-settings-page.md)
[^0071]: [ADR-0071: First-run setup in the browser](adr/0071-first-run-setup.md)
[^0088]: [ADR-0088: Changing the certificate authority](adr/0088-changing-the-certificate-authority.md)
[^0109]: [ADR-0109: The server runs on a database role that owns nothing](adr/0109-database-roles.md)
[^0110]: [ADR-0110: Trusted proxies for the client's address](adr/0110-trusted-proxies.md)
[^0125]: [ADR-0125: Outbound secrets are sealed at rest](adr/0125-secrets-sealed-at-rest.md)
