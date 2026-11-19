# ADR-0153: Richer alert rules

Date: 2026-11-18
Status: accepted; extends ADR-0017 (threshold engine), ADR-0019 (threshold endpoints), ADR-0057 (the editor) and ADR-0146 (what the importer moves)

## Context

A threshold today is one shape: a value past a limit for a duration, with a hysteresis (ADR-0017). It answers "the disk is 95% full" but not the questions people move to MonitorFlow with:

- **A value over a window.** "The average load over 15 minutes is above 8": one spike is noise, a sustained one is a problem. A duration needs every sample past the limit, so one sample below restarts it, and a minimum, a maximum, a change or a rate cannot be expressed at all.
- **Compared with last week.** "Twice the usual traffic for a Monday morning": a fixed limit is either too low at peak hours or too high at night.
- **A forecast.** "The disk will be full in 3 days": the incident should open while there is time to act, with the expected date.
- **No data.** A check that stops reporting while its agent is up (a script that hangs, a sensor removed by mistake) is silent today: its series keeps its incident but opens none (ADR-0017, "No data").
- **Combined conditions.** "Processor over 90% and load over 8" on the same server, so neither alone wakes anyone.

Zabbix trigger functions (`avg`, `min`, `max`, `change`, `nodata`, `timeleft`, `forecast`, `trendavg`, `and`) and Prometheus rules (`avg_over_time`, `rate`, `offset 1w`, `predict_linear`, `absent`) say all of these, and the importer marks them "cannot be moved" (ADR-0146).

The engine evaluates incoming samples one by one, in one task that never waits on VictoriaMetrics, and stores only transitions in PostgreSQL. Windows and last week need history the engine does not keep. The design has to say where that history comes from, how a restart, a backlog and a gap behave, and what it costs at about 1,000 agents.

## Decision

### Five kinds of rule, one state machine

A threshold row gains a **kind**:

| Kind | The value judged at each sample | Limits (warning, critical) |
| --- | --- | --- |
| `value` | The sample, as today; the default | In the metric's unit |
| `window` | A **function** of the samples of the last `window_s` seconds (60 s to 1 day): `avg`, `min`, `max`, `change` (last minus first) or `rate` (change per second) | In the metric's unit (per second for `rate`) |
| `compare` | The average of the last `window_s` (5 minutes to 1 day) against the same window `offset_s` earlier, one day or one week: `ratio` (now divided by then) or `difference` (now minus then) | A ratio, or the metric's unit |
| `forecast` | The **time left** until the value reaches `target` on the side `comparison` names (`>=` 1.0: "100% full"), from a straight line fitted over the last `window_s` (1 hour to 7 days) | Seconds left: warn below 3 days, critical below 1 day |
| `nodata` | The **seconds since the last sample** of the series arrived | Seconds of silence (60 s to 7 days) |

Each kind produces one number per evaluation, and that number goes through the state machine of ADR-0017 unchanged: per-severity pending timers on sample time, `duration_s`, hysteresis, the incident taking the highest firing severity, and the `opened`, `escalated`, `deescalated` and `resolved` events. Only the comparison of the number with the limits is fixed by the kind where the kind defines it: time left and silence alarm when low and high respectively (`<=` and `>`), so `comparison` of a forecast names the side of the target instead. A forecast heading away from its target, or flat, never reaches it: its time left is the cap of 10 years, which keeps stored values finite and is never an alarm. One already past its target has 0 s left.

**Several rules on one series.** The precedence key of ADR-0017 becomes `(metric, instance, kind)`. A disk can have its 85% threshold, its forecast and a no-data rule at once, each overridden or muted per site and server like today, and each with its own incident. `incidents` and `alert_states` gain `kind` (`value` for every existing row), and the open-incident index becomes `(server_id, metric, instance, kind)`. Silences, grouping, follow-ups, remediation and integrations key on the incident, so they need no change; a forecast and a threshold incident on the same disk are two incidents of the same check.

**Combined conditions.** A rule of kind `value`, `window`, `compare` or `forecast` may add up to two **conditions** joined with "and": each a metric of the same server, an instance (absent: the same instance as the rule's series, else the series without one), a function (`value`, or a `window` function with its window), a comparison and one limit. At each sample of the rule's series, its warning or critical condition holds only when every added condition holds on the latest value of its own series, no older than the rule's `max_gap` (ADR-0097). A condition without a fresh value does not hold. Only the rule's own series drives evaluation and carries the incident; the conditions are gates, without severities of their own. `nodata` takes no conditions: it fires when samples do not come, and the conditions would be judged on nothing. "Or" stays two rules.

### Where values are computed

**In the engine, from a summary kept in memory.** Each series that a `window`, `compare` or `forecast` rule, or a condition, needs gets a summary of at most 60 buckets per distinct window read (so a 3-minute average is not read from buckets sized for a day's forecast of the same metric): per bucket the count, sum, minimum, maximum, first and last sample. A bucket is the window divided by 60, at least 10 s. Functions read the buckets that fall in the window, so a window edge is precise to a 60th of the window; `change` and `rate` use the first and last samples themselves. A forecast fits a least-squares line through the bucket averages, weighted by their counts. A series used only by `value` and `nodata` rules keeps no summary, as today.

**A window needs enough history.** A summary knows the series from a point in time: the start of what was loaded from VictoriaMetrics, or its first sample. A `window` or `compare` value exists once the summary knows half of the window, and has a sample in it; `change` and `rate` need two samples; a forecast needs samples over a quarter of its window, in at least 3 buckets. Until then the rule judges nothing: the sample moves no timer, so a fresh agent cannot open a forecast incident from its first two disk samples.

**History comes from VictoriaMetrics, outside the engine task.** A history task asks VictoriaMetrics for what the engine needs, at most 4 queries at a time, with the same client, `peer` and `site` matchers and `extra_label` filters as ADR-0018. The engine sends it requests without waiting and takes its answers in the same loop as batches, so the single writer of ADR-0017 stays the only one changing state.

- **Loading a server.** When the engine loads a server's rules (its first batch on this instance after a start, a stream moving between instances, or a reload that adds a rule), it asks for the past window of each metric and kind that needs a summary: one range query per metric for all its instances, with a step of one bucket and the function the rule reads (`max_over_time` for `max`, `min_over_time` for `min`, `avg_over_time` otherwise). The answer fills the buckets older than the first live one; each loaded bucket counts as one sample at its average (its minimum or maximum for those functions). A restart therefore costs a window nothing once the answer is in, typically within seconds.
- **Last week.** A `compare` rule needs the average of its window `offset_s` ago. The task fetches an hour of it ahead, one range query per server, metric and rule (`avg_over_time` over the window at a 5-minute step, every instance), and the engine reads the point at or just before `at - offset_s`. When the first sample past three quarters of that hour arrives, the next hour is asked for. A sample whose past value is not loaded yet judges nothing, as above; a past without data (the server is new, or was down a week ago) never compares, and a ratio to a past of 0 neither.
- **Failures.** A query that fails is logged and counted, never retried in a loop: the next load or the next hour asks again. Windows then fill from live samples, after half a window.

**No data runs on the server clock.** The engine records when the last sample of each series a `nodata` rule watches **arrived** (the server's clock, not the sample's timestamp, so an agent with a wrong clock or sending a backlog is judged on what reached the server). Its 10-second poll gives each such series the silence so far as its value, through the same state machine, and the next sample resolves the incident. A rule for every instance watches the instances seen: live, or in VictoriaMetrics over the last day when the server was loaded (`tlast_over_time`, a VictoriaMetrics function). A rule naming its instance watches that one even if it never reported. Only servers whose stream is on this instance are watched: an agent that goes offline has its offline incident (ADR-0072) and opens no "no data" for each of its checks, and its server leaves the engine with its stream (ADR-0089).

### Restarts, backlogs and gaps

- **Restart.** Timers and open incidents reload from `alert_states` and `incidents` as today. Summaries reload from VictoriaMetrics with the server. A no-data rule does not fire for a server before its silence has lasted the rule's time **since the server was loaded**, so a restart cannot turn every quiet check into an incident, and a check silent before the restart is caught at most one rule's time late.
- **Backlogs.** Samples of a series are judged in their order, on their timestamps, as today: windows and forecasts are computed over the backlog itself, and a sample older than the last one of its series is dropped. A backlog's samples count as arrivals for no data, which they are.
- **Gaps.** A gap longer than `max_gap` restarts pending timers, as today. A window with no sample in it has no value, and a summary drops what is older than its window, so a server back after a day starts its windows over from what it sends.
- **Changed rules.** A reload that changes a rule's window or function rebuilds its summary from VictoriaMetrics. A removed or muted rule resolves its open incident with `threshold_removed`, as today.

### Cost at about 1,000 agents

Assume every server has its disks forecast (4 mounts), 3 window rules, one comparison with last week and 2 no-data rules: about 10,000 series with a summary.

- **Memory:** a summary is about 4 KiB, so about 40 MiB with one window per series; no-data series cost a timestamp each.
- **Processor:** a sample of a series with a summary updates one bucket and reads at most 60; the forecast's fit is 60 multiplications. Negligible next to ingest.
- **VictoriaMetrics:** after a restart, about 5 range queries per server as their agents reconnect, 4 at a time: a few minutes for the whole fleet, during which windows fill from live samples. In steady state, one query per server, metric and comparison rule per hour (about 1,000 an hour), and none for windows, forecasts or no data.
- **PostgreSQL:** unchanged: writes follow transitions, not samples.

### The editor, the preview and the API

**One editor.** The threshold editor of ADR-0057 opens on today's form: warn and critical limits and "for how long". Below it, "Alert on" offers in plain words: "the value" (the default), "the average, lowest, highest, change or rate over a time", "a comparison with yesterday or last week", "when it will reach a value" and "when it stops reporting", and "And also when..." adds a condition. Each choice swaps the few fields it needs, with defaults filled in; the sentence of the rule ("Warn when the average over 15 min is over 8") updates as fields change.

**A live preview.** Under the form, "What this rule would have done over the last 3 days" (1 to 7) replays the history of the server's series through the same code as the engine: the value judged, the limits, and the incidents it would have opened and resolved, with their times. On a site it runs over up to 20 of its servers, by name, and says how many incidents each would have had. `POST /api/v1/servers/{id}/thresholds/preview` and `POST /api/v1/sites/{id}/thresholds/preview` take the rule as it would be saved and `days`; they need the read role, write nothing and are not audited. No data is replayed from the gaps between samples' timestamps.

**Forecast as a recommendation.** Built-in defaults do not change, so no install gets new incidents from an update. The site's and server's alert panels offer "Warn 3 days before a disk is full" (a forecast of `monitorflow_disk_used_ratio` to 100%, learned over a day, warning under 3 days, critical under 1 day) until the site has a forecast rule on disks: Add writes it as a site row, and No thanks writes a disabled site row of the same key, which mutes nothing and stops the offer. The effective views list it as `recommended`, so API clients see and act on the same offer.

**API.** `ThresholdRequest` and `Threshold` gain `kind` (default `value`), `function`, `window_s`, `offset_s`, `target` and `conditions`; a request with fields its kind does not use is refused, so a client cannot believe a field took effect. The threshold endpoints, their validation, 409 on the same `(metric, instance, kind)` and the `threshold.create`, `.update` and `.delete` audit records are otherwise those of ADR-0019, in `/api/v1` and its OpenAPI document. Incidents gain `kind`, and a forecast incident `expected_at`: the time of its last value plus the time left. The incident's sentence (`reason`) reads "Expected to reach 100% in about 2 days", "No data for 12 minutes", "Average over 15 min is above the warning limit of 8", and notifications use the same words.

### What the importer moves

| Source | Becomes |
| --- | --- |
| Zabbix `avg`, `min`, `max(/host/key,period)` | `window` with that function and period: exact |
| Zabbix `last` | `value`: exact (as today) |
| Zabbix `change` | `window` `change` over two of the item's intervals: approximated (Zabbix compares two values, not a period) |
| Zabbix `nodata(/host/key,period)=1` | `nodata` after the period: exact |
| Zabbix `timeleft(/host/key,period,threshold)` compared with a time | `forecast` to the threshold, learned over the period: exact with the linear fit, approximated with another fit |
| Zabbix `forecast(/host/key,period,time)` compared with a value | `forecast` to that value within `time`: exact with the linear fit, approximated otherwise |
| Zabbix `trendavg(/host/key,period:now/h)` | `window` `avg` over the period ending now: approximated (hourly trends against raw samples) |
| Zabbix expressions joined with `and` on one host | One rule with up to two conditions: exact when every part maps |
| Prometheus `avg_over_time`, `min_over_time`, `max_over_time` | `window` with that function: exact |
| Prometheus `rate`, `irate`, `deriv` | `window` `rate`: exact for `rate`, approximated for the others |
| Prometheus `increase`, `delta` | `window` `change`: exact for `delta`, approximated for `increase` (no counter resets) |
| Prometheus `m / m offset 1w > 2`, `m - m offset 1d > x` | `compare` `ratio` or `difference`: exact when both sides use the same window, approximated otherwise |
| Prometheus `predict_linear(m[w], t) > x` | `forecast` to `x` within `t`, learned over `w`: exact |
| Prometheus `absent(m)`, `absent_over_time(m[w])` | `nodata` after the rule's `for` or `w`: approximated for `absent` (any series against each series) |
| Prometheus `a > x and b > y` on the same instance | One rule with a condition: exact when both sides map |

Anything else stays "cannot be moved", with its reason: `or`, more than three conditions, conditions on other hosts, counts and percentiles.

## Consequences

- People get the alerts they had in their old tool, and the disk forecast that most of them wanted, without a second editor.
- The engine now depends on VictoriaMetrics for history, but never waits on it: when it is down, windows fill from live samples after half a window, comparisons with last week judge nothing, and every other rule works as before.
- After a restart, window rules judge nothing for the seconds it takes their history to load. No-data rules wait one rule's time before firing; a check silent before the restart is caught that much late.
- A window edge is precise to a 60th of the window, and loaded history counts each bucket as one sample: a `change` across a restart compares bucket averages, not the exact samples.
- The open-incident index and the threshold key change shape, which every writer of `incidents` uses through the engine's store only (ADR-0017), and the offline task's own rows keep `kind` `value`.
- A forecast is a straight line: a disk that fills in steps (a nightly backup) forecasts from the trend of the steps over the learning window, so a window of a day or more fits such disks better than an hour.
