# ADR-0091: System logs

Date: 2026-11-03
Status: accepted; amends ADR-0013 (agent lines in VictoriaLogs) and ADR-0024 (lines are forwarded now)

## Context

MonitorFlow counts lines that match a pattern (`log`, ADR-0024, ADR-0085; `eventlog`, ADR-0048) but never sends a line. When an alert fires, people then open a terminal to read the journal, and AI remediation sees only the server's own connection events (ADR-0040). VictoriaLogs already runs in every install (ADR-0002), with TLS since ADR-0087, and the relay already buffers agent data on disk (ADR-0027). What was missing is a way to get lines from agents into it, under rules that keep 1,000 agents from flooding it and keep each customer's lines apart.

## Decision

### What is forwarded

**Linux: the systemd journal, warning and above** (priorities 0 to 4: emergency, alert, critical, error, warning), every unit and the kernel. No setting: these are the lines people look for when something is wrong, and a healthy server writes few of them. Info and debug lines are left out, since they are most of the volume and rarely what an incident needs. Plain files are not forwarded. Most services write to the journal, and the `log` check already covers files.

**How it is read.** The agent runs `journalctl --output=json --follow --all --priority=warning` and reads its output. The journal's C API would need libsystemd, which the static agent does not link (ADR-0063). `journalctl` ships with every systemd, and its JSON output has been stable for years. Each line keeps `MESSAGE` (a byte array when it is not UTF-8, decoded with replacement characters), `PRIORITY`, `_SYSTEMD_UNIT`, `SYSLOG_IDENTIFIER` (or `_COMM`), `_BOOT_ID`, `_PID` and `__REALTIME_TIMESTAMP`. A JSON line longer than 1 MiB is skipped. If `journalctl` exits, it runs again 30 s later. Without `journalctl` or a journal directory (`/var/log/journal`, `/run/log/journal`), the agent logs it once and forwards nothing.

**Access.** The agent runs as `monitorflow`, which may read only its own journal lines. The unit now has `SupplementaryGroups=systemd-journal`, the group systemd creates for reading the journal. It gives read access to the journal and nothing else. The installer drops that line when the group is missing, since systemd would refuse to start the unit. Self-updates replace the binary, not the unit (ADR-0053), so agents installed before this release lack the group until the installer runs again. Such an agent sees that it cannot open the system's journal files. It then sends one line at start, which shows among the server's logs: "The MonitorFlow agent cannot read the system journal, only its own lines. Add its user to the systemd-journal group … or run the installer again." On another init system, the agent runs as the administrator chose, as before.

**Where it resumes.** As with the positions of log files (ADR-0085), the cursor of the last line handled is saved to `<data_dir>/journal.cursor` every 5 s and at shutdown, through a temporary file and a rename. At start, the agent reads on from that cursor (`--after-cursor`), so lines written while it was stopped or updating are forwarded. If there is no cursor, or it was saved more than a day ago, the agent starts at the end (`--lines=0`), so an agent stopped for weeks does not send its whole history. If `journalctl` refuses the cursor (the journal was removed), the file is removed and the next run starts at the end. Lines that were queued but not sent when the agent stops are lost: the cursor is past them, and the server does not acknowledge batches (ADR-0013).

**Windows: not yet.** Forwarding the System and Application event logs needs a subscription (`EvtSubscribe` with a bookmark), message formatting through each provider's metadata, and a level mapping. ADR-0048 has the reading half of this for counting. It is the first follow-up, kept out of this change so the change stays reviewable. Windows agents send no `LogBatch`, and the server's Logs tab tells Windows servers so.

### Volume: safe for 1,000 agents by default

| Limit | Where | Value | Over it |
| --- | --- | --- | --- |
| Priority | agent | warning and above | not read |
| Rate | agent | 1,000 lines at once, then 10 a second, by the lines' own timestamps | dropped and counted |
| Reading pace | agent | 200 lines a second, by the clock | waits; the journal keeps them |
| Queue | agent | 2,000 lines (8 MiB of messages at most) | reading pauses |
| Message | agent and server | 4,096 bytes | cut |
| Batch | agent | 500 lines, or every 10 s | |
| Batch | server | 1,000 lines | the batch is dropped |
| Rate | server, per agent stream | 5,000 lines at once, then 500 a second, by the clock | dropped and counted |
| Disconnected | relay | on disk with samples (ADR-0027) | oldest dropped |

The agent's rate limit uses the lines' journal timestamps rather than the time it reads them. A backlog written slowly, such as two hours of an outage at one line a minute, then passes whole when the agent catches up, while a burst written at 1,000 lines a second is cut, however it is read. Lines over the limit are dropped, not delayed, since a flood that lasts would otherwise make the logs fall behind for good. The agent counts them in the batch (`LogBatch.rate_limited`). The server turns that count into a line of its own at the place of the gap: "The agent dropped 1,234 log lines: they came faster than 10 a second." Lines the server drops become "The server dropped N log lines of this agent: more than 500 a second." Both lines have priority warning and identifier `monitorflow`, so they show in the server's logs with the default filters.

**Back-pressure.** While the agent cannot reach its upstream, its queue fills and it stops reading the journal. `journalctl` then blocks on its pipe, and the lines stay in the journal until there is room. A server outage of hours loses nothing as long as the journal keeps the lines (systemd's default is 10 % of the file system). The 200 lines a second reading pace keeps a catching-up agent under the server's 500 a second. Behind a relay, `LogBatch` envelopes are buffered on disk like `SampleBatch` ones.

**What 1,000 agents cost.** A typical server writes a few warning lines an hour. The worst case allowed is 10 lines a second per agent sustained, 10,000 a second for the fleet, about 2 MB/s of JSON before VictoriaLogs compresses it. A single VictoriaLogs node ingests several times that. A bad hour on every server at once fills the server's events queue of 100,000 records (ADR-0013) before VictoriaLogs slows down, and the queue then drops and counts as for samples. gRPC compresses nothing today. The batches are text, and per-message compression of the stream is a follow-up for all envelope kinds.

### Protocol

`Envelope.logs = 17` carries a `LogBatch` with repeated `LogEntry` (timestamp, message, priority, unit, identifier, boot ID, PID, source) and `rate_limited`. The change is additive. An older server sees an envelope without a payload and ignores it. An older relay decodes the unknown field away and forwards an empty envelope, which the server ignores. Lines are therefore lost until the relay updates, and nothing breaks.

### Storage: fields and tenancy

Lines go to VictoriaLogs through the same writer and queue as the server's events (ADR-0013), with the same stream fields `peer` and `site`. As for samples, the server sets these from the stream's identity, never from the entry, so an agent cannot write into another server's or site's stream. Behind a relay, the identity is that of the agent the relay speaks for (ADR-0028). A relay's own stream sends no lines; any it sent would be ignored.

| Field | Value |
| --- | --- |
| `_time` | the journal's timestamp, within 10 minutes ahead and 7 days back, like samples |
| `_msg` | the message, redacted and cut to 4,096 bytes |
| `peer`, `site` | stream fields, from the stream's identity |
| `source` | `journal` |
| `priority` | `0` to `7` |
| `unit` | the systemd unit, left out when longer than 256 bytes or holding control characters |
| `identifier` | the syslog identifier, same rule |
| `boot_id` | 32 hex digits, or left out |
| `pid` | the process ID, or left out |

`unit` is not a stream field: VictoriaLogs advises stream fields of low cardinality, and the queries below always name the server's stream first. Hostnames are not stored: `peer` is the server's name, and the page knows its hostname.

**Every query is filtered per customer and site.** The server's logs are read only through `GET /api/servers/{id}/logs`. That route looks the server up with the user's site scope first, so a server outside it is a 404 and nothing is sent to VictoriaLogs. It then builds the LogsQL itself, starting with the stream filter `{peer=…,site=…}` and `source:=journal`. The text filters are quoted, never pasted in. It also passes the same stream filter as VictoriaLogs' `extra_stream_filters` parameter, so a bug in the query builder still cannot select another stream. This is the pattern of the metric proxy (ADR-0018). The tenancy test covers the route.

### Retention

Lines share VictoriaLogs with the server's events and keep the existing "Keep logs, in days" setting (`LOGS_RETENTION`, 30 days by default, ADR-0069). No new setting.

### Secrets in logs

The server redacts every message with the rules of ADR-0040 before it is stored: private keys and PEM blocks, JWTs, bearer and basic credentials, cloud keys, `password=`-style values and high-entropy strings. These rules over-redact rather than miss. It runs on the blocking pool, a few milliseconds per batch of 500 lines. It is a heuristic: a hex secret without a telling key name, or a password printed on its own, is stored as written. The journal on the server already held it, readable by root and the `systemd-journal` group. Forwarding makes it readable to the users who may see that server in MonitorFlow. Nothing is redacted on the agent, and the unredacted text never reaches VictoriaLogs.

### The web interface

A **Logs** tab on the server page shows the server's lines, newest first, 200 at a time with "Show older", for the last hour by default. It has three filters: level (errors and worse, or warnings and worse, the default, which is all that is forwarded), unit (picked from the units seen in that time), and a search, which matches text anywhere in the message without regard to case. Each line shows its time with its level in a word ("Error", "Warning"), then its unit over the message, so messages wrap on a phone. The lines about dropped lines appear where the gap is. The tab refreshes every 15 s on the last hour, and not when looking at another time. An incident's page links to "Logs around this incident", which opens the tab on the 15 minutes before the incident opened until 15 minutes after it resolved, or until now.

**AI remediation** already reads the newest lines of the server's `{peer,site}` stream into its context (ADR-0040, `Limits::log_lines`). The journal lines land in that stream, so the AI reads them with no change, and its redaction applies a second time.

## Consequences

- The agent sends a new envelope kind; an agent of this release talking to an older server or relay loses its lines and nothing else.
- Agents installed before this release forward only their own lines, and say so, until the installer runs again on them.
- VictoriaLogs holds agent lines for the logs retention. Its disk use grows with the fleet's warning rate, which the Settings page's retention bounds.
- The ingest's events queue is shared by server events and agent lines; a flood of lines can delay connection events, and both are dropped and counted when it is full.
- Tests: the cursor across a restart, a refused cursor, a full queue pausing reading, the rate limit and the drop count on the agent with a stand-in `journalctl`; parsing of real `journalctl` lines; validation, identity, redaction and the server's limit; a `LogBatch` through the gRPC stream; buffering in the relay; the logs route's query and its tenancy; the web tab.
- Follow-ups: the Windows System and Application event logs; plain files (wildcards, as ADR-0085 lists); stream compression for all envelopes; alerts on log text (today, a `log` check counts matches in files only); a fleet-wide log search across servers, with the same tenancy filter.
