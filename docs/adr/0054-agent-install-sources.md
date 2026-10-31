# ADR-0054: Agent install sources

Date: 2026-10-27
Status: accepted; implemented in stages (see the end); filling the cache amended by ADR-0061

## Context

The token page shows a one-line install command (ADR-0052) that downloads the release from GitHub, or from the https directory in the server setting `agent_download_url`. Sites whose hosts cannot reach GitHub must therefore run a web server of their own and copy every release into it, although they already run one server every host reaches: the central server, which since ADR-0053 keeps signed releases in a cache for self-updates.

That cache was not enough to install from. It kept only the six files self-updates need, so no `install.sh`, systemd unit, sudo rules or MSI. It is served by the mirror listener, over plain HTTP on port 7480 (ADR-0030), which sites often do not open to every host, and the installer accepts HTTPS only. Hosts often do not trust the certificate of the web interface either: Caddy issues one from its own CA for an IP address or an internal name (ADR-0051), and companies use internal CAs.

Operators want the server to offer the command by default from one of three sources, and to pick another for one command.

## Decision

### Three sources

| Source | Base URL of the files | Trust |
| --- | --- | --- |
| `github` (default) | `https://github.com/Kidoly/monitorflow/releases/latest/download` | TLS to GitHub, and the release signature (ADR-0053) |
| `central` | `https://<web interface>/agent/<version>`, from the server's cache | Checksums in the command, and the release signature |
| `custom` | An https URL the operator gives, today's `agent_download_url` | TLS to that host, and the release signature |

`github` and `custom` produce the commands of ADR-0052, unchanged.

### The cache keeps whole releases

`<mirror_dir>/agent/<version>/` now keeps every file of a release: `SHA256SUMS`, `SHA256SUMS.sig`, `VERSION`, the two Linux binaries, the Windows executable and MSI, `install.sh`, `monitorflow-agent.service` and `monitorflow-agent-sudoers`. `agent-release import --dir DIR` still verifies the signature with the keys built into the server before it reads anything else, then checks every file it keeps against `SHA256SUMS`; any mismatch, or a file not listed, fails the whole import and leaves no trace. It reads only `DIR`, so it works on a server without Internet access, from a release downloaded elsewhere. Only the three signed files are required, plus one binary; the command says which files were missing, and that Linux hosts cannot install from the server without `install.sh`. Self-updates download the same paths as before, and the mirror listener now also serves the other files, which are public and signed like the rest. Relays still copy only what self-updates need (ADR-0053): installs come from the central server.

The cache is filled by `agent-release import` only. ADR-0053 also describes `agent-release fetch` and a background check of `agent_download_url`; they are not built yet.

### Served on the web interface's address

The API listener serves `GET /agent/<version>/<file>` and `GET /agent/latest/<file>` from the cache, without a session, and the reverse proxy forwards `/agent/*` to it as it does `/api/*` (`deploy/caddy/Caddyfile`). So hosts download from the HTTPS address operators already use for the web interface and already open to administrators, through the proxy that already terminates TLS for it.

- The paths are the mirror listener's (`/agent/<version>/`), so one layout and one cache serve self-updates over HTTP and installs over HTTPS. `latest` is the newest version without a pre-release suffix, like GitHub's `latest`, resolved by the server since a static file server cannot. Only the file names of a release are served, from versions whose `VERSION` matches their directory; anything else is 404.
- With an empty cache, every `/agent/latest/…` answers 404 with a plain-text reason naming the import command, so `curl` users and logs see why.
- Not under `/api`: these are public files, not JSON behind a session and the CSRF guard, and a path that reads like the files it serves is easier to give to Ansible or a wiki.

Rejected:

- **The mirror listener.** Plain HTTP on a port many sites do not open to every host; the installer would have to accept HTTP for one source, and the one-liner itself would come over HTTP.
- **Caddy serving the cache directory.** The web container would need the `serverdata` volume, which holds the CA's private key, and Caddy cannot resolve `latest`.
- **The enrollment listener.** It speaks gRPC, and its certificate comes from the built-in CA, which hosts do not trust before they enroll either.

### Trust when hosts do not trust the certificate

The server cannot pin the web certificate's public key: the reverse proxy holds it, the server never sees it, Caddy's certificates from its own CA last hours and are renewed with new keys, and operators may run another proxy. Asking every host to trust an internal CA first defeats a one-line install.

The command pins the content instead. The server reads the checksums of the cached release from its signed `SHA256SUMS` (verifying the signature again), and the `central` command carries the version in its URL and three SHA-256 values: of `install.sh`, of `SHA256SUMS`, and of the MSI.

```
f=$(mktemp) && curl -fsSLk -o "$f" https://mf.example/agent/0.3.0/install.sh \
  && echo "<sha256 of install.sh>  $f" | sha256sum -c \
  && sudo MF_DOWNLOAD_URL=https://mf.example/agent/0.3.0 MF_SUMS_SHA256=<sha256 of SHA256SUMS> \
       MF_SERVER=… MF_TOKEN=… sh "$f"; rm -f "$f"
```

`mktemp` gives a file only the user can replace, so no other local user can swap the script between the check and `sudo`. The installer, given `MF_SUMS_SHA256`, downloads without checking the certificate (still over HTTPS only), refuses a `SHA256SUMS` with another checksum before it reads it, and checks every file against it as before; with OpenSSL 3 it also verifies the release signature. The chain is: the administrator's session on the web interface, which shows the command; the checksums in it; the files. Whoever can change the command could already change any one-liner (ADR-0052). The certificate is not checked even when hosts would trust it, so the command is the same everywhere; nothing secret is downloaded, and the token goes only to the enrollment listener, whose CA the token pins (ADR-0007).

On Windows, the command downloads the MSI with `curl.exe -k`, which ships with Windows 10 1803, Windows Server 2019 and later, and runs `msiexec` only if `Get-FileHash` gives the checksum in the command, in one PowerShell block so a mismatch stops it even when the lines are pasted one by one. On older Windows, copy the MSI to the host and check its checksum against `SHA256SUMS`.

The command names a version rather than `latest`, since its checksums are those of one release. Hosts that trust the certificate can also use `https://<web interface>/agent/latest/install.sh` with `MF_DOWNLOAD_URL=https://<web interface>/agent/latest`, like a `custom` source; an import between two downloads then fails the checksum check rather than mixing releases.

The address in the command is the origin of the web interface in the administrator's browser, since that is the address the reverse proxy answers on and the server does not know it. An administrator who reaches the interface through a tunnel or another name edits the host in the command.

Rejected:

- **`curl --pinnedpubkey`**, for the reasons above: the key is unknown to the server and short-lived.
- **Requiring hosts to trust the certificate.** It stays possible, as above, but would make `central` unusable on exactly the sites it is for.
- **A copy of the CA in the command** (`curl --cacert <(…)`): bash-only, long, and still breaks when the operator's proxy changes CA.

### The switch

**Server-wide default.** A new table `server_settings`, one row, holds `agent_install_source` and `agent_install_custom_url`, both nullable. Configuration wins, as for every server setting: `agent_install_source` (`MONITORFLOW_AGENT_INSTALL_SOURCE`, `github`, `central` or `custom`) and the existing `agent_download_url`, which is the custom URL when it is not GitHub's. A value set in the configuration shows as such in the web interface and cannot be changed there. Without any value the source is `github`, or `custom` when `agent_download_url` was changed, so existing deployments keep their commands. `custom` needs a custom URL.

`GET /api/settings/agent-install` returns the effective values, which of them the configuration sets, and the release `central` would use, for customer administrators, who create tokens. `PUT /api/settings/agent-install` changes them, for global administrators only, since the setting is server-wide; it is audited as `settings.agent_install` with the old and new values. The tokens page shows the setting, editable by global administrators.

**Per command.** `POST /api/tokens` returns, in place of `agent_download_url`, an `install` object: the default source, the GitHub and custom base URLs, and for `central` the version, its path and the three checksums, or `null` with nothing cached. The panel of a new agent token has a source selector next to the relay field, preselected with the default; both the Linux and the Windows commands follow it. With `central` and nothing cached, the panel says so and shows the import command instead; a release without the MSI or `install.sh` shows the same for that platform. The token is still shown once, and the relay field works as before.

## Consequences

- A site without Internet access imports each release once on the server and installs every host from it, over the address and certificate it already has, with nothing to make the hosts trust first.
- The cache also keeps the MSI and the installer files: one more Windows-sized file per release.
- `central` commands are longer than the others and pin one version; a token page opened before an import keeps showing the older one, which still installs.
- The installer gains `MF_SUMS_SHA256`; installers from releases before this one ignore it and need a certificate the host trusts.
- Operators with their own reverse proxy must forward `/agent/*` to the API listener (docs/deploy-server.md).
- `agent-release fetch` and the background check of ADR-0053 are still to be built.

## Stages

1. The cache keeps whole releases; `/agent/…` on the API listener and in the Caddyfile; this ADR.
2. `server_settings`, the configuration key, the settings API and the `install` object of new tokens.
3. `MF_SUMS_SHA256` in the installer; the selector and the setting on the tokens page.
4. The end-to-end test installs in `central` mode from the compose stack after an offline import; guides and roadmap.
