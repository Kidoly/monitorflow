# Installing the agent

## Getting started

1. In the web interface, open Administration › Add servers and choose the site.
2. Copy the install command it shows. It looks like this:

   ```
   curl -fsSL https://github.com/Kidoly/monitorflow/releases/latest/download/install.sh \
     | sudo MF_SERVER=monitorflow.example.com:7443 MF_TOKEN=mfe1.… sh
   ```

3. Paste it on each Linux server; the same command serves every server of the site. On Windows, the page shows a PowerShell line instead.

The server appears in the fleet within seconds, under its own name. To remove it, use Remove on its page, which shows the uninstall command.

The rest of this page is reference. Background is in architecture §4 (least privilege) and §7 (packages)[^0031][^0032][^0035][^0036]. For Windows, see [Windows](#windows) at the end.

## Quick install

The command works on any Linux distribution with systemd, x86_64 or aarch64. The installer[^0052] downloads the static agent for the machine's architecture and `SHA256SUMS` from the same place, verifies the release signature `SHA256SUMS.sig` against the release keys it carries (with OpenSSL 3; without it, it warns and checks checksums only)[^0053], checks the checksum before it changes anything, and then installs the binary, user, directories, `agent.toml`, sudo rules checked with `visudo`, and the systemd unit `monitorflow-agent`. It hands the token to the first start only, and waits until the agent has its identity. The web interface builds the command from the source you pick there ([Install sources](#install-sources)); give it the relay's address there if the agent connects through one.

The agent monitors any distribution[^0063]. Updates and package sources need the Debian or RHEL family with apt or dnf present; on any other server the server page says "Package updates aren't available on this system; monitoring works normally", and campaigns leave the server out.

One command serves every server of a site, for 30 days by default[^0073]. Each server connects under its own name, made unique with `-2`, `-3`… when another server already has it. When the command was created with "Approve new servers before they appear", new servers wait under Waiting for approval on the same page until an administrator approves them. Revoking the command stops new installs; servers already installed keep working. For one server under a name you choose, and for relays, single-use tokens remain under Advanced.

| Variable | Meaning |
| --- | --- |
| `MF_SERVER` | `host:port` of the server or relay the agent connects to, port 7443 by default. An https URL works too. IPv6 addresses in brackets: `[2001:db8::1]:7443`. Required on the first install |
| `MF_TOKEN` | The token of the site's install command, or a single-use token. Required on the first install |
| `MF_ENROLL` | `host:port` where the central server takes first connections. By default port 7445 of the `MF_SERVER` machine, which is right for a direct connection. Set it when `MF_SERVER` is a relay, to the central server's address: relays do not take first connections[^0025] |
| `MF_VERSION` | Release to install, like `0.2.0`. The latest by default |
| `MF_DOWNLOAD_URL` | https URL of a directory holding the release files, instead of GitHub ([Install sources](#install-sources)) |
| `MF_SUMS_SHA256` | SHA-256 of the release's `SHA256SUMS`, as the central server's commands give it. Every file is checked against it, so downloads do not check the certificate (HTTPS still) |
| `MF_TIMEOUT` | Seconds to wait for the first connection, 120 by default |
| `MF_REQUIRE_SIGNATURE=1` | Fail instead of warning when OpenSSL 3 is missing and the signature cannot be checked |
| `MF_ALLOW_UNSIGNED=1` | Install a release without `SHA256SUMS.sig`, like 0.1.0, with its checksums checked only |
| `MF_ALLOW_COMMANDS=1` | Let this agent run the shell commands of AI remediation (`commands.shell`), through a systemd drop-in. Works on an installed agent too, and is kept by later runs; `MF_ALLOW_COMMANDS=0` takes it back. Either value also undoes what the web interface last set. Each server shows whether it accepts AI commands[^0064] |
| `MF_AI_COMMANDS_FROM_WEB=1` | Let administrators turn AI commands on and off from the server's page (`commands.from_web`), through a systemd drop-in. Without it, the web interface can only turn them off. Works on an installed agent too; `MF_AI_COMMANDS_FROM_WEB=0` takes it back. "Allow AI commands to be turned on from the web interface" on the Add servers page adds it to the command[^0098] |
| `MF_UNINSTALL=1` | Remove the binary, the unit and the sudo rules. `agent.toml` and the identity stay, so a reinstall keeps the server's history. Add `MF_PURGE=1` to remove them and the `monitorflow` user too. "Remove" on the server's page shows the command to use |

An agent behind a relay:

```
curl -fsSL https://github.com/Kidoly/monitorflow/releases/latest/download/install.sh \
  | sudo MF_SERVER=relay1.paris.example:7443 MF_ENROLL=monitorflow.example.com:7445 MF_TOKEN=mfe1.… sh
```

If the first connection fails, for example on a used or expired token or an unreachable port 7445, the installer stops the service, removes the token, prints the end of the agent's log and exits with an error: see `journalctl -u monitorflow-agent`, fix the cause and run it again, with a new token if needed. The token is never printed. On a server shared with other users, keep it off the command line, where `ps` shows it while the installer runs:

```
read -rs MF_TOKEN && export MF_TOKEN
curl -fsSL https://github.com/Kidoly/monitorflow/releases/latest/download/install.sh \
  | sudo --preserve-env=MF_TOKEN MF_SERVER=monitorflow.example.com:7443 sh
```

**Upgrades.** Running the same command again on an installed server installs the new binary, unit and sudo rules and restarts the agent; `agent.toml` and the identity are kept, and `MF_SERVER`, `MF_ENROLL` and `MF_TOKEN` are not needed. Edit `/etc/monitorflow/agent.toml` to change the upstream later.

**Ansible and other tools.** Run the site's install command as it is on every server: it adds new servers and upgrades the others.

## System logs

On Linux, the agent forwards the warnings and errors of the systemd journal (priority warning and above, every unit and the kernel) to the central server, where they show in the Logs tab of the server's page and in the context AI remediation reads[^0091]. There is nothing to set up. The unit puts the agent's user in the `systemd-journal` group, which may read the journal and nothing else. An agent installed before this release forwards only its own lines and says so in the Logs tab until you run the install command again, or `usermod -aG systemd-journal monitorflow` and restart it.

The agent remembers where it stopped in `/var/lib/monitorflow/agent/journal.cursor`, so lines written while it was stopped or updating are sent at the next start, within a day. Each agent sends at most 1,000 lines at once, then 10 a second; lines over that are dropped, and the Logs tab says how many. While the central server cannot be reached, the agent stops reading and the journal keeps the lines. The server removes passwords, keys and tokens it recognises from the lines before it stores them, and keeps them as long as its other logs (Administration › Settings, "Keep logs").

To stop forwarding on one server, remove the group with a drop-in: `systemctl edit monitorflow-agent`, then add `[Service]` and an empty `SupplementaryGroups=`. The agent then sends only its own lines. Windows event logs are not forwarded yet.

## Install sources

The Add servers page offers the commands from three sources[^0054]. A global administrator picks the default under Administration › Settings; any command can use another. The configuration can fix the default instead: `agent_install_source` (`AGENT_INSTALL_SOURCE` in the compose `.env`), and `agent_download_url` (`AGENT_DOWNLOAD_URL`) for the custom URL.

| Source | Servers download from | Servers need |
| --- | --- | --- |
| GitHub (default) | `https://github.com/Kidoly/monitorflow/releases/latest/download` | Access to GitHub |
| This server | `https://<web interface>/agent/<version>/`, the server's own copy of a release | Access to the web interface's address, on port 443; not trusting its certificate |
| Custom URL | Any https directory holding the release files | Access to it, and trust in its certificate |

Every source serves the same signed files, and the installer checks the release signature whatever the source (with OpenSSL 3).

**This server.** For sites without Internet access. The server keeps whole releases in its cache, the one self-updates use[^0053], and serves them behind the web interface. Once per release, on a machine with Internet access, download every asset of the release from its GitHub page into one directory: `install.sh`, `SHA256SUMS`, `SHA256SUMS.sig`, `VERSION`, `monitorflow-agent-linux-x86_64`, `monitorflow-agent-linux-aarch64`, `monitorflow-agent.service`, `monitorflow-agent-sudoers`, `monitorflow-agent-windows-x86_64.exe` and `monitorflow-agent-windows-x86_64.msi`. Then choose all of them under Administration › Server update in the web interface. From a shell, copy the directory to the central server and import it ([deploy-server.md](deploy-server.md#agent-releases-on-the-server) for the compose variant):

```
monitorflow-server agent-release import --dir ./monitorflow-0.3.0
monitorflow-server agent-release list
```

The import reads only that directory and checks the signature, then every file, before it keeps anything; a release that fails is refused whole. Until a release is imported, the Add servers page says so instead of showing a command, and `https://<web interface>/agent/latest/…` answers 404 with the same advice. The command names the newest imported release and carries the checksums of its `install.sh`, `SHA256SUMS` and MSI:

```
f=$(mktemp) && curl -fsSLk -o "$f" https://monitorflow.example.com/agent/0.3.0/install.sh \
  && echo "<sha256 of install.sh>  $f" | sha256sum -c \
  && sudo MF_DOWNLOAD_URL=https://monitorflow.example.com/agent/0.3.0 MF_SUMS_SHA256=<sha256 of SHA256SUMS> MF_SERVER=… MF_TOKEN=… sh "$f"; rm -f "$f"
```

Servers need not trust the web certificate, which is often issued by Caddy's own CA or a company CA: the checksums come from the page you copied the command from, so `curl -k` cannot let a different file through. The Windows lines download the MSI with `curl.exe -k` (Windows 10 1803, Windows Server 2019 and later) and run `msiexec` only if its checksum matches. On older Windows, copy the MSI to the server and compare `Get-FileHash` with its line in `SHA256SUMS`. The address in the command is the one your browser uses for the web interface; if servers reach it by another name, change it in the command. Servers that do trust the certificate can also use the moving `latest` directory, like a custom URL: `MF_DOWNLOAD_URL=https://<web interface>/agent/latest`.

**Custom URL.** Copy the files of a release into one directory of any web server with HTTPS, and pass its URL:

```
curl -fsSL https://files.example.net/monitorflow/0.2.0/install.sh \
  | sudo MF_DOWNLOAD_URL=https://files.example.net/monitorflow/0.2.0 MF_SERVER=… MF_TOKEN=… sh
```

Keep one directory per version: `MF_DOWNLOAD_URL` names the version, and `MF_VERSION` applies to GitHub only. The installer accepts only HTTPS; the server must trust the web server's certificate. The signature makes the copy safe to serve from anywhere: an installer fetched from GitHub refuses files that the project did not sign. Set this URL as the custom URL under Administration › Settings, or as the server's `agent_download_url`, and the web interface's commands and agents' self-updates use it.

## Without systemd

The installer needs systemd. On another init system, install the same files by hand, with the release's `monitorflow-agent-linux-<arch>`, renamed, and `monitorflow-agent-sudoers`. Sudo runs the agent as root only for its package and source helpers and the rules name it by its full path, so it must be at exactly `/usr/bin/monitorflow-agent`, owned by root:

```
install -o root -g root -m 0755 monitorflow-agent /usr/bin/monitorflow-agent
useradd --system --home-dir /var/lib/monitorflow/agent --shell /usr/sbin/nologin monitorflow
install -d -o monitorflow -g monitorflow -m 0700 /var/lib/monitorflow/agent
install -d -o root -g root -m 0755 /etc/monitorflow
install -o root -g monitorflow -m 0640 agent.toml /etc/monitorflow/agent.toml
install -o root -g root -m 0440 monitorflow-agent-sudoers /etc/sudoers.d/monitorflow-agent
visudo -cf /etc/sudoers.d/monitorflow-agent
```

Start from `deploy/config/agent.example.toml`. The file name in `/etc/sudoers.d` must stay free of dots, or sudo skips it. Do not create `/var/lib/monitorflow/sources-backup/`: the helper does, as root[^0032].

From your init system, run `monitorflow-agent --config /etc/monitorflow/agent.toml` as `monitorflow`, with `MONITORFLOW_ENROLLMENT_TOKEN` in its environment the first time, so the token never stays on disk[^0007]. Everything works but the `systemd` sensor and self-updates, which fail and leave the running version in place: update with a new binary at the same path, then restart it.

## Sudo rules

Package updates and source configuration need root. The agent runs them with `sudo -n` and fixed command lines, and the rules the installer writes to `/etc/sudoers.d/monitorflow-agent` allow those exact lines and nothing else. Without them, privileged commands fail with sudo's "a password is required", and the command shows as failed.

What the rules allow:

| Command | Used by |
| --- | --- |
| `monitorflow-agent packages update` | `apply_updates`: every kind, all packages, security only or named packages |
| `shutdown -r +1` | `apply_updates` with `allow_reboot` |
| `monitorflow-agent sources apply`, `… sources revert` | `configure_sources`, `revert_sources` |
| `monitorflow-agent self-update` | `self_update`: installs a signed agent release (below) |
| `monitorflow-agent snapshots inspect`, `… snapshots take`, `… snapshots delete` | The storage part of each package report, `take_snapshot` and `delete_snapshot`: local snapshots before a change (below) |

There are no wildcards. The agent never runs `apt-get` or `dnf` through sudo: the `packages update` helper reads the kind of update and the package names on stdin, refuses names that are not package names (options, paths, shell characters), and runs `apt-get update` and the upgrade, or `dnf upgrade`, itself[^0036]. Security-only updates use `dnf upgrade --security` on dnf, and on apt the packages whose candidate comes from a `-security` suite.

## Snapshots before a change

When `/` is on LVM (thin or classic) or ZFS, MonitorFlow snapshots the system volumes (`/`, `/usr`, `/var`, `/opt` and `/boot`, whichever are on LVM or ZFS) before an update campaign updates it and before an AI remediation change, on a server that is not a Proxmox VM or container MonitorFlow knows of[^0094]; those get a Proxmox snapshot instead. Nothing needs setting up. The Rollback line of the server page says in advance what a change would get, like "Snapshot first: LVM snapshot of / and /var (the thin pool has 40.0 GiB free)", or why rollback is limited, like "/ is on plain ext4".

The `snapshots` helper reads only a snapshot name on stdin and decides the rest itself: which volumes, the size of a classic LVM snapshot (20 % of the volume, at least 1 GiB), and whether there is room. It refuses when a volume group lacks that space, or when a thin pool or ZFS dataset has less than 10 % free. Its snapshots carry the LVM tag `monitorflow` or the ZFS property `monitorflow:managed=yes`, and it never deletes a snapshot without them. `sudo monitorflow-agent snapshots inspect` prints what it sees.

Snapshots are deleted automatically: classic LVM snapshots after a day, as they slow every write and become invalid once full, the others after 7 days, and only the newest 3 are kept. Rolling back is manual, as it needs a reboot: the server's Updates tab lists the snapshots with the commands for each.

Hosts installed before snapshots have sudo rules without the `snapshots` lines, and self-update does not change them: run the install command again to get them. The Rollback line says so.

## Self-update

An administrator can update agents from MonitorFlow instead of running the installer again[^0053]. The `self_update` command names a version and where to download it. The agent downloads the release, checks its signature against the release keys built into it and refuses anything else, whatever served it; it refuses older versions unless the command allows a downgrade. Then:

1. `sudo -n /usr/bin/monitorflow-agent self-update` checks the release again as root, writes the new binary next to `/usr/bin/monitorflow-agent`, keeps the running one as `/var/lib/monitorflow/self-update/previous` and renames the new one into place.
2. It starts the watchdog in a transient unit, `monitorflow-agent-self-update`, from the previous binary. The watchdog restarts `monitorflow-agent` and waits for the new agent to connect (5 minutes by default).
3. If the new agent does not connect in time, the watchdog puts the previous binary back and restarts the service. The command then fails with "rolled back to …".

The outcome is in `/var/lib/monitorflow/self-update/status.json`, and the watchdog's log is in `journalctl -u monitorflow-agent-self-update`. `/var/lib/monitorflow/self-update` belongs to root; the helper refuses it otherwise.

**Starting an update.** On a server's page, administrators of its customer see "Update agent to …" when the server holds a newer release than the agent runs; the Commands tab follows the update and shows "rolled back to …" if it came back. The API takes the same command: `POST /api/servers/{id}/commands` with `{"kind": "self_update", "version": "0.3.0"}`, and optionally `"allow_downgrade": true` and `"health_timeout_s"` (30 to 3600).

**Many servers at once.** An update campaign can update agents instead of packages: choose "The MonitorFlow agent" and a version in the new campaign form (`agent_version` in `POST /api/campaigns`). Waves, maintenance windows and the failure threshold work as for package updates; a rolled-back server counts as failed.

**Automatic updates.** On by default[^0055]. Every 5 minutes the server moves online agents to the newest stable release it holds, never newer than the server itself, never to a pre-release and never backwards. Each site starts with one agent, as a first test; once it is updated and connected again, the site's other agents follow, a quarter of the site at a time. A server is updated automatically at most once per version: one that rolled back stays on its version until an administrator updates it or a newer release comes. When 2 updates of a release have failed (the threshold is a setting), its rollout halts everywhere and waits for a global administrator. Relays are updated the same way and first: an agent behind a relay waits until the relay runs the target version ([Installing a relay](deploy-relay.md#self-update)).

Updates › Automatic updates (global administrators) shows the version being rolled out, its progress per site, and a halted rollout with the reason and a "Resume" button; global administrators can turn automatic updates off for the whole server, pause them all at once, or change the failure threshold there. Under Admin › Sites, the site's administrators can override the server-wide setting for one site: "Server default", "On" or "Off". `auto_update.enabled` in `server.toml` (or `MONITORFLOW_AUTO_UPDATE_ENABLED`) wins over the web interface and locks the switch there. Updates are audited as `agent.auto_update`, the start and halt of a rollout as `auto_update.rollout_started` and `auto_update.halted`, changes as `settings.auto_update`, `auto_update.resume` and `site.agent_auto_update`.

**Where agents download from.** The command lists, in order: the mirror listener of the agent's relay, when the relay mirrors and reported its `mirror.public_url` (relays copy the two newest releases the server holds, each cycle of their mirror sync, and check their signature first), the server's own copy at `<mirror_public_url>/agent/<version>/` when it holds that version, then `agent_download_url` (for GitHub, the release of that version; a self-hosted URL as it is, skipped by the agent if it serves another version). For sites without Internet access, give the server a copy of each release on its mirror listener, from a directory holding the release files:

```
docker compose cp ./monitorflow-0.3.0 server:/tmp/release
docker compose exec server monitorflow-server agent-release import --dir /tmp/release
docker compose exec server monitorflow-server agent-release list
```

The import checks the signature first. The web interface offers the newest version the server holds.

**Servers installed before self-update** lack the sudo line above: run the installer once more, which installs the new rules with the new binary. Until then `self_update` fails with "a password is required" and nothing changes. A self-update replaces the binary only; when a release changes the unit or the sudo rules, its notes say to run the installer.

## Pointing a server at the mirrors

On the central server, set `mirror_public_url` to the URL this server reaches the mirror listener at, for example `http://mirror.example.net:7480`. Then an administrator of the server's customer runs:

```
POST /api/servers/{id}/sources   {"mirrors": [{"name": "debian-12"}], "mode": "replace"}
DELETE /api/servers/{id}/sources
```

The first request points the server at the mirrors, `current` snapshot by default. The second restores the sources the server had before. Follow the result with `GET /api/commands/{id}`. A failed post-check (`apt-get update` or `dnf makecache`) reverts the server on its own and reports the error. The server must be able to reach `mirror_public_url`, unless its agent connects through a relay that serves the mirrors.

Behind a relay with mirroring enabled (`[mirror] enabled = true`), set `mirror.public_url` in the relay's configuration to the URL the servers of its site reach the relay's `mirror_listen_addr` at, for example `http://relay.example.net:7480`. The relay reports it to the server with the snapshots it holds, and `GET /api/relays/{id}` shows it as `mirror_url`. Sources then point at the relay when it holds every snapshot requested (for `current`, any snapshot of the mirror), and at `mirror_public_url` otherwise[^0037]. The choice is made when the command is queued: if the relay later drops the snapshot or stops mirroring, send the sources again.

## Windows

The Windows agent is the same binary built for `x86_64-pc-windows-msvc`[^0047]. CI uploads it as the `monitorflow-agent-windows-x86_64` artifact, and its MSI package as `monitorflow-agent-windows-x86_64-msi`. It runs as the service `MonitorFlowAgent` under its virtual account `NT SERVICE\MonitorFlowAgent`, which has no administrator rights.

### Install with the MSI

The package[^0050] does every manual step below: binary, service, failure actions, folders and ACLs, and it registers the `MonitorFlowAgent` event source. In an elevated PowerShell, with a token from the web interface:

```powershell
msiexec /i monitorflow-agent-0.3.0-x64.msi /qn /l*v install.log `
  UPSTREAM_URL=https://relay.example.com:7443 ENROLLMENT_TOKEN=mfe1.…
```

| Property | Effect |
| --- | --- |
| `UPSTREAM_URL` | Writes `%ProgramData%\MonitorFlow\agent.toml` with it, unless the file exists, and starts the service. Without it, nothing is written and the service is not started: copy `agent.windows.example.toml` from `%ProgramFiles%\MonitorFlow`, then `Start-Service MonitorFlowAgent` |
| `ENROLL_URL` | Optional `enroll_url`, needed behind a relay |
| `ENROLLMENT_TOKEN` | Optional single-use token. The MSI log shows asterisks in its place. Remove it from `agent.toml` once connected (below) |
| `REMOVE_DATA=1` | On uninstall, also deletes `%ProgramData%\MonitorFlow`: configuration, identity and logs |
| `AI_COMMANDS_FROM_WEB=1` | Let administrators turn AI commands on and off from the server's page, on a new install or an upgrade: sets `[commands] from_web = true` in `agent.toml`, as `ai-commands from-web on` does[^0098] |

An invalid URL makes the install fail and roll back; `install.log` has the custom action `WriteAgentConfig` failing. The package is not signed yet, so Windows shows an unknown publisher.

The package refuses to install when someone other than an administrator made `%ProgramData%\MonitorFlow` or put something in it first, as any user of the server can: the install stops and rolls back (msiexec exits with 1603). Look at what the folder holds and who owns it (`Get-ChildItem -Recurse -Force "$env:ProgramData\MonitorFlow" | Get-Acl | Format-Table Path, Owner`), remove it if it is not yours, or after checking it give it to the administrators with `icacls "$env:ProgramData\MonitorFlow" /setowner *S-1-5-32-544 /T`, then install again.

Installing a newer package upgrades in place: the service restarts with the new binary, and `agent.toml` and the identity are kept. `msiexec /x monitorflow-agent-0.3.0-x64.msi /qn` (or Apps and Features) removes the binary and the service and keeps `%ProgramData%\MonitorFlow`, so a reinstall keeps the same identity. When the service stops on an error, the Application event log has the reason, from the source `MonitorFlowAgent`.

The package does not install the Windows Update service yet[^0050]: add it by hand as in [Windows Update service](#windows-update-service).

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
# Scripts: the same as the configuration, protected on its own.
icacls "$data\scripts.d" /inheritance:r /grant:r "SYSTEM:(OI)(CI)F" "Administrators:(OI)(CI)F" "NT SERVICE\MonitorFlowAgent:(OI)(CI)RX"
```

**Self-update on Windows.** The agent hands a downloaded, checked release to `MonitorFlowUpdate`, which checks it again, renames the running `monitorflow-agent.exe` to `monitorflow-agent.previous.exe`, puts the new one in place and restarts `MonitorFlowAgent`; if the new agent does not connect in time it renames them back. The MSI installs the update service; by hand, create it as in [Windows Update service](#windows-update-service). Its log, `%ProgramData%\MonitorFlow\update\update.log`, shows each step. The MSI still records the version it installed: a later MSI upgrade works as usual, and repairing the older MSI would put its binary back.

Start from `deploy/config/agent.windows.example.toml`. Its paths assume `%ProgramData%` is `C:\ProgramData`. Use single-quoted TOML strings for paths, so backslashes are not escapes. `%ProgramFiles%` lets only administrators write by default, which keeps the binary as safe as a root-owned `/usr/bin/monitorflow-agent`. The agent also protects what it writes itself: the private key, the command journal and the directories it creates get a DACL for `SYSTEM`, `Administrators` and their owner only, whatever the parent allows. At start, it logs a warning when the key's DACL allows anyone else.

### First connection

A service has no shell to pass `MONITORFLOW_ENROLLMENT_TOKEN` from. Put the token in `agent.toml` for the first start instead, which the MSI's `ENROLLMENT_TOKEN` does: the ACLs above make the file readable only by the service and administrators. Remove the token once the log says `identity loaded`.

```powershell
Start-Service MonitorFlowAgent
Get-Content "$data\agent\logs\agent.log" -Wait
```

As a service, the agent logs to `<data_dir>\logs\agent.log`. At 16 MiB the file moves to `agent.log.1`, replacing the older one. If the service stops at once, for example because the configuration is invalid, the System event log has the control manager's event 7024 with service-specific error 1, and `agent.log` has the reason when logging had started. `Stop-Service MonitorFlowAgent` stops the agent cleanly. `sc.exe delete MonitorFlowAgent` removes the service.

### Log files

The "A log file gets a line" check reads only files in the folders listed in `log_dirs`, which is empty on Windows: there is no common folder of text logs. List the folders in `agent.toml`, give the service read access to them, and restart the agent:

```powershell
# In agent.toml, under [sensors]: log_dirs = ['D:\Logs\MyApp']
icacls D:\Logs\MyApp /grant "NT SERVICE\MonitorFlowAgent:(OI)(CI)RX"
Restart-Service MonitorFlowAgent
```

The agent opens log files so that their program can still write, rename and delete them. It follows a file renamed or truncated by its rotation, reads UTF-8 and UTF-16 files with a byte order mark, and resumes where it stopped after a restart. Paths start with a drive letter: shares (`\\server\share`) and wildcards are not supported, so logs named by date, like IIS's, cannot be watched yet[^0085].

Make the first connection and run the agent through the service only. Started from an elevated console, the agent writes its identity as `Administrators`, and the service can then not read it. If that happened, delete `$data\agent\tls` and connect again with a new token. Without the `service` argument, `monitorflow-agent.exe` runs in the console until Ctrl+C. That is meant for `sensors probe`, for example `monitorflow-agent.exe sensors probe port --param port=3389`.

### Windows Update service

Updates, update reports and self-updates go through a second service, `MonitorFlowUpdate`[^0049][^0053]. The MSI installs it; these steps are for a manual install. It is the same binary started as `update-service` under LocalSystem, because the agent's account cannot install updates. It accepts typed requests from the agent only, on the named pipe `\\.\pipe\MonitorFlowUpdate`, whose access list admits `SYSTEM` and `NT SERVICE\MonitorFlowAgent` and nobody else. It reads no configuration file. In the same elevated PowerShell:

```powershell
sc.exe create MonitorFlowUpdate DisplayName= "MonitorFlow update service" start= auto `
  obj= LocalSystem binPath= "`"$bin\monitorflow-agent.exe`" update-service"
sc.exe failure MonitorFlowUpdate reset= 86400 actions= restart/60000/restart/60000/restart/60000
Start-Service MonitorFlowUpdate
Get-Content "$data\update\update.log"   # "update service listening"
```

The service logs to `%ProgramData%\MonitorFlow\update\update.log`, rotated at 16 MiB like the agent's log. Without it the agent runs as before, but it sends no update report, logs a warning every 5 minutes, and fails `apply_updates` and `self_update` with "the update service is not running". The two services can be installed in either order. `sc.exe delete MonitorFlowUpdate` removes it.

Updates come from the source the machine is configured for: Windows Update, Microsoft Update, or the WSUS server set by Group Policy, together with its approvals. MonitorFlow never changes that source, and its mirrors do not apply to Windows. Windows keeps its own automatic updates. On servers patched by campaigns, set them to notify or download only (Group Policy "Configure Automatic Updates", option 2 or 3), so installs happen in campaign windows. The server's Updates tab shows the source the agent reads from that policy, like "Updates come from WSUS at http://wsus.example:8530", and warns when the policy contradicts itself, for example a WSUS server named but not turned on[^0086].

### What works on Windows

| Feature | State |
| --- | --- |
| First connection, certificate renewal, stream, inventory | As on Linux; `os_family` is `windows`, the version comes from the registry, like `Windows Server 2022 Datacenter 21H2 build 20348.2849` |
| Sensors | `cpu`, `memory`, `disk`, `port`, `script` (PowerShell or a program), `log`, `windows_service` and `eventlog`[^0048]. `systemd` is refused as "not supported on this platform" in the agent's configuration acknowledgement |
| Update reports | Pending and installed software updates by KB number, with title, classification, MSRC severity, size and restart state, plus whether Windows waits for a restart. Needs the update service (above) |
| `apply_updates`, update campaigns | Through the update service. All pending updates except feature upgrades, `security_only` for the Security Updates classification, or named KB numbers or update IDs (`KB5034441`). Restarts only with `allow_reboot`, one minute after the result. Windows servers may join any campaign: pinned mirror snapshots do not apply to them |
| `configure_sources`, `revert_sources` | Refused by the API for Windows servers, and by the agent with "package sources are not supported on Windows yet" |
| `shell` (AI remediation) | Off until turned on ([below](#ai-commands-on-windows)). Commands run in Windows PowerShell 5.1 as the agent's account, and the server classifies them by rules written for PowerShell[^0084] |

There are no sudo rules on Windows. The only privileged part is the update service, which accepts typed requests from the agent alone.

### AI commands on Windows

AI remediation sends a Windows server PowerShell commands[^0084]. The agent refuses them until an administrator allows them on that server. In an elevated PowerShell:

```powershell
& "$env:ProgramFiles\MonitorFlow\monitorflow-agent.exe" ai-commands on
Restart-Service MonitorFlowAgent
```

The first line sets `[commands] shell = true` in `agent.toml` and leaves the rest of the file as it is; the setting is kept across upgrades. `ai-commands off` takes it back. Either one also undoes what the web interface last set. The server's page shows the change once the agent is back, and gives these lines on its Commands tab.

To let administrators turn AI commands on and off from the server's page instead, run `ai-commands from-web on` the same way, then restart the service; `ai-commands from-web off` takes it back. The MSI's `AI_COMMANDS_FROM_WEB=1` does the same at install[^0098].

Each command runs in Windows PowerShell 5.1 from the system directory, as `NT SERVICE\MonitorFlowAgent`, from `C:\`, with the system `PATH`, and is stopped with everything it started at the time limit. That account is not an administrator: it reads services, processes, the System and Application event logs, disks and network state, but cannot restart most services. To let AI restart one service after approval, grant the account that right on that service, for example with `sc.exe sdshow` and `sc.exe sdset`, adding `(A;;RPWPLCRC;;;S-1-5-80-546755567-1596817566-60379001-3258130523-3598288828)` (the service SID of `MonitorFlowAgent`). Without it, the command fails with "access denied" and the incident goes to a person.

[^0007]: [ADR-0007: Enrollment bootstrap](adr/0007-enrollment-bootstrap.md)
[^0025]: [ADR-0025: Relay forwarding model](adr/0025-relay-forwarding.md)
[^0031]: [ADR-0031: Command channel, package updates and package reports](adr/0031-command-channel-and-updates.md)
[^0032]: [ADR-0032: Package source configuration](adr/0032-package-source-configuration.md)
[^0035]: [ADR-0035: Source configuration commands](adr/0035-source-configuration-commands.md)
[^0036]: [ADR-0036: Package update helper](adr/0036-package-update-helper.md)
[^0037]: [ADR-0037: Relay mirror URLs](adr/0037-relay-mirror-urls.md)
[^0047]: [ADR-0047: Windows agent](adr/0047-windows-agent.md)
[^0048]: [ADR-0048: Windows sensors](adr/0048-windows-sensors.md)
[^0085]: [ADR-0085: The log file sensor on Windows, and positions kept across restarts](adr/0085-log-file-sensor-on-windows.md)
[^0049]: [ADR-0049: Windows Update](adr/0049-windows-update.md)
[^0050]: [ADR-0050: Windows agent MSI](adr/0050-windows-agent-msi.md)
[^0052]: [ADR-0052: Agent releases and the Linux installer](adr/0052-agent-releases.md)
[^0053]: [ADR-0053: Signed releases and agent self-update](adr/0053-agent-self-update.md)
[^0054]: [ADR-0054: Agent install sources](adr/0054-agent-install-sources.md)
[^0055]: [ADR-0055: Automatic agent and relay updates](adr/0055-automatic-updates.md)
[^0063]: [ADR-0063: Agent on any Linux](adr/0063-agent-on-any-linux.md)
[^0064]: [ADR-0064: AI setup in the interface](adr/0064-ai-setup-in-the-interface.md)
[^0073]: [ADR-0073: Reusable install keys per site](adr/0073-reusable-install-keys.md)
[^0084]: [ADR-0084: AI remediation on Windows](adr/0084-windows-remediation.md)
[^0086]: [ADR-0086: Where Windows servers take their updates from](adr/0086-windows-update-source.md)
[^0091]: [ADR-0091: System logs](adr/0091-system-logs.md)
[^0094]: [ADR-0094: Local LVM and ZFS snapshots before a change](adr/0094-local-snapshots.md)
[^0098]: [ADR-0098: AI commands turned on and off from the web interface](adr/0098-ai-commands-from-the-web-interface.md)
