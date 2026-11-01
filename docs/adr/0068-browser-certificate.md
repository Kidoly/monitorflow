# ADR-0068: Browser certificate without a terminal

Date: 2026-10-31
Status: accepted; amends ADR-0006 and ADR-0065

## Context

A browser opening a fresh server on an IP address or internal name met a warning, and trusting Caddy's CA took a `docker compose exec` and a file to carry (simplicity audit, B3). A `.lan` name was worse: Caddy tried ACME for it and never got a certificate, so HTTPS did not work at all.

The way to a trust store has to start before TLS is trusted, and it needs a certificate delivered somehow. That means plain HTTP, which `CLAUDE.md` forbids beyond the mirror exception of ADR-0030 and ADR-0034, hence this ADR.

## Decision

### One setting, chosen by the installer

`WEB_CERTIFICATE` in `deploy/.env` is `acme` or `internal`. It reaches Caddy as `MONITORFLOW_CERTIFICATE`, which imports one of two snippets of the Caddyfile. The installer sets it and says which one it chose and why:

- **`internal`** when no public CA could issue for `DOMAIN`: an IP address, a single label, or a name of a private namespace (`.lan`, `.local`, `.internal`, `.home.arpa` and the like), whatever it resolves to.
- **`acme`** when `DOMAIN` also resolves to a public address from this host. A name in a private range, in shared address space (`100.64.0.0/10`, which Tailscale uses) or with no answer gets `internal`.
- **Unchanged** when a name that could be public does not resolve now. A DNS failure on a re-run must not replace a certificate that works. A server with no setting yet, installed before this ADR, counts as `acme`, which is what Caddy was doing for such a name.

The check runs on every run, so a name that gets public DNS later moves to ACME by running the installer again, and editing `DOMAIN` needs nothing more. `acme` is also the default with no setting, so a new web image started by the updater on an older compose file behaves as before. An existing server gets its setting from the next run of the installer.

There is no flag to force either. The one case the rules miss is a name that resolves publicly on a host the internet cannot reach, and that one solves itself:

### When ACME fails

DNS cannot show whether ports 80 and 443 are open from outside. If there is no certificate two minutes after the stack is up, the installer says so, sets `internal` and restarts the web service. A server with its own CA works and has a way to be trusted; a server with no certificate has neither. The next run tries ACME again, so opening the ports and running the installer is enough. Such a host, on each run, has no certificate for up to two minutes.

This only applies when the web service completes no TLS handshake. A server that already has a certificate never falls back, and renewals stay Caddy's job.

### Port 80

Plain HTTP exists only for what cannot be done without it. It serves:

- the **ACME HTTP-01 challenge**, which Caddy answers before any route;
- the **`/trust/` page**;
- the **public CA certificate**, `/trust/monitorflow-ca.crt`.

Everything else is a 308 to HTTPS. No session, API or setup route is ever served on it. The listener is Caddy's only, routes are an explicit list of two paths, and those exist only with the internal CA: with ACME, `/trust/` redirects too. The HTTPS site serves the same two paths, for browsers that have upgraded the link by themselves. The API listener stays as ADR-0006 has it.

The redirect goes to `DOMAIN`, not to the `Host` of the request: the only name the certificate covers.

The page is one static file, not a route of the web interface: nothing of the application is on port 80, and it needs no API. `web-start.sh`, the web image's entry point, fills it in and copies the certificate to a public directory once Caddy has created the CA, because only a script can compute the fingerprint: Caddy's templates have no SHA-256 and browsers refuse WebCrypto to a plain HTTP page. The CA's key stays in `/data`, never in that directory.

### The page trusts the network until compared

The page and the certificate arrive unencrypted, so anyone on the path can alter both. **Until an administrator compares fingerprints, trusting this page means trusting the network.** The page says that in plain words.

So it shows the SHA-256 fingerprint of the CA, and the installer prints the same one, out of band, from the terminal of the server. The comparison has a limit too: an attacker could leave the fingerprint on the page alone and swap the file. That is why the page ends by naming the stronger check, the fingerprint the browser itself shows for the certificate chain once it is installed, which nothing in transit could have changed.

The setup link never goes through this page. A token in a plain HTTP URL, or in the fragment of one, could be read by an altered page, so the installer prints the trust page first and the setup link after, and the page sends the reader back to that link.

### HSTS only with ACME

HSTS moved to the ACME snippet. A browser that saw it refuses plain HTTP to that name for a year, which closes the trust page, and it removes the way past the warning, so a CA that changes on a reinstall would lock the name out of that browser.

### The page guides by itself

The page is plain language, with the steps for Windows, macOS, iOS, Android, Linux and Firefox, the one for the visitor's device already open, and a line that watches for the browser to start trusting the server: a request with `no-cors` to the HTTPS site fails exactly as long as the certificate is refused. Nothing of the page depends on that script.

### Tests

`deploy/smoke-test.sh` starts the compose with the internal CA and checks what port 80 answers, path by path. `deploy/install-server-test.sh` runs the internal CA path for real, from the installer's output to an HTTPS request verified against the certificate from the page.

No public name exists in CI, so ACME runs against Pebble, a real ACME server that validates port 80 of the test host. Two things stand in for the internet, and neither is in production: the host's `/etc/hosts` makes the name resolve publicly, and the test release's web image is the real one with two more lines in its Caddyfile, `acme_ca` and `acme_ca_root`. That image is named in `images.json` like any other. There is no ACME setting in the compose file or the installer.

What this does not prove: a real public CA, its rate limits, and validation from outside over real DNS.

Rejected:

- **An `MF_CERTIFICATE` flag or a prompt.** The name already decides; one more question is one more thing to get wrong.
- **Caddy's two issuers in one `tls` block, ACME then internal.** Simple, but a passing failure at a renewal would replace a working public certificate by one browsers refuse. The fallback must apply only to a first certificate.
- **A setting that sticks once chosen.** A name that gets public DNS would need an edit to benefit.
- **The trust page in the Next export.** Port 80 would have to serve the application's assets.
- **The setup link through the trust page.** See above.
- **A fingerprint served by the Rust server.** It would need the Caddy volume, and an unauthenticated plaintext route to something other than Caddy.
- **Renaming the CA `MonitorFlow`.** Caddy's `pki` option does this for a new CA only, so existing servers would differ and the page would need both names.

## Consequences

- A server on any name has a browser certificate without a terminal step, and the installer says which kind.
- Port 80 is a second plaintext exception, next to the mirror listeners. It carries no credentials and no monitoring data.
- An existing server keeps working after an update through the web interface; the trust page and the fix for `.lan` names come with the next run of the installer, as the compose file changes.
- The internal CA is in `caddydata`, which is not in backups (ADR-0058). A restore on a new host creates another CA, the installer prints its fingerprint, and each device must trust it again.
- `--uninstall --purge` removes the CA as well, with the same effect.
- A device must trust the CA once, which is one visit per device. Managed fleets can push `monitorflow-ca.crt` with their own tools.
- A public name behind a closed firewall re-tries ACME on each run of the installer, and uses up failed validations at the public CA if run often.
