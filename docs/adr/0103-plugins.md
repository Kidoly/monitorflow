# ADR-0103: Plugins

Date: 2026-11-07
Status: accepted; integration hooks wait for outgoing events (ADR-0102)

## Context

Phase 16 asks for plugins: checks, notification channels and integrations added without changing MonitorFlow, signed, isolated from the server's secrets, and installed from the web interface.

The central server holds the certificate authority, whose key signs every command agents run, the customers' data and the passwords of their channels. Agents run commands as root through sudo rules. A plugin is code someone else wrote. Whatever runs it must not reach any of that, and must not let one customer's plugin settings or data reach another customer. Breadth matters less than getting this right: a small plugin system that cannot leak is worth more than a large one that can.

## Decision

### Runtime: WebAssembly in an interpreter, with no WASI

A plugin is one WebAssembly module, run by **wasmi**, an interpreter written in Rust, inside the agent or the server process. The module sees nothing but what MonitorFlow passes it: there is no WASI, no file system, no clock, no environment, no sockets. Its only imports are six functions of our own (`mf` below), and a module importing anything else is refused before it runs.

Limits, the same for every plugin and not settable by its manifest:

| Limit | Value |
| --- | --- |
| Module | 2 MiB |
| Memory | 32 MiB of linear memory, 10,000 table entries, one memory and one table |
| CPU | 2,000,000,000 units of wasmi fuel per run (about a second of work), then the run traps |
| Time | the check's timeout or 15 s for a channel; HTTP requests are cut to what is left |
| HTTP | 8 requests per run, 64 KiB per request body, 1 MiB per response, 10 s each |
| Output | 64 KiB of result, 4 KiB of log lines |
| At once | 4 plugin runs per process; the rest wait their turn |

Each run gets a fresh instance: nothing a plugin does survives to its next run or reaches another place's run.

Rejected:

- **wasmtime with WASI and the component model.** It is the reference runtime and its component model gives typed interfaces, but it compiles modules to machine code with Cranelift: a code generator in the trusted base, with past sandbox escapes through miscompilation, about 15 MB more in an agent of 20 MB, and executable memory on every agent host. Checks run every few seconds at most and do little work, so a compiler buys nothing here. wasmi's interpreter has no executable memory, builds for every agent target (static musl on x86-64 and aarch64, Windows), meters fuel deterministically and has been audited. WASI would also be a second interface to fence off; we need none of it.
- **External processes over gRPC**, like Terraform or Vault plugins. Any language works, but a process has the agent's user, its files, its network and its key; isolating it needs namespaces or seccomp on Linux, job objects and AppContainers on Windows, each a project of its own, and code built for each platform. Signing would cover a binary that can do anything.
- **Scripts** (Lua, JavaScript, Rhai). An embedded interpreter is as sandboxed as its standard library is trimmed, and each has had escapes; plugin authors would also be tied to one language. The custom script check (ADR-0024) remains for local scripts a host's administrator installs.

### What a plugin can do

Three kinds are named in the manifest; v1 runs two.

- **`check`**, run by the agent like a built-in check (ADR-0012): same scheduler, interval and timeout, and its samples are named and validated as any check's. It gets its settings and returns samples.
- **`channel`**, a notification channel run by the central server. It gets its settings, its secret settings and the message the chat channels send (ADR-0077), as plain text, and delivers it itself.
- **`integration`**, a hook receiving outgoing events. ADR-0102 defines those events in a parallel change; until it is merged, a manifest of this kind is refused with "not supported yet". Its delivery will reuse the channel runtime, with ADR-0102's event body as input.

**The one capability is HTTP.** A plugin lists in its manifest the hosts it may call: a name (`api.pushover.net`, ports 80 and 443), a name and port (`ntfy.example.com:8443`), or `{settings.url}` for the host of the URL in one of its settings, the usual case for a check of a service the administrator names. Every request is made by MonitorFlow on the plugin's behalf, refused before any connection if its host is not listed, with `http` or `https` only, no user name or password in the URL, no redirects followed, no `Host`, `Connection` or proxy headers, and the system's web roots for HTTPS. On the central server, requests go only to public addresses, with ADR-0078's resolver, whoever installed the plugin: the server's own network holds PostgreSQL and the metric stores. They also go over HTTPS only, since they carry the plugin's secret settings across the Internet. An agent's check may reach its own network, which is what checks are for, as the port check already does.

A plugin never sees the server's configuration, its keys, the certificate authority, the command signing key, the database, other channels' secrets or another place's settings. It gets exactly its own settings, and secret settings of a channel only for that channel's run. Its log lines and errors are capped, have the secret settings it was given replaced by `[REDACTED]`, and go to the log at debug level.

### Manifest

JSON, inside the package:

```json
{
  "name": "ntfy",
  "version": "1.0.0",
  "kind": "channel",
  "title": "ntfy",
  "description": "Sends notifications to an ntfy topic.",
  "abi": 1,
  "capabilities": { "http": ["{settings.server}"] },
  "settings": [
    { "key": "server", "label": "Server", "type": "url", "required": true, "default": "https://ntfy.sh" },
    { "key": "topic", "label": "Topic", "type": "text", "required": true },
    { "key": "token", "label": "Access token", "type": "secret" }
  ]
}
```

`name` is 1 to 40 of `a-z`, `0-9` and `-`; `version` is `MAJOR.MINOR.PATCH`; `settings` has at most 20 fields of type `text`, `url`, `number`, `bool` or `secret`, the list the web interface draws a form from. A list of fields instead of JSON Schema: the form must be obvious, and every type here is one input. In v1 a check may not have `secret` settings, because check settings travel to agents with the rest of their configuration and are shown to operators; a check that needs a token waits for a later change.

### Package and signatures

A package is one `.mfplugin` file, a JSON object: `format` (`monitorflow-plugin-1`), `manifest` (the manifest's exact text), `module` (base64), `public_key` (the signer's raw Ed25519 key, base64) and `signature`. The signature is Ed25519 over `monitorflow-plugin-v1\0`, the manifest's length (4 bytes, big-endian), the manifest's bytes and the SHA-256 of the module. Ed25519 because releases use it already (ADR-0053) and `ring` verifies it.

**Who may sign.** A package is installed only if its key is trusted:

- **MonitorFlow's catalogue key**, the public halves in `deploy/plugin-keys.pem`, built into the server and the agent like the release keys. The file has no key yet: the owner makes one offline with `monitorflow-plugin keygen` and adds its public half, and until then no package counts as official. It is not the release key: that one signs code that runs as root, and must not be used for anything else.
- **Publisher keys** a global administrator adds under Administration › Plugins › Publishers, by pasting the public key the publisher printed, with a name. A company signing its own plugins adds itself. Removing a publisher is refused while a plugin it signed is installed.

**Tooling.** `monitorflow-plugin keygen --out key.pem` makes a key on the publisher's machine, writes it 0600 and prints the public half in both forms; `monitorflow-plugin pack --manifest manifest.json --module plugin.wasm --key key.pem` checks the manifest and the module against every rule above and writes the package; `monitorflow-plugin inspect FILE` shows what an administrator will see. No private key is ever generated, printed or kept by MonitorFlow's servers, CI or repository; tests make throwaway keys in memory.

### Installing from the web interface

Administration › Plugins, for **global administrators only**: a plugin is code that runs on the central server or on every agent it is given to, so installing one is a decision for whoever controls the server. The page:

1. **Upload** a file. The server checks the format, the signature against the trusted keys, the manifest and the module (it loads it with the limits above and checks its imports) and stores nothing yet; it answers what the plugin is, who signed it, and what it asks for **in plain words**: "Connects to api.pushover.net", "Connects to the address in its Server setting", "Runs on your servers as a check" or "Sends notifications from the central server", "Can read the secret settings you give it". An update says what it adds and what it drops.
2. **Install**, with the package's SHA-256 from step 1, so what is installed is what was shown. An update must have a higher version and the same signer; another signer means removing the plugin first.
3. **Enable** it for everything, a customer or a site, like a channel's coverage (ADR-0078), and turn it off or remove it. Customers are offered and named only to someone who sees several (ADR-0104): a single company picks every server or some of its sites. Turning off stops every use at once, without losing settings: channels keep their rows and say "the plugin is turned off", checks leave the agents' configuration. Remove is refused while a channel or a check uses it, and says which.

Every step is audited (`plugin.install`, `.update`, `.enable`, `.disable`, `.remove`, `plugin_publisher.add`, `.remove`) with the name, version, signer and capabilities, never the package.

**Using it.** Where a plugin is enabled, the administrators who manage channels there (ADR-0078) see it as a channel kind, its settings as the form's fields and its secret settings handled like a webhook URL: never shown again, typed again to change where it sends. Operators who manage checks there (ADR-0016) see a check plugin among the checks they can add, as kind `plugin` with the parameter `plugin` naming it and its settings as the other parameters. A place where it is not enabled refuses both. Packages are kept in PostgreSQL, so every server instance (ADR-0089) has them.

### Checks on agents

The server sends an agent the plugins its checks use with its sensor configuration (`SensorConfig.plugins`, additive): each package once per configuration, and a grant signed with the command key (ADR-0031) over the agent's ID, the plugin's name and the package's SHA-256. The agent runs a plugin only when the grant verifies against its certificate authority, so a relay that forwards configurations cannot slip a module in, and the package's own signature verifies with the key it names. `[plugins] checks = false` in `agent.toml` turns plugin checks off on a host, and `official_only = true` keeps only the catalogue's; the agent then refuses the check, which shows as refused like any check it cannot run. A compromised central server can still give agents a plugin with the hosts it chooses; it could already make them connect anywhere with the port check, and the sandbox is the boundary for the rest. A sandbox bug would give the agent's unprivileged user, not root.

### SDK and examples

`crates/plugin-sdk` wraps the `mf` imports for Rust plugins built for `wasm32-unknown-unknown`, and `docs/plugins.md`, "Write a plugin", explains the manifest, the ABI and signing. `plugins/examples/` has one plugin per kind run in v1: `http-json`, a check that fetches a web address and reports whether it answers, its status code, how long it took and one number from its JSON, and `ntfy`, a channel. They build outside the workspace, in their own, and CI builds them and runs them through the real runtime.

### ABI version 1

The module exports `memory` and `mf_run() -> i32` (0 for success). It imports from `mf`:

| Import | Does |
| --- | --- |
| `input_len() -> i32`, `input_read(ptr)` | The input JSON: `{"settings", "secrets", "message"}` for a channel, `{"settings", "instance"}` for a check |
| `output(ptr, len)` | The result JSON: `{"samples": [{"metric", "instance", "value", "labels"}]}` for a check, `{}` for a channel; the last call wins |
| `log(level, ptr, len)` | A log line, 0 error to 3 debug |
| `http(ptr, len) -> i32` | Sends a request `{"method", "url", "headers": [[name, value]], "body"}`; the length of the answer |
| `http_read(ptr)` | The answer: `{"status", "headers", "body", "elapsed_ms"}`, or `{"error"}` in words. Plugins have no clock, so the host times requests |

JSON between plugin and host keeps the ABI to these six functions and lets fields be added; a typed interface (the component model) can be a version 2 once a runtime we trust supports it.

## Consequences

- Plugins add checks and channels in any language that compiles to WebAssembly, Rust first with the SDK, signed, without changing MonitorFlow, and nothing they do reaches a secret they were not given.
- The only way out of a plugin is an HTTP request to a host the administrator saw before installing it.
- Interpreted code is slower than compiled code; within the fuel limit a plugin can parse a few megabytes of JSON, which is enough for checks and notifications and nothing more.
- No plugin is official until the owner adds a catalogue key, and there is no catalogue to browse yet: plugins come as files.
- Integration hooks, secret settings for checks, plugins on relays and a catalogue are follow-ups.
- An agent too old for plugins refuses the `plugin` kind, which shows as refused, like any kind it does not know.
