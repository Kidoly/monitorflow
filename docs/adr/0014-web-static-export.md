# ADR-0014: Web interface as a static Next.js export

Date: 2026-10-19
Status: accepted

## Context

The web interface (architecture §9) must run without a Node runtime next to
the server, talk only to the REST API, and keep authentication in the
server's HttpOnly session cookie (ADR-0006). It needs few dependencies that
a small team can keep up to date.

## Decision

- **Build.** Next.js App Router with `output: 'export'` and `trailingSlash`,
  so every route is a plain `<route>/index.html` that any static file server
  serves. No API routes, middleware or request-time server rendering. Pages
  are client components that fetch after load.
- **Same origin.** The export and `/api` are served from one origin: in
  production by the TLS reverse proxy (example in `deploy/caddy/`), in
  development by `next dev` rewrites, which exist only in development. The
  browser never calls the API cross-origin, so no CORS is needed and the
  cookie stays `SameSite=Strict`.
- **API client.** One typed module, `web/src/lib/api.ts`, with types written
  by hand from `crates/server/src/api`. It sends `credentials: 'same-origin'`
  and `X-MonitorFlow-CSRF: 1` on every non-GET request, and sends the browser
  to the login page on 401. Nothing is stored in `localStorage`.
- **Data fetching.** Plain `fetch` in effects, with polling where screens need
  fresh data (the fleet overview refreshes every 15 s). No SWR or React Query
  until caching across screens is needed.
- **Styling.** CSS Modules plus one global stylesheet of CSS variables. They
  are built into Next.js, so they add no dependency or build step and keep
  styles scoped per component. Tailwind was set aside to keep the
  dependency list short.
- **Tests.** Vitest, Testing Library and jsdom, with `fetch` mocked.
- **Node.** Version 22 LTS, pinned in `web/.nvmrc` and `engines`; CI reads
  `.nvmrc`.

## Consequences

- Client-side auth guards are UX only. Every authorization decision stays in
  the API.
- API types are duplicated by hand. Generating them (e.g. from an OpenAPI
  document) is worth doing once the API grows.
- Serving the export from the Rust server is a later task. The intended path
  is to serve `web/out` from a directory given in the server configuration
  (e.g. `web_dir`), with an `index.html` fallback for each route, on the same
  listener as the API, behind the same proxy. Embedding the files in the
  binary stays possible if single-file deployments need it.
- Without Node at runtime the export cannot set per-request headers. Security
  headers (CSP, `frame-ancestors`) come from whatever serves the files. Next
  emits inline scripts, so the CSP needs `'unsafe-inline'` for scripts until
  hashes or nonces are supported for static exports.
