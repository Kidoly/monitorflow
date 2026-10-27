# Installing the server

How to run the central server with Docker Compose: the server, the web interface behind Caddy, PostgreSQL, VictoriaMetrics and VictoriaLogs on one host (ADR-0051). For agents, see [Installing the agent](deploy-agent.md).

## Requirements

- Docker Engine 23 or later with the compose and buildx plugins (`docker-compose-plugin` and `docker-buildx-plugin` in Docker's packages). The images use BuildKit features.
- 2 vCPU and 4 GB of RAM for a few dozen agents, and disk for metrics and logs, kept 90 and 30 days by default.
- Inbound ports: 443 and 80 for browsers, 7443 (gRPC, mTLS) and 7445 (enrollment) for agents and relays.

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

```
docker compose -f deploy/docker-compose.yml exec server monitorflow-server user add --username admin --role admin
```

The password is read from the terminal. Then sign in at `https://<DOMAIN>/`. Enrollment tokens come from the web interface, or from the same command line:

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

Back up `serverdata` and a dump of the database:

```
docker compose -f deploy/docker-compose.yml exec -T postgres pg_dump -U monitorflow monitorflow | gzip > monitorflow-db.sql.gz
```

## Upgrading

```
git pull
docker compose -f deploy/docker-compose.yml up -d --build --wait
```

The server applies database migrations when it starts.

## Other settings

Every server setting has a `MONITORFLOW_*` variable (see `deploy/config/server.example.toml`). For sections that need files, like `[ai]` with its `api_key_file` or `[[proxmox.clusters]]`, mount your own `server.toml` at `/etc/monitorflow/server.toml` and the files it names in a `docker-compose.override.yml`. The variables the compose file sets still apply on top.

## Limits

- VictoriaMetrics and VictoriaLogs are reached over plain HTTP on an internal Docker network that is never published (ADR-0051). PostgreSQL is reached over TLS.
- The server image trusts the release keys of `deploy/release-keys.pem` at build time; imports of releases signed with another key are refused.
- The server image has no `aptly`, `reposync` or `createrepo_c`, so package mirrors (ADR-0030) are not available with it yet.
