# ADR-0166: More ways to draw a dashboard panel, and a simpler panel editor

Date: 2026-11-22
Status: accepted; extends ADR-0095 (dashboards) and ADR-0099 (dashboards on a screen)

## Context

Dashboards (ADR-0095) draw four things: lines over time, one figure, a top list and open alerts. The owner's feedback after using them: monitoring dashboards elsewhere draw the same data ten or fifteen ways, many of them easier to read at a glance (a dial, bars per server, a grid of coloured squares, the share of each state, when servers were in trouble over the day), and the panel form asks for everything at once, about forty choices, before showing anything. They want a good result from the defaults, the other settings out of the way, and a preview while choosing.

The constraints of ADR-0095 stay: no query language, every query built by the API, tenancy resolved for the viewer on every read, one request per dashboard, and the screens of ADR-0099 drawn by 2019 browsers.

## Decision

### Kinds that draw, and the data they draw

A panel's `kind` says how it is drawn. Several kinds draw the same data, so the data answer keeps one **shape** per kind of data, named by the base kind:

| Kind | Draws | Data shape | Fields |
| --- | --- | --- | --- |
| `chart` | Lines over time | `chart` | As before |
| `area` | Areas over time, stacked when there is one per server | `chart` | As `chart` |
| `columns` | A column per period, stacked likewise | `chart` | As `chart` |
| `figure` | One number | `figure` | As before |
| `gauge` | One number on a dial between a low and a high end | `figure` | As `figure` |
| `top` | The highest or lowest servers, as a list | `top` | As before |
| `bars` | The same, as horizontal bars | `top` | As `top` |
| `alerts` | Open alerts | `alerts` | As before |

Further kinds follow in their own changes, each with its data shape: a heatmap of the worst servers over time, a table of several metrics per server, a text panel in Markdown, a grid of one square per server coloured by its state or by a value against thresholds, a pie of the servers by state or of a metric's share per server, and a timeline of when each server had open alerts, from the incidents. They are recorded here because they share the decisions below.

Areas and columns stack their series when there is more than one: that is what areas per server are for (memory used, traffic). A stacked chart of a ratio can pass 100 %, so its scale is not fixed to 0 to 100 % as a line chart's is. Columns gather the points of a range into at most 60 periods, each the average of its points, so they stay wide enough to read; the API sends the same points as for lines.

Keeping kinds and data shapes apart means a client reading `/api/v1/dashboard-data` handles four shapes instead of one per kind, and switching a panel between `chart`, `area` and `columns` changes nothing in the API's queries or cache. Clients that only know the first four kinds still read the data of the new ones.

### Display options

Optional fields, which change how a panel is drawn and never what is read:

- `unit`: how values are written when the metric's name says it badly (`ratio`, `bytes`, `bytes_per_second`, `bits_per_second`, `per_second`, `seconds`, `celsius`, `number`).
- `min` and `max`: the scale. Without them, ratios go from 0 to 100 % and other values from 0 (or the lowest value) to the highest value or threshold.
- `thresholds`: `warning` and `critical`, in the metric's unit. They colour figures, gauges and bars (amber, red) and draw dashed lines on charts. Above them is bad, or below them for metrics where low is bad (available, free, up), the same rule the API already uses to pick the worst servers (ADR-0095), so a threshold has no direction to choose.
- `legend`: whether a chart over time names its lines; charts only.
- `color`: the first of the palette's eight colours the panel uses, so two panels side by side can differ.

They are validated like the rest (finite numbers, a scale that goes up, a colour from 1 to 8, a legend only over time), and refused on alerts panels, which have no values.

### No new spec version

The spec stays at version 1. The new kinds and fields are additions: every stored dashboard reads as it did, and no migration rewrites one, as ADR-0095 asks. A version 2 is for a change an older spec cannot be read under; there is none here.

### Drawing

The charts stay our own SVG (`TimeChart`, and the screen's chart), extended with areas and columns, rather than a charting library: ADR-0099 pins the screen to what a 2019 browser has, the existing charts are accessible by keyboard and translated, and the marks needed are a few dozen lines. Gauges are SVG arcs; bars are plain boxes. Colours come from the theme's tokens (`--series-1` to `--series-8`, `--threshold-warn`, `--threshold-crit`, `--chart-grid`), which have light and dark values, so every new panel follows the theme, on a screen too.

### A simpler editor

Making a panel starts from what to see, not from a form:

1. **What to see.** A metric, searched by its plain title, with the common ones first, and which servers (every server of the dashboard by default, a site, a customer or chosen servers).
2. **How to draw it.** Tiles with a small picture of each kind; the editor picks a good one for the metric (a dial for a percentage now, lines over time otherwise) and keeps what it can when the kind changes.
3. **A preview** of the panel with live data as it is changed, through the same data request.

Everything else (title, width, how servers combine, what it watches, order and count, unit, scale, thresholds, colour, legend) is in an **Advanced** section, closed by default, with defaults that make it unnecessary. A new, empty dashboard offers ready layouts to start from, as well as an empty page.

### Screens

Every kind is drawn on the screen of ADR-0099 too, with the same rules: no links, sizes in `em` and `vmin`, no flex `gap`, no ResizeObserver.

## Consequences

- More readable dashboards without queries, through the same API, cache and tenancy.
- API clients see new `kind` values in specs; data shapes grow only when a kind needs a new one.
- The editor shows a result before asking for details; experts still reach every setting.
- Not in this change: free resizing and rows of panels, per-series colours, value mappings and links from panels to other dashboards.

## Alternatives considered

- **A charting library** (as many dashboards use). Heavier pages, its own theming and accessibility, and the newest ones need browsers the screens of ADR-0099 do not have.
- **A `style` field on the four kinds** instead of new kinds. Fewer enum values, but every client would need to know which styles fit which kind; separate kinds with documented data shapes are as easy to read and validate.
- **A spec version 2.** Nothing old needs reading differently.
