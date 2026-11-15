# ADR-0135: Swagger UI to try the public API

Date: 2026-11-15
Status: accepted

## Context

ADR-0101 left Swagger UI out: a large script, served from a CDN or bundled, when a short page of our own reads the same document. That page explains each operation but cannot send one. People writing a script or a PSA integration want to try a call with their key before writing code, and Swagger UI is what they know for that.

## Decision

**Swagger UI at `/api/v1/swagger/`**, served by the central server next to `/api/v1/openapi.json`, which it reads. "Try it out" is on by default. A person pastes an API key under Authorize; Swagger UI sends it as `Authorization: Bearer`, and nothing else changes: the key's reach, access, areas, rate limit and audit apply as for any script.

**Built into the binary.** The files come from the `utoipa-swagger-ui` crate with its `vendored` feature, so the build downloads nothing and the page loads nothing from elsewhere: it fits the web interface's Content-Security-Policy (`script-src 'self'`). The online validator is turned off (`validatorUrl: none`), so the document is not sent to an outside service, and the key is not kept in the browser after the tab closes (`persistAuthorization: false`).

**No sign-in.** Like the document, the page is public: it holds no data, and every request it sends needs a key.

**The API documentation page stays**, as the page that explains, and gets a "Try it out" button that opens Swagger UI in a new tab.

## Consequences

- Trying a call needs a key and a browser, nothing to install.
- The server binary grows by Swagger UI's files, a few megabytes.
- Swagger UI follows the `utoipa-swagger-ui` crate's releases, updated with the other dependencies.
- ADR-0101's "Not done: Swagger UI" no longer holds; this ADR replaces that line.
