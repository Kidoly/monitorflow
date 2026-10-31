# ADR-0065: One-line server installer

Date: 2026-10-31
Status: accepted

## Context

Installing the central server took 11 steps: Docker with two plugins, `git clone`, a `.env` by hand, and minutes of compiling (simplicity audit, B1). Since ADR-0056 a release has images, and since ADR-0057 and ADR-0058 the first administrator and restoring need no terminal work. What was missing is the path that joins them. The audit's target is two steps: one command, then one link.

## Decision

`install-server.sh` is a release asset, so `https://github.com/Kidoly/monitorflow/releases/latest/download/install-server.sh` is stable. It is POSIX sh, for the same reason as `install.sh` (ADR-0052): it is piped to `sh` on hosts we know nothing about.

### What it installs

- **A bundle, not a checkout.** `monitorflow-server-compose.tar.gz`, also a release asset, holds what the stack reads from disk: `docker-compose.yml`, `pg_hba.conf`, `restore.sh` and `.env.example`. A second file rather than a script that embeds them, because the bundle is covered by `SHA256SUMS` and so by the signature, and the installer stays readable.
- **`/opt/monitorflow/deploy`.** The bundle unpacks under `deploy/`, as in the repository. The commands the web interface and the docs show, like `deploy/restore.sh <file>`, then work from `/opt/monitorflow` as they do from a checkout, and the updater (ADR-0056), which reads its project from compose labels, needs no change.
- **`.env`, once.** It has `DOMAIN`, `SERVER_NAMES`, a generated `POSTGRES_PASSWORD`, and the image references of `images.json`, which is where the updater pins them. Every key `name` of `images.json` becomes `NAME_IMAGE`, so a later image needs no change here. `RELEASE_URL` is set when the install came from `MF_DOWNLOAD_URL`, so updates come from the same place. The server creates its other secrets itself, in `serverdata`.
- **Images by digest.** The release's images are pulled by the digests `images.json` lists, one by one, because on a failure `docker compose pull` advises building a service that has `build:`, which a host without the sources cannot do. `build:` stays in the compose file, for checkouts; the installer never builds.

### Checks

The download comes first and changes nothing: `SHA256SUMS`, then `SHA256SUMS.sig` against the keys `release-files.sh` puts in the script, then each file, then that `images.json` is for that version and names the three images by digest. Only then does Docker get installed. A failed signature is final; there is no `MF_ALLOW_UNSIGNED` here, as every release with images is signed. A copy of the script without keys refuses to run. With no OpenSSL 3 on the host the signature cannot be checked, and the installer warns and checks checksums: those come over HTTPS from the source the script came from, so this trusts what the one-liner already trusted.

Docker comes from Docker's official script. It says so first. Where that script does not support the distribution, the installer stops and says to install Docker with the package manager. It does not start guessing package names.

### The public name

`MF_PUBLIC_NAME` wins. Otherwise, on a terminal it asks, with the guess as the default, reading `/dev/tty` because `curl | sh` has the script on stdin. Without a terminal it uses the guess and says so.

The guess is the host's full name if it resolves to a public address, else its primary IPv4 address. A name that resolves only inside a network could not get an ACME certificate, and Caddy would fail on it (`.lan` is the usual case), while an address always works, with Caddy's CA. `SERVER_NAMES` gets the address as well as the name, so agents can use either.

The stack being up does not prove that browsers reach it, so the installer waits for the web interface on this host and warns if it stays silent, which is how a public name without open ports shows.

### Running it again

`.env` decides: when it exists, the server is installed and it is never rewritten, so secrets and edits stay. Data in Docker volumes without `.env` is refused rather than given a new password the database would reject.

A second run brings the stack up as it is, repairing what stopped, then compares the running version with the release:

- the same: it refreshes the bundle;
- newer: it runs `monitorflow-updater update --dir`, the offline path of ADR-0056, with the verified release files, then refreshes the bundle. The updater makes its checks, the backup and the rollback, and nothing is duplicated;
- older: it says so and changes nothing.

This closes a gap of ADR-0056, which left compose file changes to `git pull`: without a checkout, running the installer again is the way. The updater still changes images only.

### Restore and uninstall

`--restore FILE` installs if needed, pulls every image, including the databases', which the updater will not pull, and calls `deploy/restore.sh`. It asks first only when there is data to replace and a terminal to ask on.

`--uninstall` asks on a terminal whether to delete the data; the answer must be the word `delete`. Without a terminal, or with Enter, it keeps the data and `.env`, so that running the installer brings the server back. `--uninstall --purge` deletes without asking, for automation. Docker and the images stay: other things may use them.

### Tests

There are no published images yet, and CI cannot publish any, so `deploy/install-server-test.sh` uses a local registry and release files signed by a throwaway key. The installer has no test path. `MF_DOWNLOAD_URL` is the self-hosted source, a test release names its registry in `images.json` like any other, and the throwaway key reaches the script through the same `release-files.sh`. It runs on a systemd container without Docker, so Docker's real script runs too.

Rejected:

- **A skip-verification or local-images flag for tests.** A flag that disables checks would exist in production too.
- **`get.monitorflow.dev`.** Needs a domain nobody has chosen. A release asset has a stable URL and needs no host of ours.
- **Asking for the admin password in the installer.** The setup link already does this in the browser (ADR-0057).
- **Pinning the new images in `.env` on an upgrade.** Simple, but it would skip the backup and the rollback.
- **`--port` and similar options.** Ports are in `.env`; the installer names a taken port and the setting, and a re-run applies it.

## Consequences

- Installing the server is one command and one link.
- A new install needs a published release with public images, and a release is live for new installs as soon as it is published (`docs/releasing.md`).
- IPv6 addresses cannot be the public name yet: the compose file builds URLs from `DOMAIN` without brackets. A host without IPv4 needs a DNS name.
- A browser warns about Caddy's own CA on an IP address or internal name; the installer says so. Removing the warning is a later task.
- The guess cannot know a NAT's public address. On a terminal the prompt shows it; without one, set `MF_PUBLIC_NAME`.
- The installer repeats a few functions of `install.sh` (downloads and signature checks): each must work alone when piped.
- `/opt/monitorflow/deploy` is fixed: there is no option for it.
