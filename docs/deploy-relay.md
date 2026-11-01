# Installing a relay

A relay forwards the streams of the agents of a site to the central server, buffers their samples while the server is unreachable, and can serve them copies of the package mirrors and of the agent releases[^0025][^0027][^0034].

## Getting started

You need a machine with systemd, x86_64 or aarch64, that the site's agents reach on port 7443 (and 7480 if it serves mirrors), and that reaches the server on 7443 and 7445.

1. Under Administration › Relays, choose **Add relay**, pick the site and paste the command on the relay's machine. The relay appears on the same page, with the addresses it found.
2. Tick the names agents reach it at, then **Confirm names**. Within seconds it accepts agents[^0066].
3. On its page, choose **Add servers behind this relay**: the install command already points at it.

The checkbox in the panel turns on [mirrors](#mirrors). The command works on one machine, for a day.

The rest of this page is reference.

## Quick install

A relay token is the other way, for scripts, a chosen ID or a relay behind another relay: under Add servers › Advanced in the web interface, or on the server:

```
docker compose exec server monitorflow-server token create --role relay --id relay1-paris \
  --site paris --server-name relay1.paris.example
```

Repeat `--server-name` for each name or address; without any, the relay reports its names and waits for confirmation. A relay behind another relay adds `--parent-relay <relay id>`[^0028]. The command is then:

```
curl -fsSL https://github.com/Kidoly/monitorflow/releases/latest/download/install.sh \
  | sudo MF_ROLE=relay MF_SERVER=monitorflow.example.com:7443 MF_TOKEN=mfe1.… sh
```

The installer verifies the release signature and checksums before it changes anything[^0052][^0053], installs the static relay for the machine's architecture, the user `monitorflow-relay`, its directories, `relay.toml`, the sudo rule of its self-update helper checked with `visudo`, and the unit `monitorflow-relay`. It hands the token to the first start only and waits until the relay has its identity.

| Variable | Meaning |
| --- | --- |
| `MF_ROLE=relay` | Install a relay instead of an agent |
| `MF_SERVER` | `host:port` of the server (or of the parent relay), port 7443 by default; an https URL works too. Required on the first install |
| `MF_TOKEN` | The token of the install command. Required on the first install |
| `MF_ENROLL` | `host:port` where the central server takes first connections; by default port 7445 of the `MF_SERVER` machine. Set it when `MF_SERVER` is a parent relay |
| `MF_MIRROR=1` | Turn on the copy of the mirrors and agent releases ([Mirrors](#mirrors)), served at the relay's first name on port 7480 |
| `MF_MIRROR_URL` | The same at another http(s) URL the site's servers reach this relay's mirror listener at, like `http://mirrors.paris.example:7480` |
| `MF_VERSION`, `MF_DOWNLOAD_URL`, `MF_TIMEOUT`, `MF_REQUIRE_SIGNATURE`, `MF_ALLOW_UNSIGNED` | As for the agent ([Installing the agent](deploy-agent.md#quick-install)) |
| `MF_UNINSTALL=1` | Remove the binary, the unit and the sudo rule; `relay.toml` and the identity stay. Add `MF_PURGE=1` to remove them and the user too |

Running the installer again upgrades the relay in place and keeps its configuration and identity. A relay and an agent can share a machine: they use different users, files and units.

Agents of the site then connect through the relay and make their first connection to the central server, which is what the relay's page gives:

```
curl -fsSL https://github.com/Kidoly/monitorflow/releases/latest/download/install.sh \
  | sudo MF_SERVER=relay1.paris.example:7443 MF_ENROLL=monitorflow.example.com:7445 MF_TOKEN=mfe1.… sh
```

## What runs

The relay runs as `monitorflow-relay` and needs root for one thing only: replacing its own binary. `sudo -n /usr/bin/monitorflow-relay self-update` is the one line the sudo rule allows; the helper installs only a release the project's key signed[^0053][^0055]. Without the rule, self-updates fail with "a password is required" and nothing changes.

## Mirrors

With the checkbox of Add relay (`MF_MIRROR=1`, or `[mirror] enabled = true`), the relay copies the server's package mirrors and agent releases and serves them on port 7480[^0034][^0037]: agents of the site then download from it. A copy takes 50 to 400 GiB per distribution under `/var/lib/monitorflow/relay/mirrors`; `mirror.mirrors` limits it to some. Which mirrors the server holds is chosen under Mirrors. Agents reach the relay's copy at its first server name on port 7480 unless `mirror.public_url` says otherwise.

## Self-update

The server updates relays like agents[^0053][^0055]: a signed `self_update` command, sent on the relay's own stream, makes the relay download the release, check its signature against the release keys built into it, and hand it to its root helper:

1. `sudo -n /usr/bin/monitorflow-relay self-update` checks the release again as root, writes the new binary next to `/usr/bin/monitorflow-relay`, keeps the running one as `/var/lib/monitorflow/relay-self-update/previous` and renames the new one into place.
2. It starts the watchdog in a transient unit, `monitorflow-relay-self-update`, from the previous binary. The watchdog restarts `monitorflow-relay` and waits for the new relay's stream to be accepted by the central server (5 minutes by default).
3. If it is not in time, the watchdog puts the previous binary back and restarts the relay; the command fails with "rolled back to …".

On a relay's page under Administration › Relays, administrators of its customer see its version, its last self-update and "Update relay to …" when the server holds a newer release; the API takes `POST /api/relays/{id}/update` with `{"version": "0.3.0"}`. With automatic updates on (Updates › Automatic updates)[^0055], the server updates relays on its own, before the agents behind them: an agent waits while its relay runs an older version.

The outcome is in `/var/lib/monitorflow/relay-self-update/status.json`, the watchdog's log in `journalctl -u monitorflow-relay-self-update`. Agents behind the relay reconnect on their own after the restart, a few seconds later. Relays installed by hand before this guide lack the sudo rule: run the installer once with `MF_ROLE=relay`.

## Checking it

```
systemctl status monitorflow-relay
journalctl -u monitorflow-relay
```

The relay shows online under Administration › Relays, with the agents connected through it.

## Removing it

```
curl -fsSL https://github.com/Kidoly/monitorflow/releases/latest/download/install.sh \
  | sudo MF_ROLE=relay MF_UNINSTALL=1 sh
```

The relay's page in the web interface does this in one go: "Remove" revokes its certificate, hides it from the lists and shows the exact uninstall command. To add it back, uninstall it and install it again with a token for the same name in the same site.

[^0025]: [ADR-0025: Relay forwarding model](adr/0025-relay-forwarding.md)
[^0027]: [ADR-0027: Relay store-and-forward buffer](adr/0027-relay-store-and-forward.md)
[^0028]: [ADR-0028: Renewal through relays and relay chaining](adr/0028-relay-renewal-and-chaining.md)
[^0034]: [ADR-0034: Relay mirror sync](adr/0034-relay-mirror-sync.md)
[^0037]: [ADR-0037: Relay mirror URLs](adr/0037-relay-mirror-urls.md)
[^0052]: [ADR-0052: Agent releases and the Linux installer](adr/0052-agent-releases.md)
[^0053]: [ADR-0053: Signed releases and agent self-update](adr/0053-agent-self-update.md)
[^0055]: [ADR-0055: Automatic agent and relay updates](adr/0055-automatic-updates.md)
[^0066]: [ADR-0066: Relay onboarding without typing](adr/0066-relay-onboarding.md)
