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

### Web interface

The web interface in `web/` is a Next.js static export (ADR-0014). It needs Node 22 (`web/.nvmrc`) and npm.

```
cd web
npm ci
npm run dev        # http://localhost:3000, /api proxied to 127.0.0.1:7444
npm run lint
npm run typecheck
npm test
npm run build      # static files in web/out
```

`npm run dev` forwards `/api` to the server's API listener (`MONITORFLOW_API_URL`, default `http://127.0.0.1:7444`), so the session cookie stays same-origin. Over plain HTTP, set `api_cookie_secure = false` in the server configuration for development, and create a user with `monitorflow-server user add --username admin --role admin`.

## Licence

AGPL-3.0-or-later. See `LICENSE`. Contributions require signing the CLA (see `CONTRIBUTING.md`).
