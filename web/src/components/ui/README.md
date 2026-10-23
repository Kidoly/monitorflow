# UI components

The shared pieces of the design handoff (`docs/design/`). Each file has a doc comment with the details; tokens come from `src/app/tokens.css`.

| Component | Use |
|---|---|
| `StatusLabel`, `Label` | Status as a tinted word. `<StatusLabel kind="server" value="online" />`; free words with `<Label tone="warn">`. |
| `Button`, `ButtonLink`, `buttonClass`, `fieldClass` | `variant` `default` / `k` (black) / `danger`, `size` `small` / `large`, `active` for an applied filter. `ButtonLink` is `.btn-k` on `<a>`. |
| `PageHeader`, `RefreshLine` | Title line: title, status, the "Updated …, refreshes every 15 seconds" line, actions. `detail` for 26 px. |
| `StatTiles` | Clickable counters; `compact` for the grey fact tiles of detail pages. |
| `FilterBar` | Native selects, toggles and search, written to the URL with `history.replaceState`. |
| `DataTable`, `PrimaryCell`, `useUrlSort`, `problemsFirst` | List tables: sortable `<th aria-sort>`, sub-lines, row actions, expanded rows, Load more 50, skeleton rows; `bare` inside a `TableBox`. |
| `FactsBox` | The two-column facts list of detail pages. |
| `TableBox` | A titled box around a `bare` `DataTable` ("Incidents on this sensor", "Timeline"). |
| `Breadcrumb`, `Tabs`, `TabPanel` | Detail page navigation. |
| `InlineConfirm` | In-place confirmation with a reason and a danger button naming the target. |
| `SecretField` | A secret shown once, with Copy. |
| `RelativeTime` | `<time>` reading "12 s ago", exact time in the title. `time.ts` also has `formatDuration` ("2 h 52 min") and `formatDay` ("10:12 today"). |
| `Banner`, `LoadingBlock`, `EmptyState`, `ErrorState`, `StaleBanner`, `NotFound`, `NoPermission` | Page states (`09-page-states.html`). |
| `Page` | The 20 px column of a page's blocks inside `<main>`. |
| `TimeChart` | SVG line chart with thresholds, markers, band, keyboard cursor and legend toggles. |

Polling: `usePolledData` in `src/lib/usePolledData.ts` (15 s, keeps the last payload, `stale` on failure). URL state: `src/lib/urlState.ts`; scope (`?customer=`): `src/lib/scope.ts`. Tests mock `next/navigation` with `src/test/navigation.ts`.
