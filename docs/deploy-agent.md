# Installing the agent

How to install the agent on a Linux server, with the installer or by hand, until there are packages. The installer and the packages do the same steps as the manual sections below. Background is in architecture §4 (least privilege) and §7 (packages), and in ADR-0031, ADR-0032, ADR-0035 and ADR-0036. For Windows, see [Windows](#windows) at the end.

## Quick install

With a token from the web interface (Admin › Enrollment tokens, role Agent), on a host with systemd, x86_64 or aarch64:

```
curl -fsSL https://github.com/Kidoly/monitorflow/releases/latest/download/install.sh \
  | sudo MF_SERVER=monitorflow.example.com:7443 MF_TOKEN=mfe1.… sh
```

The installer (ADR-0052) downloads the static agent for the host's architecture and `SHA256SUMS` from the same place, verifies the release signature `SHA256SUMS.sig` against the release keys it carries (ADR-0053, with OpenSSL 3; without it, it warns and checks checksums only), checks the checksum before it changes anything, and then does every step of [Binary and user](#binary-and-user) and [Sudo rules](#sudo-rules): binary, user, directories, `agent.toml`, sudo rules checked with `visudo`, and the systemd unit `monitorflow-agent`. It hands the token to the first start only, and waits until the agent has its identity. The web interface shows this command filled in, and the Windows one, once an agent token is created; give it the relay's address there if the agent connects through one.

| Variable | Meaning |
| --- | --- |
| `MF_SERVER` | `host:port` of the server or relay the agent connects to, port 7443 by default. An https URL works too. IPv6 addresses in brackets: `[2001:db8::1]:7443`. Required on the first install |
| `MF_TOKEN` | The enrollment token. Required unless the host is enrolled |
| `MF_ENROLL` | `host:port` of the server's enrollment listener. By default `MF_SERVER`'s host on port 7445, which is right for a direct connection. Set it when `MF_SERVER` is a relay, to the central server's address: relays do not enroll agents (ADR-0025) |
| `MF_VERSION` | Release to install, like `0.2.0`. The latest by default |
| `MF_DOWNLOAD_URL` | https URL of a directory holding the release files, instead of GitHub (below) |
| `MF_TIMEOUT` | Seconds to wait for the enrollment, 120 by default |
| `MF_REQUIRE_SIGNATURE=1` | Fail instead of warning when OpenSSL 3 is missing and the signature cannot be checked |
| `MF_ALLOW_UNSIGNED=1` | Install a release without `SHA256SUMS.sig`, like 0.1.0, with its checksums checked only |
| `MF_UNINSTALL=1` | Remove the binary, the unit and the sudo rules. `agent.toml` and the identity stay, so a reinstall keeps the server's history. Add `MF_PURGE=1` to remove them and the `monitorflow` user too |

An agent behind a relay:

```
curl -fsSL https://github.com/Kidoly/monitorflow/releases/latest/download/install.sh \
  | sudo MF_SERVER=relay1.paris.example:7443 MF_ENROLL=monitorflow.example.com:7445 MF_TOKEN=mfe1.… sh
```

If the enrollment fails, for example on a used or expired token or an unreachable port 7445, the installer stops the service, removes the token, prints the end of the agent's log and exits with an error: see `journalctl -u monitorflow-agent`, fix the cause and run it again, with a new token if needed. The token is never printed. On a host shared with other users, keep it off the command line, where `ps` shows it while the installer runs:

```
read -rs MF_TOKEN && export MF_TOKEN
curl -fsSL https://github.com/Kidoly/monitorflow/releases/latest/download/install.sh \
  | sudo --preserve-env=MF_TOKEN MF_SERVER=monitorflow.example.com:7443 sh
```

**Upgrades.** Running the same command again on an enrolled host installs the new binary, unit and sudo rules and restarts the agent; `agent.toml` and the identity are kept, and `MF_SERVER`, `MF_ENROLL` and `MF_TOKEN` are not needed. Edit `/etc/monitorflow/agent.toml` to change the upstream later.

**Serving the files yourself.** For hosts without access to GitHub, copy the assets of a release (`install.sh`, `SHA256SUMS`, `SHA256SUMS.sig`, `VERSION`, `monitorflow-agent-linux-x86_64`, `monitorflow-agent-linux-aarch64`, `monitorflow-agent.service`, `monitorflow-agent-sudoers`, and the Windows files if needed) into one directory of any web server with HTTPS, and pass its URL:

```
curl -fsSL https://files.example.net/monitorflow/0.2.0/install.sh \
  | sudo MF_DOWNLOAD_URL=https://files.example.net/monitorflow/0.2.0 MF_SERVER=… MF_TOKEN=… sh
```

Keep one directory per version: `MF_DOWNLOAD_URL` names the version, and `MF_VERSION` applies to GitHub only. The installer accepts only HTTPS; the host must trust the web server's certificate. The signature makes the copy safe to serve from anywhere: an installer fetched from GitHub refuses files that the project did not sign. Set the server's `agent_download_url` (`AGENT_DOWNLOAD_URL` in the compose `.env`) to the same URL, and the web interface's commands use it.

**Ansible.** One token per host, created on the server host as needed, with a short validity. Here the server runs with the production compose (deploy-server.md) on the inventory host `monitorflow`:

```yaml
- hosts: monitored
  become: true
  vars:
    mf_server: monitorflow.example.com:7443
    mf_site: main
  tasks:
    - name: Check whether the agent is enrolled
      ansible.builtin.stat:
        path: /var/lib/monitorflow/agent/tls/cert.pem
      register: mf_identity

    - name: Create an enrollment token
      ansible.builtin.command:
        argv: [docker, compose, -f, /opt/monitorflow/deploy/docker-compose.yml, exec, -T, server,
               monitorflow-server, token, create, --role, agent, --id, "{{ inventory_hostname_short }}",
               --site, "{{ mf_site }}", --ttl, 15m]
      delegate_to: monitorflow
      register: mf_token
      when: not mf_identity.stat.exists
      no_log: true

    - name: Install, enroll or upgrade the agent
      ansible.builtin.shell: >
        set -o pipefail &&
        curl -fsSL https://github.com/Kidoly/monitorflow/releases/latest/download/install.sh | sh
      args:
        executable: /bin/bash
      environment:
        MF_SERVER: "{{ mf_server }}"
        MF_TOKEN: "{{ mf_token.stdout | default('') }}"
      no_log: true
```

`environment` keeps the token off the command line. Without the first two tasks, the same play upgrades every agent.

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
| `monitorflow-agent self-update` | `self_update`: installs a signed agent release (below) |

There are no wildcards. The agent never runs `apt-get` or `dnf` through sudo: the `packages update` helper reads the kind of update and the package names on stdin, refuses names that are not package names (options, paths, shell characters), and runs `apt-get update` and the upgrade, or `dnf upgrade`, itself (ADR-0036). Security-only updates use `dnf upgrade --security` on dnf, and on apt the packages whose candidate comes from a `-security` suite.

Hosts set up with an earlier version of this file, which allowed `apt-get` and `dnf` lines and no `packages update`, need the new file: until then every update fails with "a password is required". Remove any wildcard rule added for named updates at the same time; it is no longer used.

## Self-update

An administrator can update agents from MonitorFlow instead of running the installer again (ADR-0053). The `self_update` command names a version and where to download it. The agent downloads the release, checks its signature against the release keys built into it and refuses anything else, whatever served it; it refuses older versions unless the command allows a downgrade. Then:

1. `sudo -n /usr/bin/monitorflow-agent self-update` checks the release again as root, writes the new binary next to `/usr/bin/monitorflow-agent`, keeps the running one as `/var/lib/monitorflow/self-update/previous` and renames the new one into place.
2. It starts the watchdog in a transient unit, `monitorflow-agent-self-update`, from the previous binary. The watchdog restarts `monitorflow-agent` and waits for the new agent to connect upstream (5 minutes by default).
3. If the new agent does not connect in time, the watchdog puts the previous binary back and restarts the service. The command then fails with "rolled back to …".

The outcome is in `/var/lib/monitorflow/self-update/status.json`, and the watchdog's log is in `journalctl -u monitorflow-agent-self-update`. `/var/lib/monitorflow/self-update` belongs to root; the helper refuses it otherwise.

**Starting an update.** On a server's page, administrators of its customer see "Update agent to …" when the server holds a newer release than the agent runs; the Commands tab follows the update and shows "rolled back to …" if it came back. The API takes the same command: `POST /api/servers/{id}/commands` with `{"kind": "self_update", "version": "0.3.0"}`, and optionally `"allow_downgrade": true` and `"health_timeout_s"` (30 to 3600).

**Many servers at once.** An update campaign can update agents instead of packages: choose "The MonitorFlow agent" and a version in the new campaign form (`agent_version` in `POST /api/campaigns`). Waves, maintenance windows and the failure threshold work as for package updates; a rolled-back server counts as failed.

**Automatic updates.** On by default (ADR-0055). Every 5 minutes the server moves online agents to the newest stable release it holds, never newer than the server itself, never to a pre-release and never backwards. Each site starts with one agent, its canary; once the canary is updated and connected again, the site's other agents follow, a quarter of the site at a time. A server is updated automatically at most once per version: one that rolled back stays on its version until an administrator updates it or a newer release comes. When 2 updates of a release have failed (the threshold is a setting), its rollout halts everywhere and waits for a global administrator. Relays are updated the same way and first: an agent behind a relay waits until the relay runs the target version ([Installing a relay](deploy-relay.md#self-update)).

Updates › Automatic updates (global administrators) shows the version being rolled out, its progress per site, and a halted rollout with the reason and a "Resume" button; global administrators can turn automatic updates off for the whole server, pause them all at once, or change the failure threshold there. Under Admin › Sites, the site's administrators can override the server-wide setting for one site: "Server default", "On" or "Off". `auto_update.enabled` in `server.toml` (or `MONITORFLOW_AUTO_UPDATE_ENABLED`) wins over the web interface and locks the switch there. Updates are audited as `agent.auto_update`, the start and halt of a rollout as `auto_update.rollout_started` and `auto_update.halted`, changes as `auto_update.settings`, `auto_update.resume` and `site.agent_auto_update`.

**Where agents download from.** The command lists, in order: the mirror listener of the agent's relay, when the relay mirrors and reported its `mirror.public_url` (relays copy the two newest releases the server holds, each cycle of their mirror sync, and check their signature first), the server's own copy at `<mirror_public_url>/agent/<version>/` when it holds that version, then `agent_download_url` (for GitHub, the release of that version; a self-hosted URL as it is, skipped by the agent if it serves another version). For sites without Internet access, give the server a copy of each release on its mirror listener, from a directory holding the release files:

```
docker compose cp ./monitorflow-0.3.0 server:/tmp/release
docker compose exec server monitorflow-server agent-release import --dir /tmp/release
docker compose exec server monitorflow-server agent-release list
```

The import checks the signature first. The web interface offers the newest version the server holds.

**Hosts installed before self-update** lack the sudo line above: run the installer once more, which installs the new rules with the new binary. Until then `self_update` fails with "a password is required" and nothing changes. A self-update replaces the binary only; when a release changes the unit or the sudo rules, its notes say to run the installer.

## Pointing a server at the mirrors

On the central server, set `mirror_public_url` to the URL this host reaches the mirror listener at, for example `http://mirror.example.net:7480`. Then an administrator of the server's customer runs:

```
POST /api/servers/{id}/sources   {"mirrors": [{"name": "debian-12"}], "mode": "replace"}
DELETE /api/servers/{id}/sources
```

The first request points the host at the mirrors, `current` snapshot by default. The second restores the sources the host had before. Follow the result with `GET /api/commands/{id}`. A failed post-check (`apt-get update` or `dnf makecache`) reverts the host on its own and reports the error. The host must be able to reach `mirror_public_url`, unless its agent connects through a relay that serves the mirrors.

Behind a relay with mirroring enabled (`[mirror] enabled = true`), set `mirror.public_url` in the relay's configuration to the URL the hosts of its site reach the relay's `mirror_listen_addr` at, for example `http://relay.example.net:7480`. The relay reports it to the server with the snapshots it holds, and `GET /api/relays/{id}` shows it as `mirror_url`. Sources then point at the relay when it holds every snapshot requested (for `current`, any snapshot of the mirror), and at `mirror_public_url` otherwise (ADR-0037). The choice is made when the command is queued: if the relay later drops the snapshot or stops mirroring, send the sources again.

## Windows

The Windows agent is the same binary built for `x86_64-pc-windows-msvc` (ADR-0047). CI uploads it as the `monitorflow-agent-windows-x86_64` artifact, and its MSI package as `monitorflow-agent-windows-x86_64-msi`. It runs as the service `MonitorFlowAgent` under its virtual account `NT SERVICE\MonitorFlowAgent`, which has no administrator rights.

### Install with the MSI

The package (ADR-0050) does every manual step below: binary, service, failure actions, folders and ACLs, and it registers the `MonitorFlowAgent` event source. In an elevated PowerShell, with a token from the web interface:

```powershell
msiexec /i monitorflow-agent-0.1.0-x64.msi /qn /l*v install.log `
  UPSTREAM_URL=https://relay.example.com:7443 ENROLLMENT_TOKEN=mfe1.…
```

| Property | Effect |
| --- | --- |
| `UPSTREAM_URL` | Writes `%ProgramData%\MonitorFlow\agent.toml` with it, unless the file exists, and starts the service. Without it, nothing is written and the service is not started: copy `agent.windows.example.toml` from `%ProgramFiles%\MonitorFlow`, then `Start-Service MonitorFlowAgent` |
| `ENROLL_URL` | Optional `enroll_url`, needed when the upstream is a relay |
| `ENROLLMENT_TOKEN` | Optional one-time token. The MSI log shows asterisks in its place. Remove it from `agent.toml` once enrolled (below) |
| `REMOVE_DATA=1` | On uninstall, also deletes `%ProgramData%\MonitorFlow`: configuration, identity and logs |

An invalid URL makes the install fail and roll back; `install.log` has the custom action `WriteAgentConfig` failing. The package is not signed yet, so Windows shows an unknown publisher.

Installing a newer package upgrades in place: the service restarts with the new binary, and `agent.toml` and the identity are kept. `msiexec /x monitorflow-agent-0.1.0-x64.msi /qn` (or Apps and Features) removes the binary and the service and keeps `%ProgramData%\MonitorFlow`, so a reinstall keeps the same identity. When the service stops on an error, the Application event log has the reason, from the source `MonitorFlowAgent`.

The package does not install the Windows Update service yet (ADR-0050 leaves its place in `AgentHelpers`): add it by hand as in [Windows Update service](#windows-update-service).

### Install by hand

These steps are what the MSI does. Run them in an elevated PowerShell, in this order: the service account exists only once the service is created, and the ACLs name it. Instead of copying the example, `monitorflow-agent.exe --config "$data\agent.toml" init-config --upstream-url https://… --enrollment-token …` writes a minimal `agent.toml`, as the MSI does.

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
# Scripts (ADR-0048): the same as the configuration, protected on its own.
icacls "$data\scripts.d" /inheritance:r /grant:r "SYSTEM:(OI)(CI)F" "Administrators:(OI)(CI)F" "NT SERVICE\MonitorFlowAgent:(OI)(CI)RX"
```

**Self-update on Windows.** The agent hands a downloaded, checked release to `MonitorFlowUpdate`, which checks it again, renames the running `monitorflow-agent.exe` to `monitorflow-agent.previous.exe`, puts the new one in place and restarts `MonitorFlowAgent`; if the new agent does not connect in time it renames them back. The MSI installs the update service; by hand, create it as in [Windows Update service](#windows-update-service). Its log, `%ProgramData%\MonitorFlow\update\update.log`, shows each step. The MSI still records the version it installed: a later MSI upgrade works as usual, and repairing the older MSI would put its binary back.

Start from `deploy/config/agent.windows.example.toml`. Its paths assume `%ProgramData%` is `C:\ProgramData`. Use single-quoted TOML strings for paths, so backslashes are not escapes. `%ProgramFiles%` lets only administrators write by default, which keeps the binary as safe as a root-owned `/usr/bin/monitorflow-agent`. The agent also protects what it writes itself: the private key, the command journal and the directories it creates get a DACL for `SYSTEM`, `Administrators` and their owner only, whatever the parent allows. At start, it logs a warning when the key's DACL allows anyone else.

### Enrolling

A service has no shell to pass `MONITORFLOW_ENROLLMENT_TOKEN` from. Put the token in `agent.toml` for the first start instead, which the MSI's `ENROLLMENT_TOKEN` does: the ACLs above make the file readable only by the service and administrators. Remove the token once the log says `identity loaded`.

```powershell
Start-Service MonitorFlowAgent
Get-Content "$data\agent\logs\agent.log" -Wait
```

As a service, the agent logs to `<data_dir>\logs\agent.log`. At 16 MiB the file moves to `agent.log.1`, replacing the older one. If the service stops at once, for example because the configuration is invalid, the System event log has the control manager's event 7024 with service-specific error 1, and `agent.log` has the reason when logging had started. `Stop-Service MonitorFlowAgent` stops the agent cleanly. `sc.exe delete MonitorFlowAgent` removes the service.

Enroll and run the agent through the service only. Started from an elevated console, the agent writes its identity as `Administrators`, and the service can then not read it. If that happened, delete `$data\agent\tls` and enroll again with a new token. Without the `service` argument, `monitorflow-agent.exe` runs in the console until Ctrl+C. That is meant for `sensors probe`, for example `monitorflow-agent.exe sensors probe port --param port=3389`.

### Windows Update service

Updates, update reports and self-updates go through a second service, `MonitorFlowUpdate` (ADR-0049, ADR-0053). The MSI installs it; these steps are for a manual install. It is the same binary started as `update-service` under LocalSystem, because the agent's account cannot install updates. It accepts typed requests from the agent only, on the named pipe `\\.\pipe\MonitorFlowUpdate`, whose access list admits `SYSTEM` and `NT SERVICE\MonitorFlowAgent` and nobody else. It reads no configuration file. In the same elevated PowerShell:

```powershell
sc.exe create MonitorFlowUpdate DisplayName= "MonitorFlow update service" start= auto `
  obj= LocalSystem binPath= "`"$bin\monitorflow-agent.exe`" update-service"
sc.exe failure MonitorFlowUpdate reset= 86400 actions= restart/60000/restart/60000/restart/60000
Start-Service MonitorFlowUpdate
Get-Content "$data\update\update.log"   # "update service listening"
```

The service logs to `%ProgramData%\MonitorFlow\update\update.log`, rotated at 16 MiB like the agent's log. Without it the agent runs as before, but it sends no update report, logs a warning every 5 minutes, and fails `apply_updates` and `self_update` with "the update service is not running". The two services can be installed in either order. `sc.exe delete MonitorFlowUpdate` removes it.

Updates come from the source the machine is configured for: Windows Update, Microsoft Update, or the WSUS server set by Group Policy, together with its approvals. MonitorFlow never changes that source, and its mirrors do not apply to Windows. Windows keeps its own automatic updates. On servers patched by campaigns, set them to notify or download only (Group Policy "Configure Automatic Updates", option 2 or 3), so installs happen in campaign windows.

### What works on Windows

| Feature | State |
| --- | --- |
| Enrollment, certificate renewal, stream, inventory | As on Linux; `os_family` is `windows`, the version comes from the registry, like `Windows Server 2022 Datacenter 21H2 build 20348.2849` |
| Sensors | `cpu`, `memory`, `disk`, `port`, `script` (PowerShell or a program), `windows_service` and `eventlog` (ADR-0048). `systemd` and `log` are refused as "not supported on this platform" in the agent's configuration acknowledgement |
| Update reports | Pending and installed software updates by KB number, with title, classification, MSRC severity, size and restart state, plus whether Windows waits for a restart. Needs the update service (above) |
| `apply_updates`, update campaigns | Through the update service. All pending updates except feature upgrades, `security_only` for the Security Updates classification, or named KB numbers or update IDs (`KB5034441`). Restarts only with `allow_reboot`, one minute after the result. Windows servers may join any campaign: pinned mirror snapshots do not apply to them |
| `configure_sources`, `revert_sources` | Refused by the API for Windows servers, and by the agent with "package sources are not supported on Windows yet" |
| `shell` (AI remediation) | Refused even with `commands.shell = true`: the server's risk rules assume `/bin/sh` (ADR-0047), so a remediation on a Windows server escalates at its first command |

There are no sudo rules on Windows. The only privileged part is the update service, which accepts typed requests from the agent alone.
