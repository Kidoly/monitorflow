# ADR-0013: Metric and log ingest

Date: 2026-10-19
Status: accepted

## Context

Agents send `SampleBatch` envelopes on their stream (ADR-0001). The server must store them in VictoriaMetrics and record events in VictoriaLogs (ADR-0002). Three constraints shape the design:

- A peer is trusted only for its own identity (ADR-0004): an agent, or a relay of the agent's site, must not be able to write series that look like another peer's or another site's.
- Sample content comes from the network and is untrusted: names, values, timestamps and sizes need bounds before they reach storage, where a bad label explodes cardinality.
- Storage can be slow or down. The stream loop must never wait for it, or one VictoriaMetrics outage would stop heartbeats and disconnect the fleet.

## Decision

**Label model.** Every series carries labels set by the server, never by the sample:

| Label | Value |
| --- | --- |
| `__name__` | `monitorflow_<sensor>`, see naming |
| `peer` | the agent ID the stream speaks for: the certificate ID, or the agent a relay speaks for after the relay policy check |
| `site` | name of the site in the peer's inventory row (ADR-0008) |
| `instance` | the sample's `instance` field (mount point, unit, core), omitted when empty |

The names `sensor`, `instance`, `peer`, `site`, `job` and every name starting with `__` or `mf_` are reserved. ADR-0012 already makes the agent refuse `sensor`, `instance`, `__*` and `mf_*`; the server adds `peer` and `site`, which it sets, and `job`, kept unused so the server can add it later without breaking agents. A sample whose `labels` map uses a reserved name is dropped, not rewritten, since a peer sending it is either buggy or trying to impersonate another one. Empty label values are removed, as Prometheus treats them as absent. The site label uses the site's name, which is what users query by; renaming a site would split its series, and no rename exists yet.

**Naming.** The metric name is `monitorflow_` followed by the sensor name: `disk_free_bytes` becomes `monitorflow_disk_free_bytes`. A sensor name follows ADR-0012, 1 to 64 characters of `[a-z0-9_]` starting with a letter, so the result is always a valid Prometheus name. The sensor picks its unit suffix with Prometheus conventions (`_bytes`, `_seconds`, `_ratio`, `_total`). Colons stay free for recording rules.

**Limits.** Per sample: a finite value; a timestamp at most 10 minutes ahead of the server clock and at most 7 days old, so relay and agent backlogs sent after a reconnect are kept; at most 16 labels of its own, names of 1 to 64 characters of `[a-zA-Z0-9_]` not starting with a digit, values (and `instance`) of at most 256 bytes without control characters. These are ADR-0012's rules, checked again because the server cannot trust the agent to apply them. Per batch: at most 10,000 samples; a larger batch is dropped whole. tonic's 4 MiB message limit already bounds the memory of one batch.

**Invalid data never closes the stream.** Invalid samples are dropped and counted per connection and per writer, with a rate-limited log line naming the first reason. Closing the stream would not help: the agent reconnects and resends its backlog, which costs more than dropping. Volume abuse is bounded by the per-batch limit and the message size limit; rate limiting per peer is a follow-up if needed.

**Protocol.** Prometheus remote write 0.1 (protobuf `WriteRequest`, snappy block compression, `POST /api/v1/write`), which ADR-0002 already names. The four protobuf messages are declared with `prost` derives in the server instead of vendoring `remote.proto`; compression uses the pure Rust `snap` crate. VictoriaMetrics' JSON import API was simpler but larger on the wire and specific to VictoriaMetrics, while remote write also works with Prometheus-compatible stores. Logs use VictoriaLogs' `/insert/jsonline`, one JSON object per line, with `peer` and `site` as stream fields. HTTP goes through `reqwest` with rustls on ring, like the rest of the workspace.

**Writers and drop policy.** Each backend has one background writer with a bounded in-memory queue (`ingest.queue_capacity` items, default 100,000). The stream loop only does a non-blocking push: when the queue is full, the new item is dropped and counted. The writer flushes when a batch reaches `ingest.batch_size` (default 5,000) or every `ingest.flush_interval_ms` (default 1,000). Network errors, 429 and 5xx are retried 5 times with backoff from 0.5 s doubling up to 10 s; other statuses drop the batch, as retrying would get the same answer. Counters (accepted, rejected, dropped, written, failed) are logged every minute when they change, as a warning when items were lost, and will be exposed as metrics. At shutdown the writers stop accepting, flush their queues and give up after `ingest.shutdown_timeout` (10 s), logging how many items were lost.

**Events.** The server records stream open and close in VictoriaLogs with the peer, site, role, relay, connection number and close reason. Records contain identifiers only: never certificates, tokens, payloads or URLs with credentials.

## Consequences

- Queries can filter by `peer` and `site` and trust them; a relay can forge data only for agents of its own site, as ADR-0004 already accepts.
- Memory used by ingest is bounded by the two queue capacities, at the cost of losing data during a long storage outage. A disk buffer on the server is not planned; relays buffer on their side.
- The agent's checks (ADR-0012) should also refuse `peer`, `site` and `job`, so a misconfigured sensor fails on the agent, where it is visible, rather than being dropped by the server.
- Changing a limit is a server-only change; agents need no update.
- Writer counters are only logged for now; exporting them as metrics is a follow-up.
