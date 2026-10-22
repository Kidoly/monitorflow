# Installing the agent

How to install the agent on a Debian-family or RHEL-family server by hand, until there are packages. The packages will do the same steps. Background is in architecture §4 (least privilege) and §7 (packages), and in ADR-0031, ADR-0032 and ADR-0035.

## Binary and user

The agent runs as the unprivileged system user `monitorflow`. Sudo runs it as root only for the source helper, and the sudoers rules name it by its full path, so it must be installed at exactly `/usr/bin/monitorflow-agent`, owned by root and writable by no one else:

```
install -o root -g root -m 0755 monitorflow-agent /usr/bin/monitorflow-agent
useradd --system --home-dir /var/lib/monitorflow/agent --shell /usr/sbin/nologin monitorflow
install -d -o monitorflow -g monitorflow -m 0700 /var/lib/monitorflow/agent
install -d -o root -g root -m 0755 /etc/monitorflow
install -o root -g monitorflow -m 0640 agent.toml /etc/monitorflow/agent.toml
```

Start from `deploy/config/agent.example.toml`. `data_dir` belongs to the agent user. The source backup lives in `/var/lib/monitorflow/sources-backup/` and belongs to root with mode 0700 (ADR-0032). The helper creates it on its first apply and refuses it when root does not own it, so do not create it as `monitorflow`.

Run the agent as `monitorflow`, for example with a systemd unit:

```
[Service]
User=monitorflow
ExecStart=/usr/bin/monitorflow-agent --config /etc/monitorflow/agent.toml
Restart=on-failure
```

On the first start, pass the enrollment token with `MONITORFLOW_ENROLLMENT_TOKEN` so it never stays on disk (ADR-0007).

## Sudo rules

Package updates and source configuration need root. The agent runs them with `sudo -n` and fixed command lines, and `deploy/sudoers/monitorflow-agent` allows those exact lines and nothing else:

```
install -o root -g root -m 0440 deploy/sudoers/monitorflow-agent /etc/sudoers.d/monitorflow-agent
visudo -cf /etc/sudoers.d/monitorflow-agent
```

The file name must stay free of dots, or sudo skips the file. Without these rules, privileged commands fail with sudo's "a password is required", and the command shows as failed in the API.

What the rules allow:

| Command | Used by |
| --- | --- |
| `apt-get -q update`, `apt-get … upgrade` (with `DEBIAN_FRONTEND` kept) | `apply_updates` on apt |
| `dnf -q -y upgrade`, with or without `--security` | `apply_updates` on dnf |
| `shutdown -r +1` | `apply_updates` with `allow_reboot` |
| `monitorflow-agent sources apply`, `… sources revert` | `configure_sources`, `revert_sources` |

There are no wildcards, so updates that name packages do not work: `apt-get install --only-upgrade <packages>` and `dnf upgrade <packages>`. Security-only updates on apt name their packages too, so they do not work either. To allow them, add a rule with a wildcard after `--only-upgrade` or `upgrade` yourself. Read ADR-0031 first: the rule is then only as narrow as the agent's package name check.

## Pointing a server at the mirrors

On the central server, set `mirror_public_url` to the URL this host reaches the mirror listener at, for example `http://mirror.example.net:7480`. Then an administrator of the server's customer runs:

```
POST /api/servers/{id}/sources   {"mirrors": [{"name": "debian-12"}], "mode": "replace"}
DELETE /api/servers/{id}/sources
```

The first request points the host at the mirrors, `current` snapshot by default. The second restores the sources the host had before. Follow the result with `GET /api/commands/{id}`. A failed post-check (`apt-get update` or `dnf makecache`) reverts the host on its own and reports the error. The host must be able to reach `mirror_public_url`, even when its agent connects through a relay (ADR-0035).
