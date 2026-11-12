# ADR-0111: The server installer always checks the release signature

Date: 2026-11-12
Status: accepted; amends ADR-0065

## Context

ADR-0065 let the server installer go on without OpenSSL 3, which it needs to verify `SHA256SUMS.sig`, with a warning and the checksums only. The reasoning was that `SHA256SUMS` comes over HTTPS from the place the script came from, so it trusts what the one-liner already trusted.

That holds only when both come from the same place. With `MF_DOWNLOAD_URL` the script may come from GitHub and the files from a mirror, and ADR-0053 does not trust a mirror. The mirror serves `SHA256SUMS` along with the files it covers, so whoever controls it chooses the image digests of `images.json`. The installer writes those to `deploy/.env` and runs them, the updater included, which mounts the Docker socket: root on the central server. A security audit (2026-11) found this on hosts with OpenSSL 1.1, such as Debian 11, Ubuntu 20.04 and RHEL 8.

The same audit found that the installer, before an update, wrote each name `SHA256SUMS` lists to `$tmp/release/<name>` without checking the name, and before checking the file, so `../` in a name wrote outside that directory.

## Decision

- **OpenSSL 3 is required.** Without `openssl pkeyutl -rawin`, the installer stops before Docker or anything else is installed, says that OpenSSL 3 is needed and which distributions have it. Always, not only with `MF_DOWNLOAD_URL`: a copy of the script saved and run later, or fetched from somewhere other than where the files are, is in the same position, and one rule is easier to explain than two. There is no option to go on without it, as there is no `MF_ALLOW_UNSIGNED` (ADR-0065).
- **Names are plain file names.** A name in `SHA256SUMS` that is empty, starts with `.`, contains `..` or contains `/` stops the installer before anything is downloaded under it. `release-files.sh` writes only the plain names of a flat directory, so a release never has one.

The agent installer (ADR-0053) keeps its warning and `MF_REQUIRE_SIGNATURE=1`: it runs on every monitored host, many of them older, and installs one binary, not images that run with the Docker socket.

## Consequences

- The central server cannot be installed on Debian 11, Ubuntu 20.04 or RHEL 8 without installing OpenSSL 3 first. The supported distributions of ADR-0053 (Debian 12, Ubuntu 22.04, RHEL 9 and later) all have it.
- A signed release served by a mirror can no longer write outside the installer's release directory, even signed by a release key.
