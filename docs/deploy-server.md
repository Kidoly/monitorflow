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

Everything lives in `/opt/monitorflow/deploy`: the compose file, and `.env` with the settings and the database password. Commands on this page that start with `docker compose -f deploy/…` or `deploy/restore.sh` run from `/opt/monitorflow`.

- **Without a terminal**, as under automation, it uses `MF_PUBLIC_NAME`, or its guess: the machine's full name if that resolves to a public address, else its IP address. To choose: `curl … | sudo MF_PUBLIC_NAME=monitor.example.com sh`.
- **Run it again at any time.** Settings and secrets stay, the stack comes back up, and a newer release is installed through the updater, with a backup and a rollback. The compose file follows the release. After editing `.env`, run it again too.
- **Moving to a new machine:** `curl … | sudo sh -s -- --restore monitorflow-backup-….tar.gz` installs, then restores the backup. Give the old server's name, so agents find it.
- **Removing it:** `curl … | sudo sh -s -- --uninstall` removes the containers and, on a terminal, asks whether to delete the data. Without a terminal, or if you press Enter, the data stays in Docker volumes and `.env` stays too, and running the installer brings all of it back. `--uninstall --purge` deletes them without asking. Docker stays installed.
- It checks `SHA256SUMS.sig` against the release keys it carries, with OpenSSL 3, and so the compose files and the image digests of `images.json`[^0056]; a machine without OpenSSL 3 gets a warning and checks the checksums only. `MF_VERSION=0.2.0` installs a given release, and `MF_DOWNLOAD_URL` a copy of the release files on your own https server, which then also serves updates.

On the first start the server creates its CA and its own certificate for `SERVER_NAMES` in the `serverdata` volume, and `db-certs` the certificates of the database link in `dbcerts`. A name added to `SERVER_NAMES` later gets a new server certificate at the next start.

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
| `serverdata` | The built-in CA, the server's certificate, the command signing key. Without it every agent and relay must be installed again |
| `mirrors` | Package mirrors and their signing key, and the agent release cache. Tens to hundreds of GB once in use |
| `pgdata` | Inventory, configuration, campaigns, incidents, audit log |
| `vmdata`, `vldata` | Metrics and logs |
| `dbcerts` | Certificates of the database link, created again when the volume is empty |
| `caddydata` | Caddy's certificates and CA. Not in backups: after a restore on a new machine, devices trust the new CA from `/trust/` |
| `backups` | Backups: daily, on request and before each update, the 7 newest kept; `BACKUP_DIR` in `deploy/.env` puts them in a directory of the machine instead |
| `update`, `updater` | Update requests and status, and the updater's own state |

The updater backs up the database and `serverdata` every day and keeps the 7 newest backups[^0058]. **Administration, Backups** shows the last one, takes one now and downloads any of them. A backup holds the CA keys: whoever has the file can impersonate the server, so keep copies somewhere safe. Metrics, logs and the mirror repositories are not in backups; the mirror signing key is.

To restore one, on a fresh install or on this one: `deploy/restore.sh <file>`.

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
   - recreates `server` and `web` and waits until the server is healthy and reports the new version;
   - adds the release's agents to the server's cache, so agents can follow[^0053];
   - replaces itself last.

The update is audited (`server.update`). The page follows it, keeps the last status while the server and the web interface restart, and shows the result; reload the page afterwards to get the new web interface. Agents and relays reconnect by themselves.

A deployment built from a checkout ([below](#from-a-checkout)) updates the same way: the first update switches it to the published images.

The updater changes images only. When a release changes `docker-compose.yml` itself, its notes say so: run the installer again, which updates the file. From a checkout: `git pull`, then `docker compose -f deploy/docker-compose.yml up -d --wait`.

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
# Set DOMAIN, SERVER_NAMES and POSTGRES_PASSWORD (openssl rand -hex 24).
docker compose -f deploy/docker-compose.yml up -d --build --wait
```

The first build compiles the server and the web interface, which takes several minutes. To update, `git pull` and run the last command again.

The server applies database migrations when it starts. Remove the `*_IMAGE` lines from `deploy/.env` first if an update had pinned published images; going back to a build older than the release you ran is a downgrade, which the database refuses.

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

- VictoriaMetrics and VictoriaLogs are reached over plain HTTP on an internal Docker network that is never published[^0051]. PostgreSQL is reached over TLS.
- The server image trusts the release keys of `deploy/release-keys.pem` at build time; imports of releases signed with another key are refused.

[^0051]: [ADR-0051: Production compose and the server's storage links](adr/0051-production-compose.md)
[^0053]: [ADR-0053: Signed releases and agent self-update](adr/0053-agent-self-update.md)
[^0054]: [ADR-0054: Agent install sources](adr/0054-agent-install-sources.md)
[^0056]: [ADR-0056: Updating the central server from the web interface](adr/0056-server-update.md)
[^0058]: [ADR-0058: Daily backups and restore](adr/0058-backups.md)
[^0068]: [ADR-0068: Browser certificate without a terminal](adr/0068-browser-certificate.md)
[^0069]: [ADR-0069: Settings page](adr/0069-settings-page.md)
[^0071]: [ADR-0071: First-run setup in the browser](adr/0071-first-run-setup.md)
