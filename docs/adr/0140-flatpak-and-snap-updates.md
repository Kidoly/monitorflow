# ADR-0140: Flatpak and Snap updates on Linux

Date: 2026-11-16
Status: accepted; the Linux part of ADR-0138

## Context

ADR-0138 sets the shared model of third-party apps: `ThirdPartyPackage` and `PackageManagerStatus` in the package report, `skip_os` and `third_party` in `ApplyUpdates`, the ID grammar of each manager in `common::third_party`, and the rule that privileged helpers act only on the signed command (ADR-0108). It leaves to this decision how the Linux agent finds Flatpak and Snap, what it reads, as whom, and what the root helper runs.

The supported distributions ship Flatpak 1.12 to 1.16: 1.12.7 on Ubuntu 22.04, 1.12.9 on Rocky and Alma 9, 1.14.6 on Ubuntu 24.04, 1.14.10 on Debian 12, 1.16.0 on Alma 10 and 1.16.6 on Debian 13. snapd is Ubuntu's default, an optional package on Debian, and comes from EPEL on Rocky and Alma; where it can, it runs from its own snap, so its version does not follow the distribution's.

## Decision

### Detection

- **Flatpak** is present when `/usr/bin/flatpak` exists. Only the default system-wide installation (`/var/lib/flatpak`, `--system`) is read and updated.
- **Snap** is present when `/usr/bin/snap` and snapd's socket `/run/snapd.socket` both exist. Snaps are system-wide by nature.

Every package report carries a status for both (ADR-0138): `ABSENT` when not present, `UNUSABLE` with the error line when a read fails, `AVAILABLE` with its apps otherwise. The version is `flatpak --version` without its `Flatpak ` prefix, and snapd's own version from `/v2/system-info`.

### Inventory, as the agent's user

Nothing in the inventory needs root.

**Flatpak**, with `LC_ALL=C`:

```
flatpak list --system --app --columns=application,arch,branch,name,version,active,origin
flatpak list --system --runtime --columns=application,arch,branch,name,version,active,origin
flatpak remote-ls --updates --system --columns=application,arch,branch,version,commit
```

`--columns` exists since Flatpak 1.2. When its output is not a terminal, Flatpak prints no header and separates columns with tabs; this is the same on every version above, captured in `crates/agent/src/packages/fixtures/flatpak/`. The `ref` column of `list` prints `<id>/<arch>/<branch>` without the kind, and `remote-ls` has no `ref` column, so the agent lists applications and runtimes apart and writes the ref itself: `app/org.example.App/x86_64/stable`, the form `flatpak update` takes and ADR-0138's grammar checks. A line the grammar refuses is left out.

`remote-ls --updates` downloads each remote's summary, which Flatpak caches in the agent user's home (its data directory `/var/lib/monitorflow/agent`, which systemd passes as `HOME`): unlike apt and dnf, Flatpak has no metadata a root timer keeps fresh, so the inventory asks the remotes at each report. A remote that cannot be reached makes the whole manager `UNUSABLE` for that report, rather than reporting apps as up to date.

**Snap**, from snapd's REST API on its socket, which any user may read: `GET /v2/snaps` for the installed snaps, `GET /v2/find?select=refresh` for the refreshes the store offers (snapd asks the store), `GET /v2/system-info` for snapd's version and refresh schedule. The API is snapd's documented and versioned interface and answers JSON; `snap list` and `snap refresh --list` print tables meant for people, whose columns have changed between releases. The agent sends plain HTTP/1.0 requests, which snapd answers without chunking and then closes; answers over 8 MiB are refused. A "snap-not-found" error from the refresh query means no refresh. Older snapd without `tracking-channel` gives `channel` instead.

### What is reported

| Field | Flatpak | Snap |
| --- | --- | --- |
| `id` | the ref, `app/<id>/<arch>/<branch>` or `runtime/...` | the snap's instance name, `name` or `name_key` |
| `name` | the `name` column (from the application's data; a derived name for runtimes) | the snap's `title` |
| `version` | the declared version, or the 12-character commit when there is none, as for most runtimes | the snap's version |
| `available_version` | the version the remote declares when it differs, otherwise the new commit (a rebuild, or no declared version) | the refresh's version, or `<version> (revision <n>)` when only the revision changes |
| `origin` | the remote | the channel the snap tracks, like `latest/stable` |
| `scope` | `MACHINE` | `MACHINE` |

Runtimes are reported and updated like applications: they carry the libraries and drivers the applications run on, and most security fixes. Branches and channels are part of the ID or the origin; an update never switches either.

### Per-user Flatpak installations

Out of scope. They live in each person's home directory, which the agent's user cannot read, and only that person can update them (`flatpak --user` as root would update root's own installation). Reading them as root would also mean running a helper per user. ADR-0138's `USER` scope would be the agent account's own installation, which is always empty on a server, so the Linux agent reports none. Other system-wide installations declared in `/etc/flatpak/installations.d` are out of scope for the same reason of rarity; they can be added as a later change.

### Updates, through the root helper

No new sudo line: the same `sudo -n /usr/bin/monitorflow-agent packages update` reads the signed `apply_updates` command (ADR-0036, ADR-0108). Its spec gains `skip_os` and `third_party` (`[{"manager": "flatpak", "packages": [...]}]`), built from the signed payload only and checked with `common::third_party::check` for the Linux platform, which refuses winget, unknown managers, a manager twice and IDs outside its grammar. The helper runs the OS update unless `skip_os`, then each selected manager in the order given, and stops at the first failure.

| Manager | Selection | Command, as root |
| --- | --- | --- |
| Flatpak | all | `/usr/bin/flatpak update --system --noninteractive -y` |
| Flatpak | named | `/usr/bin/flatpak update --system --noninteractive -y -- <refs>` |
| Snap | all | `/usr/bin/snap refresh` |
| Snap | named | `/usr/bin/snap refresh <names>` |

Before naming IDs, the helper reads what is installed (the same `flatpak list` lines, or `/v2/snaps`) and drops the IDs it does not hold, printing `flatpak: not installed: <ids>`; a manager the host lacks prints `flatpak: not installed`. Neither is a failure (ADR-0138). An ID that is installed and up to date is passed on: Flatpak answers "Nothing to do" and snapd has nothing to refresh, both with success. `flatpak update` of an app also updates the runtimes it needs. No ID can start with `-`; `--` is passed to Flatpak anyway, and snap is given only names snapd's own grammar accepts. A named ref was checked in all six distributions' containers to update and exit 0, and an unknown one to exit 1, which the installed check avoids.

`snap refresh` is run without `--ignore-running`: snapd refuses to refresh a snap whose applications are running, and the command fails with snapd's message rather than stopping them.

### Snap's automatic refreshes

MonitorFlow does not disable or hold them. snapd refreshes snaps several times a day by default; that is how snap publishers ship security fixes, and holding them is a choice the administrator makes with `snap refresh --hold`, which MonitorFlow neither undoes nor makes. A MonitorFlow update only runs a refresh sooner. The agent reads the schedule (`refresh.timer`, `refresh.hold`, `refresh.next`) from `/v2/system-info` at no extra cost and logs it; reporting it to the server needs a field ADR-0138 does not have, proposed as an additive change. Flatpak has no automatic updates on servers.

### Reboots

None. Flatpak updates deploy alongside the running version, and running applications keep the old one until restarted. A snap refresh restarts the snap's own services, which is the snap's restart, not the host's; kernel snaps exist only on Ubuntu Core, which MonitorFlow does not support. Neither manager touches the report's `reboot_required`.

## Consequences

- Linux servers list their Flatpak apps, runtimes and snaps with their pending updates, and campaigns can update them with the sudo file unchanged.
- Each report contacts the Flatpak remotes and, through snapd, the snap store. A host without network access to them shows the manager unusable, with the error, instead of out-of-date data.
- A compromised agent account gains nothing: it can still only read, and the helper updates installed system-wide apps named by a signed command to what their remote or channel offers.
- Per-user Flatpak installations stay invisible; an administrator who needs them updated does it per user.
- Tests: Flatpak's output for each distribution is a fixture captured in containers; snapd's answers are fixtures and a fake socket in `crates/agent/tests/snapd.rs`. snapd needs systemd and AppArmor or SELinux, so it does not run in a test container, and the snap path is tested against the fake only.
