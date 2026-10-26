# ADR-0052: Agent releases and the Linux installer

Date: 2026-10-26
Status: accepted

## Context

Installing a Linux agent meant building it, then following the manual steps of `docs/deploy-agent.md`: binary, system user, directories, configuration, sudo rules, systemd unit and the token on the first start (ADR-0007). Windows has had an MSI since ADR-0050, but only as a CI artifact. Operators want one command per host, from the token page or from Ansible. GitHub Releases is the natural host, but sites without Internet access must be able to serve the same files themselves. Architecture §7 plans `.deb` and `.rpm` packages; they need repositories and signing keys per distribution family, which is more than a first release needs.

## Decision

**Release files.** `.github/workflows/release.yml` runs on a `v*` tag. It fails unless the tag is `v` followed by the workspace version in `Cargo.toml`, so a release cannot carry a binary that reports another version. It builds, and attaches to a GitHub Release with fixed names:

| Asset | Content |
| --- | --- |
| `monitorflow-agent-linux-x86_64`, `monitorflow-agent-linux-aarch64` | Static musl agent |
| `monitorflow-agent-windows-x86_64.exe`, `monitorflow-agent-windows-x86_64.msi` | As built by the `windows` CI job (ADR-0047, ADR-0050) |
| `install.sh` | `deploy/install.sh` |
| `monitorflow-agent.service`, `monitorflow-agent-sudoers` | `deploy/systemd/monitorflow-agent.service`, `deploy/sudoers/monitorflow-agent` |
| `SHA256SUMS` | SHA-256 of every other file, `sha256sum` format |

Names carry no version, so `https://github.com/Kidoly/monitorflow/releases/latest/download/<name>` always serves the newest release, and `…/releases/download/v<version>/<name>` a given one. A version with a suffix (`0.2.0-rc.1`) is published as a pre-release, which `latest` skips. `deploy/release-files.sh` adds the installer, the unit and the sudo rules to a directory of binaries and writes `SHA256SUMS`; the workflow and the CI test use it, so they serve the same files. A manual run (`workflow_dispatch`) and pull requests that touch these files build everything as the `release-files` artifact and publish nothing. Only the job that publishes has `contents: write`.

**Static musl binaries, built with cargo-zigbuild.** One Linux binary per architecture runs on every distribution, whatever its glibc, which keeps the asset list short and the installer free of distribution detection. `cargo zigbuild` uses zig as the C compiler and linker: ring's C and assembly and the musl libc build for x86_64 and aarch64 on the same x86_64 runner, without Docker or a cross GCC. zig and cargo-zigbuild come from PyPI at pinned versions (`ziglang` 0.14.1, `cargo-zigbuild` 0.24.0) in a virtual environment, through `.github/actions/build-agent-linux`. `cross` was rejected because it runs every build in a Docker image of its own, pulled from a registry we do not control, with a toolchain version that must follow ours. The distribution's `musl-tools` was rejected because it covers the runner's own architecture only. musl's allocator is slower than glibc's; the agent allocates little, so it is kept, and a different allocator can be added if a profile shows otherwise.

**The installer.** `deploy/install.sh` is POSIX `sh`, since it is piped to `sh`, and checked by shellcheck in CI. It reproduces the manual steps exactly: `/usr/bin/monitorflow-agent` owned by root, the system user `monitorflow`, `/var/lib/monitorflow/agent` mode 0700, `/etc/monitorflow/agent.toml` root:monitorflow 0640 written by the agent's own `init-config` (ADR-0050), so it is validated as the agent loads it, the sudo rules checked with `visudo -cf` before they are installed, and the unit. Inputs come from the environment:

| Variable | Meaning |
| --- | --- |
| `MF_SERVER` | `host:port` or https URL of the upstream, server or relay; port 7443 by default. Required when there is no configuration yet |
| `MF_TOKEN` | Enrollment token. Required unless the host is enrolled |
| `MF_ENROLL` | Enrollment listener, when it is not on `MF_SERVER`'s host at port 7445: always when `MF_SERVER` is a relay |
| `MF_VERSION` | GitHub release to install; the latest by default |
| `MF_DOWNLOAD_URL` | https URL of a directory with the release files, instead of GitHub |
| `MF_UNINSTALL=1`, `MF_PURGE=1` | Remove the agent; with `MF_PURGE`, its configuration, identity and user too |

IPv6 addresses go in brackets, as in URLs; a bare one is refused with that hint.

**Integrity.** The installer downloads `SHA256SUMS` and the files from the same base, over HTTPS only (`curl --proto =https`, or `wget --https-only`), and checks every file before it changes anything; a missing entry or a mismatch stops it. The checksums protect against corruption and against a mirror that serves other binaries than the base the checksums come from, not against whoever controls that base: TLS to GitHub, or to the site's own server, is the root of trust, as it is for the one-line command itself. Signed releases (minisign or Sigstore, and Authenticode for Windows, ADR-0050) are left to the release process.

**Token handling.** The token must not stay on disk (ADR-0007), and a unit cannot take it from a shell. The installer writes it to `/etc/monitorflow/agent-enroll.env`, mode 0600 root, which the unit names in `EnvironmentFile=-…`: systemd reads it as root before it drops to `monitorflow`, so the agent's user never can. The installer then waits for the identity (`<data_dir>/tls/cert.pem` and `key.pem`) and deletes the file. It stops waiting when the agent's process exits, as on a refused token, or after `MF_TIMEOUT` seconds (120); it then stops the service, deletes the file, prints the end of the journal and points to `journalctl -u monitorflow-agent`. The token is never printed. It does appear on the `sudo` command line while the installer runs, visible to local users in `ps`; the guide shows how to pass it through the environment instead on shared hosts.

**Idempotence.** Running the installer again on an enrolled host replaces the binary (written next to it and renamed, so the running agent is not disturbed), the unit and the sudo rules, keeps the configuration and the identity, ignores `MF_SERVER`, `MF_ENROLL` and `MF_TOKEN` with a message, and restarts the service. That is the upgrade path. Uninstalling keeps `/etc/monitorflow/agent.toml` and the identity, like the MSI without `REMOVE_DATA` (ADR-0050).

**Self-hosting.** A site copies the files of a release into any directory served over HTTPS and passes it as `MF_DOWNLOAD_URL`; the installer fetches `install.sh` from there too.

**CI.** The `compose` job builds the x86_64 agent as the release does, then `deploy/install-test.sh` starts the production compose, serves the release directory over HTTPS with a throwaway CA, creates a token with `monitorflow-server token create`, pipes `install.sh` to `sh` on the runner, which has systemd, and checks the result through the API (the agent online) and on disk (owners, modes, token file gone, token in no file, journal or output). It runs the installer again and checks that the identity and configuration are unchanged, checks that a wrong checksum leaves the binary untouched, then uninstalls with and without `MF_PURGE`. Pull requests that change the release files also run the release workflow without publishing.

## Consequences

- One command installs, enrolls or upgrades a Linux agent, from GitHub or from a site's own server.
- Releases are cut by pushing a tag; the workflow refuses a tag that disagrees with `Cargo.toml`. Nothing else publishes.
- `.deb` and `.rpm` packages (architecture §7) are still to come; they will reuse the unit and the sudo rules, and the installer may then prefer them.
- Binaries and the installer are not signed yet: their integrity rests on HTTPS to the release host.
- Only x86_64 and aarch64 Linux, and x86_64 Windows, are built.
