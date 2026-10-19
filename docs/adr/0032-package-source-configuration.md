# ADR-0032: Package source configuration

Date: 2026-10-19
Status: accepted

## Context

Architecture §7 has the agent point a server's package manager at its MonitorFlow mirror, back up the original sources and distribute the repository signing keys. The mirrors are published by the server with a fixed layout (ADR-0030): `<base>/apt/<mirror>/<snapshot>/`, `<base>/dnf/<mirror>/<snapshot>/`, a snapshot being a name or `current`. Commands reach the agent signed over the command channel (ADR-0031), whose handler registry calls this code. The agent runs as a dedicated unprivileged user, but `/etc/apt` and `/etc/yum.repos.d` belong to root. A change that breaks package installs must undo itself, and a server must always be revertible to its exact previous sources, including after a crash halfway through.

## Decision

**Spec.** `proto/monitorflow/v1/sources.proto` defines `ConfigureSources`: mirror base URL, family (`apt`, `dnf`), mode (`replace` or `add`), repositories of that family (apt: mirror, snapshot, suite, components, architectures; dnf: mirror, snapshot, repo ID, name, `gpgcheck` defaulting to true), and signing keys (name, armored public key, expected fingerprint). `RevertSources` has no fields. Before touching anything the agent checks the whole spec: values used in paths, URLs or file fields are restricted to letters, digits and `._+~-` (no `..`, newline or separator can reach a file), the URL is `http(s)` without query, and each key must decode to exactly one primary public key whose v4 (SHA-1) or v5/v6 (SHA-256) fingerprint equals the expected one. Packets other than public key, subkey, user ID, user attribute and signature are refused, so secret key material is never installed. The key is written re-armored from the checked packets, never as given. This is a small parser in `packages/sources/key.rs` on `ring` and `base64`, rather than a full OpenPGP crate; signatures are checked by apt and rpm themselves.

**Files.** apt: `/etc/apt/sources.list.d/monitorflow.sources` (deb822, a stanza per repository) with `Signed-By:` listing `/etc/apt/keyrings/monitorflow-<name>.asc`. dnf: `/etc/yum.repos.d/monitorflow-<repo_id>.repo` with section `[monitorflow-<repo_id>]` (dnf refuses a repeated ID, and the post-check selects `monitorflow-*`), `gpgkey=file:///etc/pki/rpm-gpg/RPM-GPG-KEY-monitorflow-<name>`. Every key is trusted for every repository of the spec. In `replace` mode apt's `sources.list` and `sources.list.d/*.list|*.sources` are moved into the backup; dnf's other `.repo` files are rewritten in place with `enabled=0` in every section, because rpm recreates a missing `%config(noreplace)` file on update but keeps a modified one. Symbolic links and special files among the sources are refused.

**Backup.** `/var/lib/monitorflow/sources-backup/`, mode 0700 and owned by the user running the helper (root); it is refused otherwise, since the agent's own data directory is writable by the agent user. One backup is active from the first apply until revert: `<unix-time>/manifest.toml` lists every path with its action (moved, disabled, overwritten, created, created directory) and, for originals, mode, owner and SHA-256, with copies under `files/`. Copies and manifest are synced before the first change to the host, and every write is a synced temporary file in the same directory renamed over the target. Later applies with the same family and mode add new paths to the same manifest, so MonitorFlow's own files are never taken for originals; applying the same spec changes nothing and runs no post-check. Another family or mode reverts first, then starts a new backup. A source changed by someone after MonitorFlow set it aside is refused rather than overwritten, as its new content has no copy. Revert restores each original whose content, mode or owner differs, after checking the copy against the manifest, removes MonitorFlow's files and empty directories, and clears `active`; the backup directory is kept. It works from any intermediate state, so it is also the rollback.

**Post-check.** A `PostCheck` trait; `SystemCheck` runs `apt-get update` restricted to `monitorflow.sources` (an apt warning or error line is a failure, as apt before 2.1 exits 0 when an index fails) or `dnf makecache --refresh` on `monitorflow-*` repositories, with a 5-minute timeout. A failure midway or of the post-check reverts to the originals and reports the error; a failed revert reports both errors.

**Privileges.** The agent does not write `/etc` itself: it runs the helper `sudo -n /usr/bin/monitorflow-agent sources apply` with the encoded `ConfigureSources` on stdin, or `sudo -n /usr/bin/monitorflow-agent sources revert`. The helper always works on `/` and takes no path argument, so the packaged sudoers rule allows exact command lines without wildcards:

```
monitorflow ALL=(root) NOPASSWD: /usr/bin/monitorflow-agent sources apply, /usr/bin/monitorflow-agent sources revert
```

The library API is `apply(spec, root, checker)` and `revert(root)` on a configurable root, which tests set to a temporary directory. The helper trusts the agent user for the spec: command signatures are checked by the agent before it calls the helper (ADR-0031).

## Consequences

- A compromised agent user can point the host at other repositories and keys, but not write any other file as root; it would still need a way to install packages as root.
- Package updates of `*-release` packages on dnf hosts write `.rpmnew` files next to the disabled ones instead of re-enabling them.
- Restoring to the previous MonitorFlow configuration after a failed re-apply is not supported: a failure always goes back to the originals.
- Hosts whose sources are symbolic links need them replaced by files before MonitorFlow can manage them.
- Packaging must install the sudoers rule above and the binary at `/usr/bin/monitorflow-agent`.
