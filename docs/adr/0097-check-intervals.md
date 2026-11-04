# ADR-0097: How often checks run

Date: 2026-11-16
Status: accepted

## Context

The owner asked to choose how often each check runs: every hour for a slow script, every 30 seconds for a port that matters. The pieces were already there underneath. Every row of `sensor_configs` carries an `interval_ms` that the agent applies (ADR-0012, ADR-0016), bounded from 1 s to 24 h. But the built-in checks (CPU, memory, disk, disk activity) always ran every 60 s from server code, the web interface only showed the interval, and every check it added got 60 s.

Changing the interval also changes things around it that were tuned for one minute:

- The threshold engine restarts a pending timer after a silence of more than `max_gap`, 10 minutes (ADR-0017). A check every hour has a gap of an hour between every two samples, so a threshold "for 5 min" could never fire. ADR-0017 listed this as a consequence to fix once intervals vary.
- A sensor's timeout defaults to 10 s, fine every minute but short for an hourly script, which could take longer and has the time.
- The server page reads the latest value with an instant query, which VictoriaMetrics answers from the last few minutes. An hourly check would show "No data yet" most of the hour.
- The short chart ranges sample every 30 s or 2 min, so an hourly check draws one isolated point or none.
- Offline detection (ADR-0072) must not mistake a quiet check for a server that is down.

The load test (ADR-0090) measured 433 samples a second from 1,000 servers at 60 s, with VictoriaMetrics and the server far from their limits.

## Decision

**Choices.** The interface offers 10 s, 30 s, 1 min, 5 min, 15 min and 1 h, and "Other" for any number of seconds, minutes or hours in the same bounds. The words are "every 30 s", "every 1 h". No cron-like schedule or time of day: a check measures something steadily, and "every" covers what was asked.

**Bounds: 10 s to 24 h.** The API refuses anything shorter than 10 s. At 10 s, every check of 1,000 servers would send six times the measured load, about 2,600 samples a second, which a single VictoriaMetrics takes and the load test's notes already consider safe; 1 s would be 60 times, beyond what was measured, and few metrics change meaningfully faster than every 10 s. The agent keeps accepting 1 s, so rows written earlier keep running, but they must be raised to 10 s when they are next edited. 24 h is the existing limit of the agent and the schema.

**Where it is set.** Same layers as thresholds:

1. Built-in: the built-in checks every 60 s, in server code.
2. Site: `sites.check_interval_ms`, NULL for the built-in interval, sets how often the site's built-in checks run. `PUT /api/sites/{id}/check-interval` with `{"interval_ms": 300000}`, or `null` to go back. The effective views (`GET /api/sites/{id}/sensors` and `GET /api/servers/{id}/sensors`) carry `check_interval_ms` and `check_interval_origin` (`default` or `site`).
3. Each check: its row's `interval_ms`, on a site or a server, as before. A check added from the interface starts at the site's interval. Changing a built-in or site check for one server writes that server's own copy of it, which a server row already meant.

A schema trigger bumps the site's sensor revision when its interval changes, so agents get the new configuration at the next poll, like any other row change (ADR-0016). A single company has one site, so it sees one setting, "Built-in checks run every …", on its server and site pages; an MSP sets it per site. There is no server-wide default beyond the built-in one: one more layer would be one more place to look, for a need nobody stated.

**Thresholds keep their meaning, and the screen says what they do with slow checks.** Durations are not refused or rewritten. Instead:

- The engine's allowed gap for a series is `max(max_gap, 2 × interval)` of the check that writes it, found by the longest sensor name prefixing the metric, as the web interface already matches them. An hourly check keeps its pending timer from one sample to the next and can lose one sample without restarting it. The engine reloads a server's cache when its sensor revision moves, not only its threshold revision.
- So a threshold opens after its duration rounded up to whole intervals, at the first sample that proves it: "for 5 min" with hourly checks opens at the second bad check in a row. With a duration of 0 it opens at the first bad check, as before.
- Where an alert's delay is longer than its duration, the interface says it in a sentence next to the alert, such as "Checked every 1 h, so this alert opens up to 2 h after the problem starts.", and when the interval is chosen. Refusing the combination would block a reasonable choice (hourly disk checks with the built-in 5-minute disk alert); silently changing the duration would make the alert say something it does not do.

**Timeouts scale with the interval.** Without a timeout of its own, a check gets a sixth of its interval, at least 10 s and at most 60 s, and never longer than the interval: 10 s up to a 1-minute interval, 50 s at 5 minutes, 60 s from 6 minutes. The function is in `monitorflow_sensors` and the agent uses it, but the server also sends the result explicitly, so agents released before this get it too. A row's own timeout still wins.

**Latest values and charts.** `GET /api/metrics/query` takes `lookback` in seconds (1 s to 2 days): the last value within that window, with `last_over_time`. The server page asks for two intervals of each check. Chart steps were already at least the interval for long ranges (ADR-0020); short ranges now do the same when the interval is longer than their step, with the same rollup as long ranges, and the chart says how often the check runs when that leaves few points.

**Offline detection is unchanged.** It follows the stream and the agent's heartbeat every 30 s (ADR-0072), never samples, so a server whose checks run hourly is online as long as its agent is connected.

**Who.** Operators of the site, as for every sensor and threshold row; viewers see the intervals. Changes are audited: `site.check_interval` with the value before and after, and `sensor_config.update` already records a row's interval.

## Consequences

- An hourly check alerts later than a 1-minute one. The interface says by how much, and a shorter interval on the checks that matter is the fix.
- A threshold's pending timer survives a silence of up to two intervals, so a check every hour that misses one sample still alerts; one that misses two starts again, as a 1-minute check does after 10 minutes.
- Copying a site or built-in check to one server to change its interval stops that server from following later changes of the site's parameters for that check. The server's row shows as "This server only", and "Use the site's" removes it.
- 10 s on every check of every server is six times the measured load; it has not been measured itself.
- Rows below 10 s written through SQL or before this keep running and show their interval; editing them needs a value of at least 10 s.
- Dashboards (ADR-0095) still read their latest values with VictoriaMetrics' default look-back; panels on hourly checks may show "No recent value" between checks until they pass `lookback` too.
