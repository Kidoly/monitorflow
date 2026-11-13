# ADR-0126: Containers without capabilities, third-party images by digest

Date: 2026-11-12
Status: accepted; amends ADR-0051, ADR-0056, ADR-0068 and ADR-0100

## Context

A security audit (2026-11-09, part 5) found that no service of the compose files (`docker-compose.yml`, `.shared.yml`, `.joined.yml`) limited what its processes may do. Every container had Docker's default capabilities, setuid binaries could raise privileges, and the image layers were writable. The internet-facing `web` service ran Caddy as root, the default of the `caddy` image. A flaw in Caddy or in what it parses would give root in the container, with `CHOWN`, `DAC_OVERRIDE` and the other default capabilities.

PostgreSQL, VictoriaMetrics and VictoriaLogs were named by tag. `images.json`, the signed release file, names our own four images (server, web, updater, mirror) by digest, but nothing pinned these three. Whoever could move a tag on Docker Hub would have their image pulled at the next `docker compose pull` or install, and it would see all the data.

## Decision

### Hardening, on every service

Each service in all three compose files has `security_opt: [no-new-privileges:true]` and `cap_drop: [ALL]`, through the `x-hardening` anchor. A service adds back only what it needs, with a comment explaining why:

| Service | User | Capabilities added | Read-only image, tmpfs |
| --- | --- | --- | --- |
| `db-certs` | root | `CHOWN`: hands the key to PostgreSQL's user | yes |
| `storage-certs`, `db-migrate`, `server` | 10001 | none | yes, `/tmp` for the server |
| `postgres` | root, then 999 through gosu | `CHOWN`, `DAC_OVERRIDE`, `FOWNER`, `SETGID`, `SETUID`, for the entrypoint before gosu | yes, `/var/run/postgresql`, `/tmp` |
| `victoriametrics`, `victorialogs` | root | `DAC_READ_SEARCH`: the key and password are 0600 files owned by the server's user | yes, `/tmp` |
| `web-data` (new, one-shot) | root | `CHOWN`, `DAC_READ_SEARCH` | yes, no network |
| `web` | 10002 | none | yes, `/srv/monitorflow/run`, `/config`, `/tmp` |
| `mirror` | 10001 | none | no, see below |
| `updater` | root | `CHOWN`, `DAC_OVERRIDE`, `FOWNER`: backups and restores of the server's data, `.env` kept with its owner; `FSETID`: without it, root's chmod clears the setgid bit of the backups directory, whose group is the server's, and the server can no longer read new backups | no |

Processes that switch from root to another user lose their capabilities when they switch, so PostgreSQL itself runs with none. The updater holds the Docker socket, which is root on the host anyway. For it, the limits catch mistakes rather than form a boundary, and it keeps a writable image for the Docker CLI.

The mirror tools parse what they download from upstreams, so they would gain the most from a read-only image, but they keep a writable one. Run by a user other than root, dnf ignores `cachedir` and keeps its repository metadata in `/var/tmp`. A tmpfs there would hold tens of megabytes per large repository in memory. The mirror runs as the server's user, without capabilities, and cannot gain privileges.

### Caddy as its own user

`web` runs as uid and gid 10002, named `monitorflow-web` in the image. That is not the server's user, so a Caddy flaw cannot read a file of the server even where a volume would allow it. Ports 80 and 443 stay those of the container: `net.ipv4.ip_unprivileged_port_start=0` is set for its network namespace only, which Docker already does by default since 20.10, and no capability is needed. The ports published on the host do not change.

The user is set in the compose file, not in the image. During an update, the updater recreates `web` from the new release's image while the old compose file is still in place (ADR-0056). It installs the new compose file afterwards (ADR-0087). An image that was non-root by itself would meet root-owned Caddy data and fail at that point. With the old compose file, the new image runs as root, as before.

Caddy's data volume belonged to root on every installed server. The one-shot `web-data` service gives it to 10002 before each start of `web`. A run on a volume that is already right changes nothing. The image makes `/data` and `/config` 10002's, so a new volume starts that way.

The trust page and the fingerprint that `web-start.sh` writes (ADR-0068) move to `/srv/monitorflow/run`, a tmpfs, so the rest of the image can be read-only. The installer reads `/srv/monitorflow/run/ca-fingerprint`. Port 80 still redirects every other path, the old and new places included.

`docker compose cp` cannot write into a read-only container or a tmpfs. The installer, `install-test.sh` and the guide's offline import of an agent release send release files through `tar` and `compose exec` instead.

The installer's root runs that fill the volumes of an added central server (`copy_join_data`, ADR-0100) add `CHOWN`, `DAC_OVERRIDE` and `FOWNER` back for that one command. They write into directories of the services' users and keep the owners of the files.

### Third-party images by digest, in the compose file

`postgres`, `victoriametrics` and `victorialogs` are named `tag@sha256:<index digest>`. Docker pulls the digest and checks the content against it. The tag stays for people and Dependabot, whose `docker-compose` ecosystem already watches `deploy/` and updates the tag and the digest together.

They are not added to `images.json`. The release signature already covers the compose files: they ship in the signed `monitorflow-server-compose.tar.gz`, and the updater installs them from its own image, which `images.json` pins by digest (ADR-0087). A digest in the compose file is therefore signed by every release. A second list would have to agree with the compose file, and the installer, the updater and their tests would have to learn new keys for nothing.

The base images of our Dockerfiles (`caddy`, `debian`, `node`, `rust`, `docker`) stay named by tag. They are read at build time only, and what runs is our own image, pinned by `images.json`.

## Consequences

- A Caddy flaw gives uid 10002 without capabilities, in a read-only image, instead of root.
- A moved tag on Docker Hub changes nothing on installed servers until a release with its digest is published, after a person has merged the Dependabot pull request.
- Installed servers get all of this when they install the first release that has it: the updater puts the new compose file in place, and `web-data` takes over Caddy's volume.
- A service that needs a new capability, or writes outside its volumes, fails at start in the smoke test. CI's compose job checks that every container drops all capabilities and that `web` and `server` run without any.
- Operators who added their own services through an override file are not affected. Operators who changed one of ours may have to add a capability back in that file.
