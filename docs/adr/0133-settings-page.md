# ADR-0133: One Settings page instead of an Administration section

Date: 2026-11-14
Status: accepted; changes the web part of ADR-0029

## Context

ADR-0029 put administration pages under `/admin/` with an Administration entry, which became a sidebar section of 14 links: settings, relays, sites, install commands, users, API keys, notifications, integrations, AI, server update, backups, plugins, certificate authority and license. They are opened rarely but took most of the sidebar, which should hold daily work. Links to these pages exist outside the interface: notifications and integration events already sent, bookmarks, guides.

## Decision

**One entry.** The sidebar keeps Monitoring, Updates and Remediation, and has one Settings item pinned at its bottom, for users with an administrator grant of any kind, as the section was. The API still decides.

**The Settings page.** `/settings/` lists the pages in groups, Fleet (sites, relays, add servers, certificate authority), Alerting (notifications, integrations), Access (users and roles, API keys), AI & extensions (AI, plugins) and System (general, server update, backups, license), with a search on names and descriptions. Each row may show one status line from data the interface already reads; a line that needs someone is tinted warning or critical and puts a badge on the Settings item. No endpoint exists for these lines alone; they are read every 5 minutes and when the page opens.

**Inside a settings page** the sidebar shows the same list, with a way back to the app's sidebar.

**Addresses.** The pages move to `/settings/<page>/`; the old settings page becomes `/settings/general/` and `/admin/tokens/` becomes `/settings/add-servers/`. Mirrors and automatic updates stay at `/admin/mirrors/` and `/admin/updates/`, under Updates. Each old address keeps a page that replaces itself with the new one, query and hash kept: a static export (ADR-0014) has no server redirects. Links the server writes into notifications and integration events use the new addresses.

## Consequences

- A permission change on one page still needs the matching `visible` rule in the Settings catalogue, as it did in the sidebar.
- The old addresses cost one small page each and a client-side hop. They can go once links sent before this change no longer matter.
- Status lines cost about a dozen reads every 5 minutes per administrator with the interface open.
