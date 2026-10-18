# MonitorFlow

Open source monitoring and fleet management for Linux servers, written in Rust. Lightweight agents, outbound-only relays for isolated sites, central apt and dnf mirrors with update campaigns, and AI-assisted remediation under human approval.

Status: early development. See `docs/architecture.md` and `docs/roadmap.md`.

## Development

```
docker compose -f deploy/docker-compose.dev.yml up -d
cp .env.example .env
cargo build --workspace
scripts/check.sh
```

Requires `rustup`, `protoc`, and Docker. The Rust version is pinned in `rust-toolchain.toml`; rustup installs it on first use.

## Licence

AGPL-3.0-or-later. See `LICENSE`. Contributions require signing the CLA (see `CONTRIBUTING.md`).
