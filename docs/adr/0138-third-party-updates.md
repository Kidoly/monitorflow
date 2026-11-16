# ADR-0138: Third-party updates

Date: 2026-11-16
Status: accepted; Windows details in ADR-0139, Linux details in ADR-0140

## Context

MonitorFlow updates what the operating system's own manager offers: apt and dnf on Linux (ADR-0031, ADR-0036), Windows Update on Windows (ADR-0049). Software installed another way is invisible to it: on Windows, tools and browsers that winget knows; on Linux, Flatpak and Snap apps. They are often the most exposed software on a machine, and administrators patch them by hand or not at all.

Three pieces of work follow this decision: this one sets the shared model (protocol, storage, API, campaigns), ADR-0139 the winget collector and installer on Windows, ADR-0140 the Flatpak and Snap ones on Linux. They must agree on the protocol before any of them lands.

Constraints:

- The protocol is additive (`proto/`). Relays decode every envelope and encode it again, and prost keeps no unknown fields, so a relay older than its agent drops whatever it does not know (ADR-0055, `hello_compat.rs`).
- An agent older than the server ignores the fields of `ApplyUpdates` it does not know and runs what it does know: an OS update.
- Every change as root or LocalSystem goes through a helper that checks the signed command itself (ADR-0108), with fixed command lines and names checked against a grammar (ADR-0036).
- Existing campaigns, single commands and API clients keep their meaning.

## Decision

### Sources

Every pending update has a **source**: `os`, the system's own manager as today, or a **third-party manager**: `winget` (Windows), `flatpak` and `snap` (Linux). The protocol names the managers in an enum, `PackageManager`; the API and the database use the lowercase names above. A manager of another platform never appears in a report and is refused for that platform's servers. A later manager is a new enum value and a new ADR; an agent value the server does not know is ignored, not stored.

OS packages stay exactly where they are: `PackageReport.packages`, `server_packages` and `GET /api/servers/{id}/packages`. Third-party apps get their own fields, table and route. Counting them in with OS packages would silently change every place that reads `server_packages`: pending counts, fleet lists, check recommendations, the AI context.

### Inventory

`PackageReport` gains two fields:

- `third_party` (6): the apps each usable manager knows as installed, as `ThirdPartyPackage`:

| Field | Meaning |
| --- | --- |
| `manager` | the manager |
| `id` | what the manager's update command takes: the winget package identifier (`7zip.7zip`), the Flatpak ref (`app/org.mozilla.firefox/x86_64/stable`), the snap name (`firefox`) |
| `name` | the display name, empty when the manager gives none |
| `version` | the installed version, as the manager writes it |
| `available_version` | the version an update would install, empty when none |
| `scope` | `MACHINE` (winget machine scope, Flatpak system installation, every snap) or `USER` (installs of the account the collector runs as) |
| `origin` | where updates come from: winget source, Flatpak remote, snap channel; empty when unknown |

  An app is identified by `(manager, id, scope)`. It is **pending** when `available_version` is set. Versions are text: winget writes `Unknown` or `< 1.2` for some apps, and nothing compares them. A report holds at most 5,000 apps per manager; the agent keeps the pending ones first.

- `managers` (7): one `PackageManagerStatus` for **every** third-party manager of the host's platform (winget on Windows; Flatpak and Snap on Linux), whether present or not: `state` `AVAILABLE`, `ABSENT` (not installed) or `UNUSABLE` (installed but the agent could not list its apps: not initialised, a source unreachable, an error), its `version` when known, and an `error` line when unusable. An absent or unusable manager has no apps in the report.

Both fields empty means **not reported**: an agent before this decision, or a relay before it on the way, which drops both fields together. The server then shows third-party apps as not known yet, never as "none". A new agent always sends at least one status, so a report with statuses is also how the server knows the agent understands third-party updates (below).

Collection runs with every package report, at the same times (ADR-0031, ADR-0049), and needs no privilege where the manager allows it; ADR-0139 and ADR-0140 say how. A manager that fails is reported `UNUSABLE` and never fails the OS part of the report.

### Commands

`ApplyUpdates` gains:

- `skip_os` (4): leave the OS manager out. False, the default, keeps every older command's meaning: `packages` (1) and `security_only` (2) apply to OS packages only, as before.
- `third_party` (5): a list of `ThirdPartySelection`, `{manager, packages}`. A listed manager updates the named apps, or every pending app when `packages` is empty. A manager not listed is left alone. Each manager appears at most once.

An empty `third_party` with `skip_os` set asks nothing and is refused by the agent and the API.

**Older agents.** An agent before this decision would read a third-party-only command as "update every OS package". The server therefore never sends `skip_os` or a non-empty `third_party` to a server whose latest report has no `managers`: the API refuses such a single command (400 "this agent does not report third-party apps yet"), and a campaign gives such a target no command and the error "agent does not report third-party apps" when it would skip the OS. A campaign that keeps the OS and adds third-party managers still queues on an older agent: it updates the OS, as it would have, and its inventory never showed apps. An agent that receives a third-party selection for a manager the host lacks, or a named app that is not installed, says so in its output ("flatpak: not installed", "not pending: …") and does not fail, as Windows does for updates that are not pending (ADR-0049).

**Order and result.** The agent runs the OS update first, then each manager in the order listed, and stops at the first failure, which fails the command. The output has one block per source. A reboot follows the rules of `allow_reboot` as before, after everything ran.

**Security only.** Third-party managers have no security classification. `security_only` is not applicable to them: a selected manager is included as a whole (or for the named apps), whatever `security_only` says. The web interface states it next to the option.

**Scope.** Only `MACHINE` installs are updated. `USER` entries are listed for information.

### Names

`common::third_party::valid_id(manager, id)` is the grammar per manager, and `common::third_party::check` the check of a whole selection, applied by the API, by the agent before it builds anything, and again by the privileged helper:

| Manager | Grammar |
| --- | --- |
| winget | 2 to 8 segments joined by `.`, each 1 to 32 of ASCII letters, digits and `_+-`, starting with a letter or digit; at most 128 bytes. Covers `7zip.7zip`, `Notepad++.Notepad++`, `Python.Python.3.12` |
| flatpak | `app/` or `runtime/`, an ID of 3 to 255 bytes with at least three `.`-separated elements, each a letter or `_` then letters, digits, `_` (and `-` in the last one), then `/`, an arch (`[a-z0-9_]`, 1 to 32) and `/`, a branch (letters, digits, `._-`, 1 to 64, starting with a letter or digit) |
| snap | 1 to 40 of lowercase letters, digits and `-`, with at least one letter, not starting or ending with `-` nor holding `--`; optionally `_` and an instance key of 1 to 10 lowercase letters or digits |

None of them can start with `-`, hold a space, a path separator or a shell character, so none reads as an option or a file.

### Privileged helpers

The helper that already acts on `apply_updates` acts on the third-party part too, from the same signed command (ADR-0108): on Linux the root helper behind `packages update` (no new sudo line; its spec gains the managers and names, built from the signed payload only), on Windows the update service (ADR-0049) under a step ADR-0139 names. Each runs fixed command lines, listed in ADR-0139 and ADR-0140, with the checked IDs as their only variable part, and never a free-form argument. Neither takes a manager, an ID or an option from anything but the signed `ApplyUpdates`.

### Server

- **Storage.** `server_third_party_packages` (server, manager, package ID, scope, name, version, available version, origin), keyed by the identity above, and `server_package_managers` (server, manager, state, version, error). Each report replaces both whole; one without statuses empties them, so the server shows "not known yet" and stops sending third-party selections. The server cannot tell an older relay on the way from a downgraded agent, and closing the gate is the safe answer to both.
- **API.** `GET /api/servers/{id}/third-party` answers the statuses and the apps (paginated, `?pending=true`), under the rules of `GET /api/servers/{id}/packages`, and is in the public API. `POST /api/servers/{id}/commands` with `apply_updates` takes `include_os` (default true) and `third_party` (`[{"manager": "winget", "packages": []}]`).
- **Campaigns.** A campaign stores `include_os` (default true) and `third_party` with its `ApplyUpdates` spec, and its JSON shows both under `spec`. Every existing campaign is OS only. A named app must fit its manager's grammar (400 otherwise). When the campaign skips the OS, targets of a platform that has none of the selected managers are left out at creation, as they have nothing to do, and creation is refused (400) when none is left. Each target's command carries the selection minus the managers of another platform. A target whose agent does not report third-party apps is handled as above ("Older agents").
- **Automatic updates.** ADR-0055's automatic updates install new agent and relay releases, never packages, so there is no package selection to extend there. Should automatic package updates come, their default is OS only, like campaigns.

### Web

The server's Updates tab lists pending updates by source: the operating system first, then each third-party manager of its platform with its count, or its state when absent or unusable ("Flatpak is not installed"), or "Not known yet" while the agent has not reported. Campaign and update forms get the choice of sources: operating system, third-party apps, or both, OS alone by default.

### Out of scope

- Installing new software, uninstalling, or downgrading: only `MACHINE` installs a manager already lists are updated to the version it offers.
- Pinning third-party apps to a version or holding their updates, including snapd's own automatic refreshes, which keep running on their schedule.
- Per-user installs of other users: the agent sees and updates only machine-wide installs, plus its own account's for information.
- Other managers (Chocolatey, Homebrew, language package managers), and the Microsoft Store source of winget.

## Consequences

- Older agents, servers and relays keep working: a newer agent behind an older relay reports OS packages only, and an older agent never receives a command that would skip the OS.
- Pending counts, recommendations and the AI context still mean OS packages; adding third-party apps to them is a later decision.
- A compromised agent account gains nothing new: the helper updates only installed machine-wide apps named by a signed command, to what their manager offers.
- Third-party updates install what the publisher put in the manager's source, with less vetting than a distribution's; the per-manager choice and OS-only default make that an administrator's explicit choice.
