# MonitorFlow — UI design handoff

Target stack: Next.js static export, React 19, TypeScript, CSS Modules, CSS custom properties on `:root`, no component library, hand-written SVG charts.

## What is in this folder

- `tokens.css` — every colour, size and font as CSS variables, light and dark (`data-theme="dark"` on `<html>`, or `prefers-color-scheme` when unset).
- `screens/*.html` — one self-contained, pixel-reference HTML file per screen (open in a browser). Files ending in `.dark.html` are the same screen with the dark token set. `00-design-system.html` shows every component in isolation. `09-page-states.html` shows loading, empty, error, stale, not-found, no-permission and the phone layout.
- The markup in each screen is the intended DOM: real `<nav>`, `<table>`, `<button>`, `<a>`, `<label>`, `aria-current`, `aria-sort`, `role="tablist"`. Reproduce its structure and class roles in CSS Modules; copy numeric values (sizes, paddings, radii) exactly, do not round them.

## The system in one paragraph

Black on white, Inter 14 px / 1.5, thin #eaeaea borders, no shadows except menus, radius 5 / 6 / 8 px. Left sidebar 240 px, active item is a solid black pill (white on dark). The scope switcher shows only the customer name. Status is never a symbol or a colour alone: it is a word inside a tinted label (light background, dark text of the same hue, contrast >= 4.5:1): Online / Offline / Revoked, OK / Warning / Critical / Unknown, Pending / Approved / Installed / Failed / Rejected, Read-only / Low-impact / High-impact / Forbidden. Tables have 52 px rows, 40 px header, 14 px text, five columns or fewer; secondary facts go in a 12 px grey sub-line under the main cell. Each list page is: title line -> clickable counters (stat tiles) -> filter row (36 px buttons + search) -> one table with row actions at the right -> one grey sentence. Detail pages are: breadcrumb -> title with status label and actions -> a plain two-column facts box (label grey, value black, no extra styles) -> tabs or a big chart -> tables.

## Screen list and routes

| File | Route | Notes |
|---|---|---|
| 01-sign-in | /signin/ | two error states: wrong credentials, server unreachable |
| 02-fleet | / | counters Online / Critical / Warnings, flat table sorted problems-first, healthy rows collapsible |
| 03-server-sensors | /servers/view/?id= | facts box, then the sensor list (Sensor, Status, Last value); tabs Sensors / Updates / Commands |
| 03b-sensor | /servers/sensor/?id=&sensor= | counters, big chart with thresholds and Opened marker, configuration facts, incidents of this sensor |
| 03c-server-updates | /servers/view/?id=&tab=updates | patch workflow: Scan now, counters by status, table with Approve / Reject per row, Apply approved |
| 03d-server-commands | /servers/view/?id=&tab=commands | command history, output expands under the row |
| 04-incidents | /incidents/ | filters in URL, Load more 50 |
| 05-incident | /incidents/view/?id= | same structure as 03b, plus Threshold-when-opened and Timeline, one line for AI remediation |
| 06-admin-relays | /admin/relays/ and /admin/relays/view/?id= | list, then detail: facts box + "Servers connecting through <relay>" |
| 07-admin-sites | /admin/sites/ | grouped by customer, inline New site form, permanent-name note |
| 08-admin-enrollment-tokens | /admin/tokens/ | form on top, success panel below (token shown once, Copy, snippet), table with inline revoke |
| 10-mirrors, 10b-mirror-detail | /admin/mirrors/ | counters, table; detail: snapshots table, runs table with expandable log |
| 11-campaigns | /updates/campaigns/ | counters, campaigns table, servers-of-running-campaign table |
| 12-approvals | /remediation/approvals/ | one table for waiting / running / decided; AI reasoning expands under the row, marked untrusted; tier comes from rules |
| 13-audit-log | /remediation/audit/ | append-only, no edit or delete controls in the DOM |
| 14-users-roles | /admin/users/ | counters, users table with Grants / Disable |

## Interaction notes

Interaction notes — navigation and shell

• Sidebar is a <nav aria-label="Main"> of <a> links; the active link carries aria-current="page" and is drawn as a solid black (dark: white) pill. Open incident count is a <span> inside the Incidents link with aria-label="3 open incidents".
• Scope switcher is a <button aria-haspopup="listbox"> that opens a two-level list (customer → sites). It shows only the customer name; the site filter lives in the filter bar. Choice is stored in the URL (?customer=acme) so every page keeps it and links are shareable.
• Skip link is the first focusable element; it targets <main id="main">.
• Theme: a data-theme attribute on <html> ("light" | "dark" | absent = follow prefers-color-scheme), stored in localStorage. All colours are CSS variables, so components never branch on theme.
• Status is never a symbol: every status, severity and state is a word inside a tinted label (light background, dark text of the same hue, ≥ 4.5:1). The word is the accessible name.
• At < 1100 px the sidebar collapses to icons with tooltips; at < 800 px it becomes a top bar with a menu button and the content stacks.

Interaction notes — tables and charts

Tables
• 52 px rows, 14 px text, five columns or fewer; secondary facts (OS, agent version, relay) are a 12 px grey sub-line under the hostname.
• Sortable headers are <th aria-sort> wrapping a <button>; click toggles asc/desc and the header shows "↑" or "↓" as text. Sort and filters live in the URL query.
• Default order is "Problems first": open critical, open warning, offline, certificate warning, then hostname. Healthy rows follow; at 1,000+ rows the list is virtualised (react-window or a simple windowed slice) under a sticky <thead>.
• Keyboard: Tab moves between links and buttons in reading order; Enter on a hostname opens the server page.
• Relative times: <time datetime="…" title="2026-10-22 14:31:53">12 s ago</time>; the exact time is also in the accessible name.
• Polling: a single useInterval(15 000) per page; on failure the state keeps the last payload and sets stale=true, which shows the banner and leaves the table untouched.

Charts
• Pure SVG in a <figure> with <figcaption> listing series and thresholds.
• Hover readout: onPointerMove over a transparent <rect> covering the plot area; nearest sample by x, a vertical guide line and a readout box with time + one row per series. Keyboard: the plot <rect> is focusable (tabindex=0), ← → move the cursor one sample; the readout text is in an aria-live="polite" region.
• Threshold lines: <line stroke-dasharray="4 3"> plus a <text> label right-aligned above the line ("Warning 80 %").
• Markers (Opened, Resolved): vertical lines with a label at the top; the open window is a translucent band.
• Ticks: 5 y ticks from a nice-number scale; x ticks every 1/6 of the range, HH:MM for ≤ 24 h, DD MMM for 7 d.
• Max 8 series; legend items are <button aria-pressed> that toggle a series (dimmed, not removed).

Interaction notes — forms, confirmations, secrets

• Inline confirmation replaces the trigger button in place (no modal): a bordered box with the consequence sentence, the reason <select>, Cancel and the danger button whose label names the target ("Revoke web-01"). The danger button is disabled until a reason is chosen. Focus moves to the select on open and back to the trigger on cancel.
• Secrets shown once: the token field is a read-only <input> with a Copy button (navigator.clipboard, then the button reads "Copied" for 2 s and announces it through aria-live). The warning banner precedes the field in DOM order.
• Filter bar: 36 px buttons that open native <select>-backed menus, and a search <input type="search">; every change updates the URL with history.replaceState and refetches. "Clear" resets to the scope only.
• Load more: a <button> appends 50 rows and keeps focus on the first new row's first link.
• Responsive: at ≤ 1280 px the facts list goes to a single column and the chart grid to 2 columns; on a tablet charts are 1 column; on a phone only the Incidents list and page are expected to be used, read-only.

Interaction notes — coming next

• Approvals: the AI text block is visually marked "AI-generated, untrusted" and is never rendered as HTML (plain text, pre-wrap). The risk tier label comes from the rule engine response, with the matched rule shown in monospace; the AI never sets it.
• Edit then approve: the command becomes an editable <textarea>; approving re-evaluates the tier server-side and shows the new tier before the final confirm.
• Campaign waves: each wave is a <details> block; progress bars are <progress> elements with a text percentage beside them.
• Mirrors: run logs are a <pre> with max-height and a "Wrap lines" toggle; "Sync now" is disabled while a run is in state running.
• Audit log: no edit or delete controls exist in the DOM at all; filters are the same filter bar component.

## Rules that are easy to get wrong

- `.btn-k` (black button) is also used on `<a>`: declare it with higher specificity than the global `a` colour rule, or scope link colour with `a:not(.btn)`.
- Group headers, topology diagrams, side-by-side boxes of different heights and open confirmation dialogs were removed on purpose; do not add them back.
- No icons for status, no pills for filters, no cards with shadows. Icons exist only in the sidebar (16 px, 1.6 stroke).
