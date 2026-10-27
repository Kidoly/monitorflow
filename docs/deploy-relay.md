# Installing a relay

A relay forwards the streams of the agents of a site to the central server, buffers their samples while the server is unreachable, and can serve them copies of the package mirrors and of the agent releases (ADR-0025, ADR-0027, ADR-0034). How to install one on a Linux host with systemd, with the installer or by hand; the installer does exactly the steps of [By hand](#by-hand).

## Before you start

- A host with systemd, x86_64 or aarch64, that the site's agents reach on port 7443 (and 7480 if it serves mirrors), and that reaches the server on 7443 and 7445.
- A relay token. Relay tokens name the DNS names or addresses agents use to reach the relay, which go in its certificate; create one on the server:

  ```
  docker compose exec server monitorflow-server token create --role relay --id relay1-paris \
    --site paris --server-name relay1.paris.example
  ```

  Repeat `--server-name` for each name or address. A relay behind another relay adds `--parent-relay <relay id>` (ADR-0028).

## Quick install

Create a relay token under Admin › Enrollment tokens (role Relay, with the names agents use to reach it): the panel shows the command below filled in, from the source chosen there (GitHub, this server or a custom URL), with the mirror URL if you give one. By hand:

```
curl -fsSL https://github.com/Kidoly/monitorflow/releases/latest/download/install.sh \
  | sudo MF_ROLE=relay MF_SERVER=monitorflow.example.com:7443 MF_TOKEN=mfe1.… sh
```

The installer verifies the release signature and checksums before it changes anything (ADR-0052, ADR-0053), installs the static relay for the host's architecture, the user `monitorflow-relay`, its directories, `relay.toml`, the sudo rule of its self-update helper checked with `visudo`, and the unit `monitorflow-relay`. It hands the token to the first start only and waits until the relay has its identity.

| Variable | Meaning |
| --- | --- |
| `MF_ROLE=relay` | Install a relay instead of an agent |
| `MF_SERVER` | `host:port` of the server (or of the parent relay), port 7443 by default; an https URL works too. Required on the first install |
| `MF_TOKEN` | The relay token. Required unless the relay is enrolled |
| `MF_ENROLL` | `host:port` of the server's enrollment listener; by default `MF_SERVER`'s host on port 7445. Set it when `MF_SERVER` is a parent relay |
| `MF_MIRROR_URL` | The http(s) URL the site's hosts reach this relay's mirror listener at, like `http://relay1.paris.example:7480`. Turns on the copy of the mirrors and agent releases ([Mirrors](#mirrors)) |
| `MF_VERSION`, `MF_DOWNLOAD_URL`, `MF_TIMEOUT`, `MF_REQUIRE_SIGNATURE`, `MF_ALLOW_UNSIGNED` | As for the agent ([Installing the agent](deploy-agent.md#quick-install)) |
| `MF_UNINSTALL=1` | Remove the binary, the unit and the sudo rule; `relay.toml` and the identity stay. Add `MF_PURGE=1` to remove them and the user too |

Running the installer again upgrades the relay in place and keeps its configuration and identity. A relay and an agent can share a host: they use different users, files and units.

Agents of the site then connect to the relay and enroll with the server:

```
curl -fsSL https://github.com/Kidoly/monitorflow/releases/latest/download/install.sh \
  | sudo MF_SERVER=relay1.paris.example:7443 MF_ENROLL=monitorflow.example.com:7445 MF_TOKEN=mfe1.… sh
```

## By hand

With the files of a release (`monitorflow-relay-linux-<arch>`, `monitorflow-relay.service`, `monitorflow-relay-sudoers`), checked against `SHA256SUMS` (`sha256sum -c --ignore-missing SHA256SUMS`) once its signature verifies, as [docs/releasing.md](releasing.md) shows:

```
useradd --system --user-group --home-dir /var/lib/monitorflow/relay --no-create-home \
  --shell /usr/sbin/nologin monitorflow-relay
install -d -o monitorflow-relay -g monitorflow-relay -m 0700 /var/lib/monitorflow/relay
install -d -o root -g root -m 0755 /etc/monitorflow
install -o root -g root -m 0755 monitorflow-relay-linux-x86_64 /usr/bin/monitorflow-relay
install -o root -g root -m 0440 monitorflow-relay-sudoers /etc/sudoers.d/monitorflow-relay
visudo -cf /etc/sudoers.d/monitorflow-relay
install -o root -g root -m 0644 monitorflow-relay.service /etc/systemd/system/monitorflow-relay.service
```

`/etc/monitorflow/relay.toml`, owner `root`, group `monitorflow-relay`, mode 0640; every key and its default are in `deploy/config/relay.example.toml`:

```
upstream_url = "https://monitorflow.example.com:7443"
# Only when the enrollment listener is not the upstream's host on 7445:
# enroll_url = "https://monitorflow.example.com:7445"

# Only to serve mirrors:
# [mirror]
# enabled = true
# public_url = "http://relay1.paris.example:7480"
```

Then the first start, with the token in a root-only file that systemd reads and that is deleted once the relay is enrolled (ADR-0007):

```
(umask 077; printf 'MONITORFLOW_ENROLLMENT_TOKEN=%s\n' 'mfe1.…' >/etc/monitorflow/relay-enroll.env)
systemctl daemon-reload
systemctl enable --now monitorflow-relay
# Once /var/lib/monitorflow/relay/tls/cert.pem exists:
rm /etc/monitorflow/relay-enroll.env
```

The relay runs as `monitorflow-relay` and needs root for one thing only: replacing its own binary. `sudo -n /usr/bin/monitorflow-relay self-update` is the one line the sudo rule allows; the helper installs only a release the project's key signed (ADR-0053, ADR-0055). Without the rule, self-updates fail with "a password is required" and nothing changes.

## Mirrors

With `MF_MIRROR_URL` (or `[mirror] enabled = true` and `public_url`), the relay copies the server's package mirrors and agent releases and serves them on port 7480 (ADR-0034, ADR-0037): agents of the site then download from it. A copy of every mirror can take tens of gigabytes under `/var/lib/monitorflow/relay/mirrors`; `mirror.mirrors` limits it to some.

## Self-update

The server updates relays like agents (ADR-0053, ADR-0055): a signed `self_update` command, sent on the relay's own stream, makes the relay download the release, check its signature against the release keys built into it, and hand it to its root helper:

1. `sudo -n /usr/bin/monitorflow-relay self-update` checks the release again as root, writes the new binary next to `/usr/bin/monitorflow-relay`, keeps the running one as `/var/lib/monitorflow/relay-self-update/previous` and renames the new one into place.
2. It starts the watchdog in a transient unit, `monitorflow-relay-self-update`, from the previous binary. The watchdog restarts `monitorflow-relay` and waits for the new relay's stream to be accepted upstream (5 minutes by default).
3. If it is not in time, the watchdog puts the previous binary back and restarts the relay; the command fails with "rolled back to …".

On a relay's page under Admin › Relays, administrators of its customer see its version, its last self-update and "Update relay to …" when the server holds a newer release; the API takes `POST /api/relays/{id}/update` with `{"version": "0.3.0"}`. With automatic updates on (ADR-0055, Updates › Automatic updates), the server updates relays on its own, before the agents behind them: an agent waits while its relay runs an older version.

The outcome is in `/var/lib/monitorflow/relay-self-update/status.json`, the watchdog's log in `journalctl -u monitorflow-relay-self-update`. Agents behind the relay reconnect on their own after the restart, a few seconds later. Relays installed by hand before this guide lack the sudo rule: run the installer once with `MF_ROLE=relay`.

## Checking it

```
systemctl status monitorflow-relay
journalctl -u monitorflow-relay
```

The relay shows online under Admin › Relays, with the agents connected through it.

## Removing it

```
curl -fsSL https://github.com/Kidoly/monitorflow/releases/latest/download/install.sh \
  | sudo MF_ROLE=relay MF_UNINSTALL=1 sh
```

Revoke the relay in the web interface too, so its certificate no longer works.
