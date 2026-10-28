# Installing the server

How to run the central server with Docker Compose: the server, the web interface behind Caddy, PostgreSQL, VictoriaMetrics and VictoriaLogs on one host (ADR-0051), and the updater that installs new releases from the web interface (ADR-0056). For agents, see [Installing the agent](deploy-agent.md).

## Requirements

- Docker Engine 23 or later with the compose and buildx plugins (`docker-compose-plugin` and `docker-buildx-plugin` in Docker's packages). The images use BuildKit features.
- 2 vCPU and 4 GB of RAM for a few dozen agents, and disk for metrics and logs, kept 90 and 30 days by default.
- Inbound ports: 443 and 80 for browsers, 7443 (gRPC, mTLS) and 7445 (enrollment) for agents and relays.
- For updates from the web interface: outbound HTTPS to `github.com` and `ghcr.io`, or to your own copy of the releases (see [Offline servers](#offline-servers)), and disk for the database dumps taken before each update.
- The `updater` service mounts the Docker socket, `/var/run/docker.sock`, which gives it root on the host. It has no listener and only installs releases signed by the project's key. With rootless Docker, set `DOCKER_SOCKET` in `deploy/.env` to your socket.

## First start

```
git clone https://github.com/Kidoly/monitorflow.git && cd monitorflow
cp deploy/.env.example deploy/.env && chmod 600 deploy/.env
# Set DOMAIN, SERVER_NAMES and POSTGRES_PASSWORD (openssl rand -hex 24).
docker compose -f deploy/docker-compose.yml up -d --build --wait
```

The first build compiles the server and the web interface, which takes several minutes. `--wait` returns once every service is healthy.

On the first start the server creates its CA and its own certificate for `SERVER_NAMES` in the `serverdata` volume, and `db-certs` the certificates of the database link in `dbcerts`. A name added to `SERVER_NAMES` later gets a new server certificate at the next start.

## Administrator and first agent

While no user exists, the server prints a one-time setup link in its log (ADR-0057). Open it to choose the administrator's name and password; you are then signed in, with a site `main` to add servers to. The link works once and for 24 hours. To print a new one:

```
docker compose -f deploy/docker-compose.yml exec server monitorflow-server setup-link
```

The command line still creates users, before or after setup; the password is read from the terminal:

```
docker compose -f deploy/docker-compose.yml exec server monitorflow-server user add --username admin --role admin
```

Enrollment tokens come from the web interface, or from the same command line:

```
docker compose -f deploy/docker-compose.yml exec server monitorflow-server token create --role agent --id web-01 --site main --create-site
```

On a Linux host, install the agent with that token and a name of `SERVER_NAMES`, as in [Quick install](deploy-agent.md#quick-install):

```
curl -fsSL https://github.com/Kidoly/monitorflow/releases/latest/download/install.sh \
  | sudo MF_SERVER=<a name of SERVER_NAMES>:7443 MF_TOKEN=mfe1.… sh
```

A token created in the web interface comes with this command filled in. Hosts without access to GitHub can install from this server itself, once a release is imported ([below](#agent-releases-on-the-server)), or from a copy of the release files on another web server of the site: see [Install sources](deploy-agent.md#install-sources).

## Agent releases on the server

The server serves agent releases to hosts at `https://<DOMAIN>/agent/<version>/` and `https://<DOMAIN>/agent/latest/` (ADR-0054), and to self-updating agents (ADR-0053). It holds only the releases you import. Download every asset of a release from its GitHub page into a directory, on any machine, and copy it to the server host. The import needs no network: it checks the release signature against the keys built into the server, then every file.

```
docker compose -f deploy/docker-compose.yml cp ./monitorflow-0.3.0 server:/tmp/release
docker compose -f deploy/docker-compose.yml exec server monitorflow-server agent-release import --dir /tmp/release
docker compose -f deploy/docker-compose.yml exec server monitorflow-server agent-release list
```

An update of the server from the web interface imports its release this way by itself ([Updating](#updating)).

Then choose "This server" as the default install source on the tokens page, or set `AGENT_INSTALL_SOURCE=central` in `deploy/.env`. Releases stay in the `serverdata` volume.

The web image's Caddy forwards `/agent/*` to the server like `/api/*`. With a reverse proxy of your own, forward both paths to the API listener.

## Browser certificate

With a public DNS name in `DOMAIN`, Caddy gets a certificate by ACME, which needs ports 80 and 443 reachable from the internet. With an IP address or an internal name, Caddy issues one from its own CA. To trust it, import its root certificate in browsers:

```
docker compose -f deploy/docker-compose.yml exec web cat /data/caddy/pki/authorities/local/root.crt > monitorflow-web-ca.crt
```

## Data and backups

Volumes are prefixed with the project name, like `monitorflow_serverdata`.

| Volume | Holds |
| --- | --- |
| `serverdata` | The built-in CA, the server's certificate, the command signing key. Without it every agent and relay must enroll again |
| `pgdata` | Inventory, configuration, campaigns, incidents, audit log |
| `vmdata`, `vldata` | Metrics and logs |
| `dbcerts` | Certificates of the database link, created again when the volume is empty |
| `caddydata` | Caddy's certificates and CA |
| `backups` | Database dumps taken before each update; `BACKUP_DIR` in `deploy/.env` puts them in a host directory instead |
| `update`, `updater` | Update requests and status, and the updater's own state |

Back up `serverdata` and a dump of the database:

```
docker compose -f deploy/docker-compose.yml exec -T postgres pg_dump -U monitorflow monitorflow | gzip > monitorflow-db.sql.gz
```

## Updating

### From the web interface

In **Administration, Server update** (global administrators):

1. **Check for updates.** The server fetches the latest release from GitHub, or from `RELEASE_URL` when set (else `AGENT_DOWNLOAD_URL`), checks its signature and shows the version, the release notes and what changes: the server, the web interface, the updater and the agent release.
2. **Update to …**, then confirm. The updater:
   - verifies the release again with the keys built into it, and refuses anything that is not newer than the running server;
   - pulls the release's images by digest;
   - dumps the database into the `backups` volume (`monitorflow-<from>-to-<to>-<time>.dump`);
   - pins the images in `deploy/.env` (`SERVER_IMAGE`, `WEB_IMAGE`, `UPDATER_IMAGE`);
   - recreates `server` and `web` and waits until the server is healthy and reports the new version;
   - adds the release's agents to the server's cache, so agents can follow (ADR-0053);
   - replaces itself last.

The update is audited (`server.update`). The page follows it, keeps the last status while the server and the web interface restart, and shows the result; reload the page afterwards to get the new web interface. Agents and relays reconnect by themselves.

A deployment built from a checkout updates the same way: the first update switches it to the published images.

The updater changes images only. When a release changes `docker-compose.yml` itself, its notes say so; update the file by hand (`git pull`) and run `docker compose -f deploy/docker-compose.yml up -d --wait`.

### When an update fails

If the new version does not become healthy, the updater puts the previous images back in `deploy/.env` and restarts them. Nothing else is needed when no migration ran: the page says that the previous version runs again with the same data.

Migrations are forward-only: a server refuses a database migrated by a newer version. If the new version had already migrated the database when it failed, the previous version cannot start on it, and the page says so and names the backup. Restore it as below. Data written between the backup and the failure is lost.

### Restoring after a failed update

On the server host, with the dump named on the page:

```
docker compose -f deploy/docker-compose.yml stop server
docker compose -f deploy/docker-compose.yml exec -T postgres dropdb -U monitorflow --force monitorflow
docker compose -f deploy/docker-compose.yml exec -T postgres createdb -U monitorflow monitorflow
docker compose -f deploy/docker-compose.yml exec -T postgres pg_restore -U monitorflow -d monitorflow --no-owner /backups/<dump>
docker compose -f deploy/docker-compose.yml up -d --wait server
```

PostgreSQL mounts the `backups` volume read-only at `/backups` for this. The updater does not delete dumps: remove old ones from the volume, or from `BACKUP_DIR`, yourself.

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

Without the updater, or to run your own builds:

```
git pull
docker compose -f deploy/docker-compose.yml up -d --build --wait
```

The server applies database migrations when it starts. Remove the `*_IMAGE` lines from `deploy/.env` first if an update had pinned published images; going back to a build older than the release you ran is a downgrade, which the database refuses.

## Other settings

Every server setting has a `MONITORFLOW_*` variable (see `deploy/config/server.example.toml`). For sections that need files, like `[ai]` with its `api_key_file` or `[[proxmox.clusters]]`, mount your own `server.toml` at `/etc/monitorflow/server.toml` and the files it names in a `docker-compose.override.yml`. The variables the compose file sets still apply on top.

## Limits

- VictoriaMetrics and VictoriaLogs are reached over plain HTTP on an internal Docker network that is never published (ADR-0051). PostgreSQL is reached over TLS.
- The server image trusts the release keys of `deploy/release-keys.pem` at build time; imports of releases signed with another key are refused.
- The server image has no `aptly`, `reposync` or `createrepo_c`, so package mirrors (ADR-0030) are not available with it yet.
