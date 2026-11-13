# ADR-0124: Inline scripts of the web export allowed by hash

Date: 2026-11-12
Status: accepted; amends ADR-0014

## Context

The static export (ADR-0014) has inline scripts in every page: the theme
script of the root layout, Next's bootstrap, and the page's own
`self.__next_f.push` payload, split into several scripts. The Caddyfile
allowed them with `script-src 'self' 'unsafe-inline'`, which lets any
injected script run and takes away what the CSP does against XSS. Nonces
need a server that renders each response, which a static export has not.

One header listing the hashes of every page was tried: with Node 26 the
export has about 130 distinct inline scripts, a 7 KB header that grows by
about three hashes per page. Browsers and Caddy take it, but a proxy in
front with the usual 4-8 KB buffer for response headers (nginx's
`proxy_buffer_size`) would answer 502, and the list would have to travel
from the build to the proxy's configuration.

## Decision

- `npm run build` ends with `web/scripts/csp-hashes.ts`. It adds to the
  `<head>` of every page in `out/`, right after the charset and before any
  script, `<meta http-equiv="Content-Security-Policy" content="script-src
  'self' 'sha256-…' …">` with the hashes of that page's inline scripts,
  about eight.
- The proxy's header keeps `script-src 'self' 'unsafe-inline'` along with
  the rest of the policy (`frame-ancestors` and the like, which a `<meta>`
  cannot set). A browser enforces every policy it is given: an inline
  script runs only if both allow it, so only the page's own do.
- The build fails on inline event handlers or `javascript:` URLs, which a
  hash cannot allow, and on a page whose `<head>` it cannot find.
- `style-src` keeps `'unsafe-inline'`: React writes `style` attributes, and
  an injected style cannot run code.

## Consequences

- Only the inline scripts the build wrote run. Injected markup that comes
  before the `<meta>` could escape it, but the pages are static files and
  the head holds nothing from users.
- The policy travels with the files: any proxy serving `out/` gets it
  without configuration, and it cannot go stale against the build.
- Pages are rewritten after `next build`; `npm run build` is the only
  supported way to build the export.
