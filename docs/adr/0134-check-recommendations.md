# ADR-0134: Check recommendations from what the agent sees

Date: 2026-11-14
Status: accepted

## Context

On a large fleet, the checks a server needs follow from what it runs: a database server needs its database watched, a web server its ports. People add those checks by hand, server by server, and forget some. A discovery that runs once when a machine is added is soon wrong: software is installed and removed, ports move, shares are mounted.

The agent already sees what runs on its machine. The server knows which checks a server has. Between the two, the server can say "PostgreSQL is running here and nothing checks it". The design has to keep the agent cheap at 1,000 servers, treat what an agent says as untrusted (architecture §4: a compromised machine or relay can say anything about its own site), and never change what is monitored without a person.

## Decision

**What the agent reports.** A Linux agent looks every 5 minutes: one `systemctl list-units --type=service --state=running` (spawned as ADR-0021 spawns `systemctl show`: no shell, `LC_ALL=C`, killed on timeout), `/proc/net/tcp` and `tcp6` for listening TCP ports, and `/proc/self/mounts` for the file systems that hold data. Per port it says whether every socket on it is on loopback, and whether one is on a loopback or wildcard address, so a check on 127.0.0.1 reaches it. Per network share it gives the file server's host. Databases and containers are not listed apart: they show as services and ports (`postgresql@16-main.service`, 5432; `docker.service`), and listing containers would need the Docker socket, which the agent's user does not have. A Windows agent reports nothing yet.

**When.** The agent sends a `DiscoveryReport`, a new variant of the envelope, when what it saw differs from what it last sent, and at least every hour, so "checked 12 min ago" on the server's page means something. A report waiting while disconnected goes on the next connection. Nothing rides on the sample path: the cost is one process every 5 minutes and a few kilobytes an hour. A relay forwards the report like any envelope; one older than it drops it, and its servers show no recommendations until it is updated.

**What the server keeps.** Each report is bounded before anything else: at most 256 services, 128 ports and 64 mounts; unit names in systemd's charset ending in `.service`; ports 1 to 65535, once each; absolute paths of at most 256 bytes without control characters; file system types of `[a-z0-9._]`; share hosts that a port check would accept as an address. What does not fit is dropped and counted. The server keeps the latest report per server in `server_discovery` (the bounded lists as `jsonb`, when it came, how many entries were dropped), replaced each time. It stores at most one report a minute per stream, so an agent that floods costs a comparison, not a write. Only agents report: a relay's report about itself is ignored. Removing a server removes its row.

**Recommendations are computed when asked, not stored.** `GET /api/servers/{id}/recommendations` applies a table of rules in server code to the latest report. Each rule names software (PostgreSQL, MySQL or MariaDB, Redis, MongoDB, nginx, Apache, SSH, Docker, Postfix, Proxmox VE, and others) by the service names and well-known ports it runs, and proposes checks of existing kinds: the running service (the unit as reported, so `postgresql@16-main.service`) and its port on 127.0.0.1 when the agent says a local check reaches it. A network share gets a check that its file server answers on 2049 (NFS) or 445 (SMB). A proposed check that the server's effective configuration already has (same kind, same unit, same port on a local address) is left out, and so is a recommendation with nothing left. Computing on read means a recommendation follows the machine within 5 minutes and disappears as soon as someone adds the check, with no table to keep in step.

The rules are in server code, not in a file administrators edit: each one is a few lines and a test, and new software is added for everyone in a release. Ports alone recommend only for software whose port is unambiguous; unknown listening ports recommend nothing, since programs listen on many ports nobody needs to watch.

**A person adds every check.** A recommendation only proposes. "Add" posts the proposed checks through the existing routes for checks and alerts (ADR-0016, ADR-0019, ADR-0057), with the same validation, roles and audit as adding them by hand, at the server's layer. Nothing on the server writes a check by itself, and the agent cannot make it: what it reports is turned into proposals of existing kinds with values checked as above, and the API checks them again when a person adds them. Text in a recommendation that came from the agent (a unit name, a host) is shown as text.

**Dismiss.** Operators of the site dismiss a recommendation for a server, for good: `POST /api/servers/{id}/recommendations/dismiss` with `{"key"}` writes `check_recommendation_dismissals` (server, key, who, when), and `POST …/restore` brings it back. The key names the rule and what it is about (`postgresql`, `share:/mnt/backup`), so a dismissal survives the service restarting under another unit name; it goes in the body because a share's key holds a path. Only a key the server recommends now can be dismissed, so the table holds keys the server made, never text from a request. Both are audited (`check_recommendation.dismiss`, `.restore`). A dismissed recommendation is listed under "Show dismissed".

**On screen.** The server's page lists the recommendations above its checks: "PostgreSQL is running", what the agent saw ("postgresql@16-main.service is running, port 5432 is listening"), the checks that Add creates with their alerts, and when the agent last reported. Without a report it says why: not reported yet, or a Windows agent. Readers see them; the buttons are for operators.

## Consequences

- Recommendations follow the machine: software installed today is recommended within 5 minutes, and a check added by hand makes its recommendation disappear.
- An agent costs one `systemctl` every 5 minutes and a report an hour; the server one small write per changed report.
- A compromised agent can make up recommendations for its own server, which a person still has to add. It cannot reach another server's row: the report is stored under the stream's verified identity.
- The service list of each server is stored on the central server. It is the kind of inventory the server already holds (packages, OS), and is shown only to those who read the server.
- Windows agents, UDP services, listing containers and recommendations across a site ("12 servers run nginx without a check") are follow-ups.
