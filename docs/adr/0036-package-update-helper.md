# ADR-0036: Package update helper

Date: 2026-10-25
Status: accepted

## Context

ADR-0031 has the agent run apt and dnf through `sudo -n` with fixed command lines, and ADR-0035 ships `deploy/sudoers/monitorflow-agent` with those exact lines and no wildcard. Only full upgrades fit that file. Updates of named packages put the names on the command line (`apt-get … install --only-upgrade <packages>`, `dnf upgrade <packages>`), and so do security-only updates on apt, which name the packages whose candidate comes from a `-security` suite. Sudo refuses all of them, so they fail with "a password is required" unless an administrator adds a wildcard rule, which is then only as narrow as sudo's glob matching and lets the agent user pass any option to the package manager as root. Source configuration had the same problem and solved it with a root helper reading its spec on stdin (ADR-0032).

## Decision

**Helper.** All updates, full upgrades included, go through one subcommand of the agent binary run as root: `sudo -n /usr/bin/monitorflow-agent packages update`, with an update spec as JSON on stdin. One path for every kind keeps one sudoers line and one place where the command lines are built. The helper takes no argument, so sudoers allows that exact line:

```
monitorflow ALL=(root) NOPASSWD: /usr/bin/monitorflow-agent packages update
```

**Spec.** `{"kind": "all" | "security" | "named", "packages": [...]}`, unknown fields refused, at most 64 KiB and 1,024 names. `all` takes no names, `named` needs at least one, `security` takes an optional list that restricts it to those packages. Every name must match the distributions' grammar the agent already applies (`common::command::valid_package_name`: starts with a letter or digit, then letters, digits and `._+:-`, at most 128 bytes), so a name can be neither an option nor a path. The agent builds the spec from `ApplyUpdates` (`security_only` gives `security`, otherwise names give `named`, otherwise `all`) and checks it before running anything, rejecting the command as before; the helper checks it again, as it trusts nothing on stdin. The protocol does not change.

**Command lines.** The helper detects the OS family from `os-release` itself, rather than trusting the spec, and runs the tools at their fixed paths, with `DEBIAN_FRONTEND=noninteractive` set for apt (no `env_keep` needed any more):

| Family | Kind | Commands |
| --- | --- | --- |
| apt | all | `apt-get -q update`, then `apt-get -q -y -o Dpkg::Options::=--force-confdef -o Dpkg::Options::=--force-confold upgrade` |
| apt | named | `apt-get -q update`, then the same options with `install --only-upgrade <packages>` |
| apt | security | `apt-get -q update`, `apt-get -s -q dist-upgrade`, then `install --only-upgrade` of the installed packages whose candidate comes from a `-security` suite (among the named ones, if any); nothing when there are none |
| dnf | all, named | `dnf -q -y upgrade [<packages>]` |
| dnf | security | `dnf -q -y upgrade --security [<packages>]` |

For apt security, the helper keeps ADR-0031's simulation rather than a security-only sources list or `-o Dir::Etc::SourceList=…` options. Those need a list naming the security suites of the host, which differs between Debian (`<codename>-security` on its own host), Ubuntu (`<codename>-security` on the archive) and MonitorFlow mirrors (any suite name the administrator chose), and in the deb822 or one-line format the host uses. Running `apt-get update` on that list alone would also drop the other lists from `/var/lib/apt/lists` unless it gets its own state directory too. The simulation reads the origins apt already resolved and needs no extra file. Package names read from it are checked with the same grammar before use. Its limits are the ones ADR-0031 accepted: a package whose newest candidate is a point release that also carries the security fix is not seen as a security update, and dependencies an upgrade pulls in may come from any suite.

**Output.** The helper prints each command line as `$ <line>` and its output to stdout, the tool's stderr to its stderr, `nothing to upgrade` when the security selection is empty, and `Error: …` as its last stderr line on failure, exiting with the failing tool's code (1 when it could not run it). It stops at the first failure. The agent appends this to the command's output and uses the last stderr line as the command's error. `shutdown -r +1` stays a separate sudo line run by the agent after the update and the report, as before.

## Consequences

- `deploy/sudoers/monitorflow-agent` holds three exact lines for the agent (`packages update`, `sources apply|revert`) and `shutdown -r +1`; named and security updates work with it on both families, and no administrator needs a wildcard rule.
- A compromised agent user can still upgrade any installed package to any version the configured repositories offer, as with the full upgrade it could already run; it cannot pass options, install from a file, or install a package that is not installed (`--only-upgrade`, `dnf upgrade`).
- The helper runs as root with the agent's binary, so `/usr/bin/monitorflow-agent` must stay root-owned and not writable by anyone else (already required by ADR-0032).
- Killing the agent's `sudo` on a command timeout does not kill the helper or the package manager under it, as before with apt and dnf run directly.
- Hosts with an older `/etc/sudoers.d/monitorflow-agent` lose updates until the new file is installed: the agent no longer runs `apt-get` or `dnf` through sudo.
