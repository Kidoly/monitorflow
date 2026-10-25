# Installing the agent

How to install the agent on a Debian-family or RHEL-family server by hand, until there are packages. The packages will do the same steps. Background is in architecture §4 (least privilege) and §7 (packages), and in ADR-0031, ADR-0032, ADR-0035 and ADR-0036. For Windows, see [Windows](#windows) at the end.

## Binary and user

The agent runs as the unprivileged system user `monitorflow`. Sudo runs it as root only for its package and source helpers, and the sudoers rules name it by its full path, so it must be installed at exactly `/usr/bin/monitorflow-agent`, owned by root and writable by no one else:

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
| `monitorflow-agent packages update` | `apply_updates`: every kind, all packages, security only or named packages |
| `shutdown -r +1` | `apply_updates` with `allow_reboot` |
| `monitorflow-agent sources apply`, `… sources revert` | `configure_sources`, `revert_sources` |

There are no wildcards. The agent never runs `apt-get` or `dnf` through sudo: the `packages update` helper reads the kind of update and the package names on stdin, refuses names that are not package names (options, paths, shell characters), and runs `apt-get update` and the upgrade, or `dnf upgrade`, itself (ADR-0036). Security-only updates use `dnf upgrade --security` on dnf, and on apt the packages whose candidate comes from a `-security` suite.

Hosts set up with an earlier version of this file, which allowed `apt-get` and `dnf` lines and no `packages update`, need the new file: until then every update fails with "a password is required". Remove any wildcard rule added for named updates at the same time; it is no longer used.

## Pointing a server at the mirrors

On the central server, set `mirror_public_url` to the URL this host reaches the mirror listener at, for example `http://mirror.example.net:7480`. Then an administrator of the server's customer runs:

```
POST /api/servers/{id}/sources   {"mirrors": [{"name": "debian-12"}], "mode": "replace"}
DELETE /api/servers/{id}/sources
```

The first request points the host at the mirrors, `current` snapshot by default. The second restores the sources the host had before. Follow the result with `GET /api/commands/{id}`. A failed post-check (`apt-get update` or `dnf makecache`) reverts the host on its own and reports the error. The host must be able to reach `mirror_public_url`, unless its agent connects through a relay that serves the mirrors.

Behind a relay with mirroring enabled (`[mirror] enabled = true`), set `mirror.public_url` in the relay's configuration to the URL the hosts of its site reach the relay's `mirror_listen_addr` at, for example `http://relay.example.net:7480`. The relay reports it to the server with the snapshots it holds, and `GET /api/relays/{id}` shows it as `mirror_url`. Sources then point at the relay when it holds every snapshot requested (for `current`, any snapshot of the mirror), and at `mirror_public_url` otherwise (ADR-0037). The choice is made when the command is queued: if the relay later drops the snapshot or stops mirroring, send the sources again.

## Windows

The Windows agent is the same binary built for `x86_64-pc-windows-msvc` (ADR-0047). CI uploads it as the `monitorflow-agent-windows-x86_64` artifact. It runs as the service `MonitorFlowAgent` under its virtual account `NT SERVICE\MonitorFlowAgent`, which has no administrator rights. These steps are what the MSI package will do. Run them in an elevated PowerShell, in this order: the service account exists only once the service is created, and the ACLs name it.

```powershell
$bin  = "$env:ProgramFiles\MonitorFlow"
$data = "$env:ProgramData\MonitorFlow"
New-Item -ItemType Directory -Force $bin, "$data\agent", "$data\scripts.d" | Out-Null
Copy-Item monitorflow-agent.exe "$bin\monitorflow-agent.exe"
Copy-Item agent.windows.example.toml "$data\agent.toml"   # then set upstream_url

sc.exe create MonitorFlowAgent DisplayName= "MonitorFlow agent" start= auto `
  obj= "NT SERVICE\MonitorFlowAgent" `
  binPath= "`"$bin\monitorflow-agent.exe`" --config `"$data\agent.toml`" service"
sc.exe failure MonitorFlowAgent reset= 86400 actions= restart/60000/restart/60000/restart/60000

# Configuration and scripts: the service reads, administrators write.
icacls $data /inheritance:r /grant:r "SYSTEM:(OI)(CI)F" "Administrators:(OI)(CI)F" "NT SERVICE\MonitorFlowAgent:(OI)(CI)RX"
# Identity, sensor configuration, command journal and logs: the service writes.
icacls "$data\agent" /inheritance:r /grant:r "SYSTEM:(OI)(CI)F" "Administrators:(OI)(CI)F" "NT SERVICE\MonitorFlowAgent:(OI)(CI)M"
```

Start from `deploy/config/agent.windows.example.toml`. Its paths assume `%ProgramData%` is `C:\ProgramData`. Use single-quoted TOML strings for paths, so backslashes are not escapes. `%ProgramFiles%` lets only administrators write by default, which keeps the binary as safe as a root-owned `/usr/bin/monitorflow-agent`. The agent also protects what it writes itself: the private key, the command journal and the directories it creates get a DACL for `SYSTEM`, `Administrators` and their owner only, whatever the parent allows. At start, it logs a warning when the key's DACL allows anyone else.

### Enrolling

A service has no shell to pass `MONITORFLOW_ENROLLMENT_TOKEN` from. Put the token in `agent.toml` for the first start instead: the ACLs above make the file readable only by the service and administrators. Remove the token once the log says `identity loaded`.

```powershell
Start-Service MonitorFlowAgent
Get-Content "$data\agent\logs\agent.log" -Wait
```

As a service, the agent logs to `<data_dir>\logs\agent.log`. At 16 MiB the file moves to `agent.log.1`, replacing the older one. If the service stops at once, for example because the configuration is invalid, the System event log has the control manager's event 7024 with service-specific error 1, and `agent.log` has the reason when logging had started. `Stop-Service MonitorFlowAgent` stops the agent cleanly. `sc.exe delete MonitorFlowAgent` removes the service.

Enroll and run the agent through the service only. Started from an elevated console, the agent writes its identity as `Administrators`, and the service can then not read it. If that happened, delete `$data\agent\tls` and enroll again with a new token. Without the `service` argument, `monitorflow-agent.exe` runs in the console until Ctrl+C. That is meant for `sensors probe`, for example `monitorflow-agent.exe sensors probe port --param port=3389`.

### What works on Windows

| Feature | State |
| --- | --- |
| Enrollment, certificate renewal, stream, inventory | As on Linux; `os_family` is `windows`, the version comes from the registry, like `Windows Server 2022 Datacenter 21H2 build 20348.2849` |
| Sensors | `port`. The other kinds are refused with "not available on this platform" in the agent's configuration acknowledgement, until the Windows sensors task |
| `apply_updates`, `configure_sources`, `revert_sources` | Refused with "package updates are not supported on Windows yet" ("package sources …" for the source commands). Leave Windows servers out of update campaigns |
| `shell` (AI remediation) | Refused even with `commands.shell = true`: the server's risk rules assume `/bin/sh` (ADR-0047), so a remediation on a Windows server escalates at its first command |

There are no sudo rules on Windows, because the agent runs nothing privileged.
