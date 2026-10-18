# ADR-0012: Sensor model and scheduling

Date: 2026-10-19
Status: accepted

## Context

Phase 2 adds sensors (CPU, memory, disk, then services, ports, logs and custom scripts) that the agent runs on an interval and ships upstream in batches (architecture §5). Before writing the sensors we need one shape for all of them and clear scheduling rules. A slow or broken sensor must not delay the others or the stream. A custom script is untrusted input. Sensor and label names end up as VictoriaMetrics label names on the server. `crates/sensors` must stay free of network code (architecture §2).

## Decision

**Trait.** `Sensor::collect(&self, deadline) -> Result<Vec<Sample>, SensorError>`, async, `Send + Sync + 'static`. The sensor has no name of its own: a `SensorConfig { name, instance, interval, timeout, params }` gives it one, so one implementation can be configured several times (`port`/`22`, `port`/`443`). `(name, instance)` is unique per scheduler. `SensorError` is either `Transient` (retry on the next tick) or `Permanent` (configuration or host problem: that sensor stops, logged at error level, until it is configured again).

**Timeouts.** The scheduler enforces the timeout by cancelling the collection. It also passes the deadline to `collect`, because cancellation cannot stop a blocking task or a child process: sensors that use those must bound them by the deadline or kill them on drop. The timeout defaults to 10 s and is never longer than the interval, so a collection is always over before the next tick.

**Scheduling.** Each sensor gets its own task. Its first tick comes after a random delay below its interval, so a fleet started together does not collect in lockstep. Each collection runs in a spawned task, so a panic is contained. Runs of the same sensor never overlap: the next one starts only after the previous one returned, timed out (cancelled and awaited) or panicked. Missed ticks are skipped, never bursted. Errors, timeouts and panics are counted and logged at most once per 5 minutes per sensor and kind, with the number of lines held back. Recovery is logged once. Shutdown cancels the running collections and waits for every task.

**Samples.** The scheduler sets `Sample.sensor` from the configuration, overwriting whatever the sensor put there, and fills an empty instance with the configured one. It then validates each sample: the value must be finite, the timestamp positive, the instance and label values at most 256 bytes with no control characters, and there may be at most 16 labels. Sensor names match `[a-z][a-z0-9_]*`, label names match `[a-zA-Z_][a-zA-Z0-9_]*`, both up to 64 bytes. The label names `sensor` and `instance` and the prefixes `__` and `mf_` are reserved for the server. Invalid samples are dropped and counted.

**Output and drop policy.** Samples go on one bounded channel (1024 by default) with `try_send`. When the channel is full the new sample is dropped and counted: a slow consumer never slows down or blocks the sensors, and since the oldest samples are already queued, the series keep their earlier points. Counters (`collected`, `failed`, `timed_out`, `panicked`, `invalid`, `dropped`) are exposed by the scheduler handle.

## Consequences

- Sensors are small and testable: an async function plus a configuration. `testing::FakeSensor` (feature `testing`) scripts delays, errors and panics for tests.
- Scheduling semantics are tested on tokio's paused clock, without real waiting.
- Dropping new samples rather than old ones keeps the bounded channel simple (`tokio::mpsc` cannot evict). Durable buffering belongs to the upload side and to relays (architecture §2), not here.
- Changing the configuration means building a new scheduler. When the server starts pushing configuration, a reload replaces the scheduler (or gains add/remove operations) without changing the trait.
- The server must keep `sensor`, `instance` and `mf_*` free for its own labels when writing to VictoriaMetrics.
