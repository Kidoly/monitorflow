# ADR-0027: Relay store-and-forward buffer

Date: 2026-10-19
Status: accepted

## Context

Architecture §2 says a relay keeps data on disk while its uplink is down and sends it in order once the link returns. ADR-0025 left this out: a relay whose upstream is down refuses its agents, which keep up to 50,000 samples in memory (ADR-0016's uplink queue, about 10 minutes) and lose the rest, and lose everything when they restart. Sites behind a relay are exactly the ones with fragile links, so an outage of hours must not lose metrics.

Constraints:

- Relays do not interpret envelopes (architecture §3, ADR-0025). The server keeps checking which agents a relay may speak for, and a relay can forge data only for agents of its site (ADR-0004).
- The server does not acknowledge envelopes. It tracks sequence numbers per stream and ignores an envelope whose sequence is not above the last one (ADR-0013's stream loop).
- The server keeps samples at most 7 days old (ADR-0013). Batches hold at most 10,000 samples and fit in tonic's 4 MiB message limit.
- One upstream stream per agent stream, and a newer stream for an agent replaces the older one on the server with `ABORTED` (ADR-0025).
- A relay runs on modest hardware; no embedded database or new heavy dependency.

## Decision

**What is buffered.** Only `SampleBatch` envelopes going up. Everything else is either regenerated or meaningless later:

- `Hello` is not a record: the relay keeps the agent's last `Hello` next to its records and sends it at the start of every upstream stream it opens for the agent.
- Heartbeats are dropped while the link is down; a new stream needs none from the past.
- `SensorConfigAck` is dropped: the server pushes the configuration after every accepted `Hello` and the agent acknowledges it again.
- `CommandResult` is not buffered for now. No command reaches an agent while its link is down, and the command lifecycle (expiry, audit of lost results) belongs to the command execution task, which will decide whether a late result is still wanted. Until then a result sent while the link is down is dropped and counted, like a heartbeat.
- Nothing going down is ever buffered: commands and configurations are the server's to resend, and a stale signed command must not run late.

**When.** An agent's batches go to the buffer whenever the relay has no working upstream stream for that agent: the relay's own stream is down (ADR-0025's `upstream_up`), dialing or opening the agent's upstream stream fails on the link (connect, TLS, transport error, the upstream's `UNAVAILABLE`), or the upstream stream fails the same way mid-flight. The relay then retries the upstream with the backoff of ADR-0009, and at once when its own stream reopens. Batches also go to the buffer while older records of the same agent are being replayed, so they stay behind them.

**Agents are accepted while the upstream is down.** This replaces ADR-0025's "refuse new agents": the relay keeps checking what it already checked for every agent, the certificate against the CA and the upstream's CRL (with its `next_update`) and a `Hello` naming the origin of the stream within 10 s: the certificate's ID, or for a downstream relay the origin of the `monitorflow-via` path it sent (ADR-0028). The server's relay policy runs later, when the relay replays the agent's `Hello` with its path. If the server refuses that `Hello` with `PERMISSION_DENIED`, `UNAUTHENTICATED`, `INVALID_ARGUMENT` or `FAILED_PRECONDITION`, the agent's records are deleted and counted as dropped, since they would be refused again, and a connected agent gets the status as ADR-0025 says. An agent of another site can therefore fill the buffer of a relay until the link returns; the size cap bounds that, and the same agent can already send garbage to its own relay today. A relay whose CRL expired still refuses every agent at the TLS handshake, which limits blind buffering to about a day without a fresh CRL (ADR-0025).

**Replay.** For each agent, everything goes through one task in the relay, which is the only writer and reader of that agent's records. When it gets an upstream stream (dial, `Hello`, accepted), it first sends the agent's buffered batches oldest first, then live envelopes; batches that arrive meanwhile are appended behind the backlog. Agents that are not connected but have records (the agent left or the relay restarted) are replayed by the same code with their stored `Hello`; the upstream stream is closed when the backlog is empty, so the server shows the agent online for the duration of the replay only. When an agent connects while its records are being replayed this way, its new stream takes over and continues the replay before its live data. Order is per agent; agents replay concurrently.

**The path is stored with the `Hello` (ADR-0028).** Behind chained relays, a stream's identity is its `Hello` and its `monitorflow-via` path together: the server checks the path against the relays' parents and records the relay the origin is attached to as `via_relay`. Every upstream stream the relay opens for an agent, live or a deferred replay, carries the path of that agent's latest stream: the agent alone when it connects directly, or the downstream relay's path with that relay appended. The path is stored next to the `Hello`, so a replay after a restart sends it too. Records are filed by the path's origin, the ID the server keys streams by, so an agent that moved to another downstream relay replays its older records under its latest path. That is what it would claim if it connected now; the server checks the path at replay time against the parents of that moment, so a relay removed from the chain meanwhile is refused then, not when the records were written. The alternative, one path per record, would split an agent's records across paths and need one upstream stream per path, which the server's one-stream-per-agent rule does not allow.

**Sequence numbers are the relay's on the streams it opens.** Replayed batches must sit between the `Hello` and the agent's live envelopes on one upstream stream, and an agent that restarted numbers from 1 again, so agent sequence numbers cannot be kept. The relay numbers every envelope it sends on an upstream stream from 1, `Hello` included, and drops envelopes whose agent sequence is not above the previous one from the same agent stream, which is what the server did before. Payloads stay unchanged. This amends ADR-0025's "same sequence numbers"; the server only needs them per stream.

**Delivery is at-least-once for buffered records, at-most-once for envelopes in flight.** Without acknowledgements the relay cannot know what the server received. A segment (below) is deleted only once every record in it was handed to an upstream stream. If that stream fails first, the whole segment is sent again on the next stream, so up to one segment can arrive twice. Duplicates are harmless for samples: the same series, timestamp and value. VictoriaMetrics returns one value per timestamp, and the threshold engine ignores samples not newer than the last it saw (ADR-0017). Loss is worse than duplication for metrics, which is why at-least-once was chosen. Envelopes already handed to a live upstream stream (up to the 16-envelope queue plus HTTP/2 buffers) are lost if the link fails, as they are on a direct agent link. Server acknowledgements of batches would remove both the duplicates and this window; they are a follow-up, needed as well to buffer `CommandResult`.

**Timestamps.** Samples keep the agent's timestamps; the relay never rewrites them. The server's 7-day age limit drops older samples, so the buffer's age cap defaults to 7 days and cannot be set higher. VictoriaMetrics accepts old and out-of-order samples within its retention period (30 days in the compose files), so a replay lands where it belongs on the graphs.

**Disk format.** Under `<data_dir>/buffer/`, one directory per origin, `agent-<id>` (the prefix keeps IDs `.` and `..` safe), holding `hello.pb` (the path's length as u16 LE, the path as in its metadata entry, then the last `Hello` envelope; written to a temporary file and renamed, and parsed back with the same checks as the metadata, so an unreadable or forged file drops the agent's records instead of being replayed) and segment files `<n>.seg`, `n` a 20-digit number from one counter for the whole relay, so the smallest is the oldest anywhere. A segment is append-only and holds records:

| Field | Size | Content |
| --- | --- | --- |
| length | u32 LE | length of the payload |
| checksum | u32 LE | CRC-32C of the time and the payload |
| written at | i64 LE | relay clock, milliseconds since the epoch |
| payload | length | the `Envelope` as protobuf |

A segment is closed once it reaches an eighth of `max_bytes`, at most 16 MiB, and a new one opened, so dropping a segment never drops more than about an eighth of the buffer. Per agent, appending and reading never race, as one task does both.

One directory per agent was chosen over one global log with the agent ID in records. Replaying one agent then reads only its own files, and a segment can be deleted as soon as its agent sent it, without waiting for the slowest agent in a shared file.

The checksum is CRC-32C from the `crc` crate, already in the dependency tree through sqlx. A database (sled, RocksDB, SQLite) was rejected: a log only appended, read in order and deleted whole needs none of what they bring, and they add a large dependency and their own recovery rules.

**Crash safety and fsync.** Appends go to the open segment of each agent; a background tick fsyncs the segments written since the last tick every `fsync_interval_ms` (default 1,000 ms; 0 syncs after every record). A crash loses at most that interval of buffered batches, the same order of loss as the agent's own memory queue. Closing a segment also syncs it. At start the relay reads every segment: the first record with a short header, a length above 16 MiB, a short payload or a wrong checksum ends the segment, which is truncated there, logged and synced. Only the last segment of an agent should ever be torn; the check runs on all of them anyway. Empty segments are deleted.

**Bounds.** `max_bytes` (default 1 GiB) caps the total size of the segments. An append that does not fit deletes the oldest segments of the whole relay, whichever agent they belong to, until it fits; a record larger than the cap is dropped. `max_age` (default and maximum 7 days) is checked on every record read, and on a minute tick that deletes segments whose newest record is older. Every drop is counted (records and bytes) and logged as a warning, at most once a minute; the counters are logged with the buffer's state every minute while it is not empty and will be exported as metrics with the relay's other counters.

**Configuration.** A `[buffer]` section in `relay.toml`: `max_bytes`, `max_age` (seconds) and `fsync_interval_ms`. It replaces the top-level `buffer_max_bytes`, which was documented as unused.

## Alternatives rejected

- **Writing every batch through the disk**, also when the link is up. It gives one code path but makes the disk part of every forwarded batch and costs I/O when nothing is wrong.
- **Keeping agent sequence numbers on replay**, with one upstream stream per buffered agent stream. Replayed and live envelopes could not share a stream, and the server replaces an agent's older stream with a newer one, so replay and live data would race.
- **Buffering on the agent instead.** Agents keep their memory queue; a disk buffer on every server costs more to operate than one on the relay of a site, and does not help when a relay sits between them and the cut. The agent's disk buffer stays a separate roadmap item for direct agents.
- **At-most-once (delete a record when read).** Simpler, but a link that fails during a replay would lose up to a segment of backlog each time.

## Consequences

- Agents behind a relay keep connecting during an outage, so ADR-0025's refusal while the relay's own stream is down no longer applies; the paragraph "No disk buffer yet" is replaced by this ADR.
- The server sees relay-assigned sequence numbers for relayed agents. It never relied on agent numbering beyond the stream.
- An outage of a day on a site of 100 agents sending 80 samples a second is about 700 million samples, tens of GiB in protobuf; operators size `max_bytes` to their sites, and the oldest data goes first.
- A replay of a large backlog shows the agent online on the server while it lasts, even if the agent is gone.
- Server acknowledgements are the follow-up that turns duplicates into exactly-once and allows buffering `CommandResult`.
