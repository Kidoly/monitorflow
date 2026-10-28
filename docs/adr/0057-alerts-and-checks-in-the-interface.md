# ADR-0057: Alerts and checks in the web interface

Date: 2026-10-28
Status: accepted

## Context

The API has every route to change thresholds (ADR-0019) and sensor configurations (ADR-0016), per site and per server, but the web interface called none of them. Tuning a threshold or adding a port check meant an API call with the CSRF header and a server UUID (simplicity audit, B5). The audit's target is an Edit button on every threshold, for one server or the whole site, and an Add a check button, in two clicks.

Both resources are layered: built-in defaults, then the site's rows, then the server's. A row's raw fields (`monitorflow_port_443_up`, `<`, `critical: 1`, `hysteresis`) mean little to someone who wants "tell me when port 443 is closed".

## Decision

**One panel, two places.** A server page tab "Alerts and checks" and a new site page (`/sites/view/?id=`) show the same panel: "When to alert" (the effective thresholds) and "What is checked" (the effective sensors). Each line says where its value comes from: Built-in, Site default, This server only, or Set for this site. The server page's Site fact, the Sites administration list, the Sensors tab and the sensor page link to it. The site page needs the site's own view without a server, so the API gains two read routes built with the same resolve functions as the server views: `GET /api/sites/{id}/thresholds/effective` and `GET /api/sites/{id}/sensors`, with the read role.

**Plain words.** A threshold reads as a sentence: "Warn when a disk is over 85% full, critical over 95%, for 5 min." Ratios are edited as percentages and durations in minutes. The comparison and the hysteresis are kept as they are and not shown. A check's alert (a port closes, a service stops, a log gets a matching line, a script reports a problem) has one fixed trigger, so it is edited as a severity and a duration only. A row whose values were changed through the API reads as a number instead.

**Writes go to one layer.** Saving writes the row of the chosen layer for the metric and instance: it replaces that row if there is one, else adds it. "This server only" is the default on a server; "Every server of the site" writes the site's row, and the form says when this server keeps its own value. Going back to the inherited value deletes the layer's row. Turning an alert off writes a disabled row (ADR-0019 muting) and "Turn back on" deletes it.

**Adding a check.** The form asks for one kind and its few fields, with defaults prefilled (host `127.0.0.1`, channel `System`). It offers the kinds the server's system runs: Linux kinds or Windows kinds, plus port and script. A site, which can mix systems, gets all of them. The sensor name comes from the target (`port_443`, `service_nginx`), and the alert that goes with the check is added at the same layer, on by default. Removing a check added at this layer also deletes that layer's thresholds on its metrics. A check inherited from the site or the built-ins is turned off with a disabled row instead, which is listed with "Turn back on".

**Roles.** Controls show for operators and above whose role covers the site (UX only; the API decides, as before). Others see the same panel read-only, with one sentence saying who can change it.

## Consequences

- Thresholds and checks need neither the API nor the CLI. Every change still goes through the audited endpoints of ADR-0016 and ADR-0019.
- Comparison, hysteresis, interval, timeout and instance-specific rows are not editable in the interface. These are options the defaults cover; the API keeps them.
- There is no HTTP check: no such sensor exists. The list grows with the registry.
