# ADR-0086: Where Windows servers take their updates from

Date: 2026-11-03
Status: accepted

## Context

ADR-0049 left Windows servers on whatever update source they are configured for, and named reporting that source as a follow-up. On Linux, the Updates tab of a server says where its packages come from: its own sources or a mirror of this install, through its relay when it has one (ADR-0074). On Windows the same section said "the sources it came with" and "Mirrors carry Linux packages only", which answers nothing: an administrator taking over a fleet cannot see which servers follow WSUS and which go to Microsoft, nor catch a policy that names a WSUS server without turning it on.

Windows has no single place that states the source. The Windows Update client decides from its policy, which Group Policy writes under `HKLM\SOFTWARE\Policies\Microsoft\Windows\WindowsUpdate` (and its `AU` subkey), and MDM (Intune) under `HKLM\SOFTWARE\Microsoft\PolicyManager\current\device\Update`. The COM API can list registered services, but only the update service of ADR-0049 calls COM, and its default selection does not say why it chose a server. Anyone may read both policy keys.

## Decision

### What the agent reads

The agent reads the policy itself, as its own account, each time it collects a package report (ADR-0049), and sends the result in a new field of `PackageReport`, `windows_update_source` (`WindowsUpdateSource`, additive). The update service and its pipe protocol are unchanged.

| Value | Key | Use |
| --- | --- | --- |
| `WUServer` | `WindowsUpdate` | the WSUS server |
| `WUStatusServer` | `WindowsUpdate` | where the client reports, shown when it differs from `WUServer` (case and a trailing slash aside) |
| `UpdateServiceUrlAlternate` | `WindowsUpdate` | where downloads come from instead of WSUS |
| `UseWUServer` | `AU` | WSUS is used only when it is 1 |
| `NoAutoUpdate`, `AUOptions` | `AU` | the automatic update mode |
| `DoNotConnectToWindowsUpdateInternetLocations` | `WindowsUpdate` | the ban on Internet locations |
| `SetPolicyDrivenUpdateSourceForQualityUpdates`, `…ForFeatureUpdates` | `WindowsUpdate` | 0 takes that kind of update from Microsoft while on WSUS (Windows Server 2022 and Windows 10 2004 and later) |
| `DeferQualityUpdates`, `DeferQualityUpdatesPeriodInDays`, `DeferFeatureUpdates`, `DeferFeatureUpdatesPeriodInDays` | `WindowsUpdate` | Windows Update for Business deferrals |

MDM fills only what Group Policy leaves unset, with the same names where MDM has them: `UpdateServiceUrl` (counted only when it is an `http` or `https` URL, as MDM's placeholder is not) stands for `WUServer` with `UseWUServer` 1, `AllowAutoUpdate` maps to `AUOptions` (0 notify, 1 to 4 install, 5 off), and the deferral days need no separate switch. The other MDM areas, Windows Update for Business rings, update rings of Intune that apply through other CSPs, and the services registered in the client (Microsoft Update) are not read: they would need COM or knowledge of Intune internals, and they do not change the answer to "WSUS or Microsoft".

### How it is interpreted

The agent sends what Windows makes of these values, not the values, because the rules are Windows' rules, the agent is where they are tested against a real registry, and the server needs no Windows knowledge to store or show them:

- **WSUS** when `UseWUServer` is 1 and `WUServer` is set, with its URL.
- **WSUS without a server** when `UseWUServer` is 1 and `WUServer` is missing or blank. Windows cannot use WSUS then.
- **Microsoft** otherwise. A `WUServer` set while `UseWUServer` is not 1 is ignored by Windows; it is still sent, so the page can say so.
- **Automatic updates**: `NoAutoUpdate` 1 turns them off whatever `AUOptions` says. `AUOptions` 2 notifies, 3 and 7 download and wait, 4 installs on a schedule, 5 lets local administrators choose. Any other value, and no value, is reported as unset: Windows decides.
- The status and download servers, the ban on Internet locations and the policy-driven source matter only with WSUS, and are sent only then.
- Deferrals matter only for updates that come from Microsoft, since WSUS approvals decide the rest. They are sent for Microsoft, and for a kind of update that policy takes from Microsoft while on WSUS; otherwise zero. Days are capped at 365.
- **Managed** says whether any of these values is set at all.

The "dual scan" of Windows 10 1607 to 1909, where deferral policies next to WSUS sent quality updates to Microsoft unless `DisableDualScan` was set, is not modelled: those builds are out of support, and the server releases MonitorFlow supports use the policy-driven source instead.

### What is sent

Nothing beyond what the page shows. URLs are trimmed, lose control characters and any user information (`http://user:password@host` becomes `http://host`), and are cut to 512 characters. The keys hold no secrets otherwise. A policy that cannot be read (any error but a missing key) gives a report without the field, and a warning in the agent's log.

### Storage and API

Migration `20261113110000_windows_update_source` adds a nullable `servers.windows_update_source` JSON column, replaced with each package report: a report without the field clears it, so an agent that goes back to an older version shows nothing rather than a stale source. The server checks the values again (a known origin, URLs within 512 characters and without control characters, at most 365 days); an unknown origin, from an agent newer than the server, is dropped.

`GET /api/servers/{id}/sources` and each server of `GET /api/sites/{id}/sources` gain `windows_update_source`: `{"origin": "microsoft" | "wsus" | "wsus_without_url", "wsus_url", "status_url", "download_url", "managed", "automatic": "off" | "notify" | "download" | "install" | "local_choice" | null, "no_internet", "quality_from_microsoft", "feature_from_microsoft", "quality_deferral_days", "feature_deferral_days"}`, `null` on Linux and until a report carries it. Site rows also gain `os_family`. No new route: the field belongs where a server's sources already are, with the same access rule, so tenancy is unchanged.

### On screen

On a Windows server's Updates tab, the "Package sources" section becomes **Update source**, with a label (WSUS, Microsoft, WSUS without a server, Not known yet) and plain sentences:

- "Updates come from WSUS at http://wsus.example:8530." or "Updates come from Microsoft (Windows Update)."
- Warnings for contradictions: "Policy names the WSUS server … but doesn't turn it on, so Windows ignores it.", WSUS turned on without a server, and a kind of update taken from Microsoft while policy forbids Windows Update Internet locations.
- Then the details: the download and status servers when they differ, the automatic update mode ("Windows installs updates on its own schedule, outside update campaigns."), "No Windows Update policy is set: Windows uses its own defaults.", and deferrals.

Quality updates are called **monthly updates** on screen. Without a reported source the label is "Not known yet", and the page says it appears with the next list of updates from an up-to-date agent, never a guess.

**Fleet view**: the package sources table of a site page lists Windows servers with the same label and sentence, so a site's sources read at a glance. A fleet-wide filter was not added: a site is how administrators already look at sources, and the server listing would need the column joined on every page for one Windows question.

## Consequences

- Reading the source adds three registry opens to each package report, every six hours by default.
- The source arrives with the update list, so it is missing while a scan fails, for example when WSUS cannot be reached. Sending it apart from the scan would need its own message and retry, for the one case where the scan error in the agent's log already says the same.
- Old agents, and Linux servers, show no update source; a Windows server shows "Not known yet" until its agent is updated.
- The interpretation lives in the agent: a fix to it reaches a server when its agent updates.
- ADR-0049's follow-up on the update source is done. Driver updates and a per-update timeout remain.
