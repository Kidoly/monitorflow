# Contributing

Thanks for your interest. Before a first pull request can be merged, contributors sign a Contributor License Agreement; the text and process are not published yet and will be added here.

- Open an issue before large changes.
- One pull request per change, under about 500 lines where possible.
- Run `scripts/check.sh` before pushing.
- Architecture changes need an ADR in `docs/adr/`.
- Code in `ee/` is the Enterprise edition, under `ee/LICENSE` (ADR-0100); the rest is AGPL. The CLA covers both.

## Building and testing

```
docker compose -f deploy/docker-compose.dev.yml up -d
cp .env.example .env
cargo build --workspace
scripts/check.sh
```

Requires `rustup`, `protoc`, and Docker. The Rust version is pinned in `rust-toolchain.toml`; rustup installs it on first use.

### Web interface

The web interface in `web/` is a Next.js static export. It needs Node 26 (`web/.nvmrc`) and npm.

```
cd web
npm ci
npm run dev        # http://localhost:3000, /api proxied to 127.0.0.1:7444
npm run lint
npm run typecheck
npm test
npm run build      # static files in web/out
```

`npm run dev` forwards `/api` to the server's API listener (`MONITORFLOW_API_URL`, default `http://127.0.0.1:7444`), so the session cookie stays same-origin. Over plain HTTP, set `api_cookie_secure = false` in the server configuration for development, and create a user with `monitorflow-server user add --username admin --role admin`. In production, a TLS reverse proxy serves `web/out` and forwards `/api`; see `deploy/caddy/Caddyfile`.

### Architecture diagram

`docs/images/architecture.html` is the source of the diagram in the README: a self-contained page with its icons inlined. After editing it, screenshot its `.canvas` element in light and dark at 2x (with the `data-theme="dark"` attribute on `<html>` for dark) into `architecture.png` and `architecture-dark.png`.
