# ADR-0005: Pin the Rust toolchain

Date: 2026-10-18
Status: accepted

## Context

Local builds used the machine's default toolchain (1.89) while CI installed the latest stable (1.99). Newer clippy lints then failed only on GitHub, twice in the first day. The workspace also advertised `rust-version = "1.85"` without any job checking it.

## Decision

`rust-toolchain.toml` pins an exact Rust version and its components. It is the only place the version is written. CI installs it with `rustup toolchain install`, which reads that file. `rust-version` in `Cargo.toml` equals the pinned channel: the platform ships as binaries and containers, so supporting older compilers has no value for users and would need its own CI job.

## How to bump

1. In a dedicated PR, change `channel` in `rust-toolchain.toml` and `rust-version` in `Cargo.toml`.
2. Run `scripts/check.sh`, fix new clippy lints in the same PR.
3. Merge it before feature branches; open branches rebase on `main` to pick it up.

## Consequences

- Local runs, Claude threads and CI use the same rustc, clippy and rustfmt.
- Branches created before a bump keep their old toolchain until rebased. PR CI runs on the merge ref, so it already uses the new version.
- rust-src and rust-analyzer are part of the pin so editors work without extra setup.
