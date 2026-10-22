# MonitorFlow

Open source monitoring and fleet management platform in Rust: agents on Linux servers, optional relays per site, one central server with a Next.js web interface. Read `docs/architecture.md` before touching protocol, security or storage code. Task list per phase: `docs/roadmap.md`.

## Layout

- `proto/` Protobuf contracts. Changes must be additive (new fields, new `oneof` variants). Never renumber.
- `crates/common` generated gRPC code, TLS, config, shared types
- `crates/sensors` sensor trait and implementations, no transport code (gRPC, TLS, HTTP); probes like a TCP connect are fine (ADR-0021)
- `crates/agent`, `crates/relay`, `crates/server` the three binaries
- `web/` Next.js static export, talks only to the REST API
- `deploy/` compose files, Dockerfiles, packaging
- `docs/adr/` architecture decisions. Add one when you change a decision.

## Commands

```
cargo build --workspace                 # build everything
cargo test --workspace                  # unit and integration tests
cargo clippy --workspace --all-targets -- -D warnings
cargo fmt --all
scripts/check.sh                        # fmt + clippy + test, what CI runs
docker compose -f deploy/docker-compose.dev.yml up -d   # postgres, victoriametrics, victorialogs
```

Web (`web/`, Node 22 from `.nvmrc`): `npm ci`, then `npm run dev`, `npm run lint`, `npm run typecheck`, `npm test`, `npm run build`. CI runs all of them in the `web` job.

sqlx checks queries at compile time against `DATABASE_URL` (see `.env.example`). Run `cargo sqlx prepare --workspace` after changing queries so the build works offline and in CI.

## Conventions

- Rust 2021 on the toolchain pinned in `rust-toolchain.toml` (ADR-0005). Never bump it inside a feature PR. No `unsafe` without a comment explaining why.
- Errors: `thiserror` in libraries, `anyhow` only in binaries' `main`.
- Logging with `tracing`; never `println!` outside tests and CLI output.
- Async with `tokio`; no blocking calls on the runtime, use `spawn_blocking`.
- All new code has unit tests. Network and database code has an integration test under `tests/`.
- Every link is mTLS, including in development. One exception: the mirror content listeners of the server and relays, and relays' fetches from them, are plain HTTP (ADR-0030, ADR-0034), as they carry only public GPG-signed packages, read-only, without credentials or monitoring data. Never add any other plaintext listener.
- Commands sent to agents are signed and risk-classified by rules; the AI's output is untrusted text.
- Audit log is append-only. No UPDATE or DELETE on it, ever.
- Secrets never go in the repository, logs, or AI context. Use the redaction helpers.
- Comments and identifiers in English. User-facing strings in the web interface in English, i18n later.

## Workflow

- One branch and one PR per roadmap task. Branch names: `phase1/enrollment`, `phase2/sensor-cpu`.
- Before implementing a non-trivial task, write a short plan and list the files you will touch.
- Run `scripts/check.sh` before saying a task is done. Report test output faithfully.
- Keep PRs under about 500 changed lines. Split if larger.
- Commits: imperative subject, under 72 chars, body explains why.
