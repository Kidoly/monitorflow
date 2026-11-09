# Write a plugin

A plugin adds a check or a notification channel to MonitorFlow without changing MonitorFlow. It is one WebAssembly module and a manifest, signed with your key. An administrator installs the file from Administration › Plugins, sees what it asks for, and turns it on where it should be used.[^adr]

A plugin runs in a sandbox. It cannot read files, start programs or open connections. It gets its settings as input, returns a result, and can make HTTP requests only to the hosts its manifest lists. MonitorFlow makes those requests for it.

## Kinds

| Kind | Runs on | Gets | Returns |
| --- | --- | --- | --- |
| `check` | each server it is configured for, like a built-in check | its settings and the instance it watches | samples |
| `channel` | the central server, for each notification | its settings, its secret settings and the notification | nothing, or an error |

Integration hooks for outgoing events will be a third kind.

## Quick start in Rust

You need Rust and its WebAssembly target:

```
rustup target add wasm32-unknown-unknown
```

`Cargo.toml`:

```toml
[package]
name = "disk-api"
version = "1.0.0"
edition = "2021"

[lib]
crate-type = ["cdylib"]

[dependencies]
monitorflow-plugin-sdk = { git = "https://github.com/Kidoly/monitorflow" }

[profile.release]
opt-level = "s"
lto = true
panic = "abort"
strip = true
```

`src/lib.rs`, a check:

```rust
use monitorflow_plugin_sdk::{check, CheckInput, Request, Sample};

fn run(input: CheckInput) -> Result<Vec<Sample>, String> {
    let url = input.setting("url")?;
    let response = Request::get(url).send()?;
    let free = response.json()?["free_bytes"]
        .as_f64()
        .ok_or("no free_bytes in the answer")?;
    Ok(vec![
        Sample::new("free_bytes", free),
        Sample::new("seconds", response.elapsed_ms as f64 / 1000.0),
    ])
}

check!(run);
```

A channel uses `channel!` with a `fn(ChannelInput) -> Result<(), String>`. Its input has `settings`, `secrets` and `message`, with the notification's `event` (`incident.opened`, `incident.resolved`, `approval.waiting`, `test` and the others), `text` in one line, the `url` of its page, `lines` when it tells several things at once, and `payload`, the JSON body a webhook channel would get. All of it is plain text: escape it for whatever format your service reads.

Build:

```
cargo build --release --target wasm32-unknown-unknown
```

Two complete examples are in [`plugins/examples`](../plugins/examples): `http-json`, a check, and `ntfy`, a channel.

## The manifest

`manifest.json`, next to your code:

```json
{
  "name": "disk-api",
  "version": "1.0.0",
  "kind": "check",
  "title": "Disk API",
  "description": "Free space reported by the storage API.",
  "abi": 1,
  "capabilities": { "http": ["{settings.url}"] },
  "settings": [
    { "key": "url", "label": "Address", "type": "url", "required": true, "help": "Like https://storage.lan/api/space" }
  ]
}
```

| Field | Rules |
| --- | --- |
| `name` | 1 to 40 of `a-z`, digits and `-`, starting with a letter. It identifies the plugin: an update keeps it |
| `version` | `MAJOR.MINOR.PATCH`. An update must be higher |
| `kind` | `check` or `channel` |
| `title`, `description` | What administrators read: up to 60 and 500 characters |
| `abi` | `1` |
| `capabilities.http` | Up to 16 hosts the plugin may call: `api.example.com` (ports 80 and 443), `api.example.com:8443`, or `{settings.KEY}` for the host and port of the address in a `url` setting. Nothing else is reachable |
| `settings` | Up to 20 fields of the form administrators fill in: `key` (`a-z`, digits, `_`), `label`, `type`, `required`, `default`, `help` |

Setting types are `text`, `url` (http or https, no user name or password), `number`, `bool` (`true` or `false`) and `secret`. A secret is never shown again after it is saved, and only channels may have them for now: a check's settings travel to the servers it runs on.

Administrators see your capabilities in words before installing, like "Connects to the address in its Address setting". Ask for as little as you can.

## Samples

A check returns samples named after a suffix: `free_bytes` of a check called `storage` becomes the metric `storage_free_bytes`. A suffix is lowercase letters, digits and `_`. A sample may set `instance` (what it is about, like a volume) and up to 16 labels. Values must be finite numbers. Report "down" as a value, like `up` 0, rather than an error: an error means the check itself could not run, and shows as such.

## Limits

The same for every plugin; a manifest cannot change them.

| | |
| --- | --- |
| Module | 2 MiB |
| Memory | 32 MiB |
| CPU | about a second of work per run |
| Time | the check's timeout, or 15 s for a channel |
| HTTP | 8 requests per run, 64 KiB per request body, 1 MiB per answer, 10 s each, no redirects followed |
| Result | 64 KiB; log lines 4 KiB per run |

Each run starts from a fresh instance: keep nothing between runs. Plugins have no clock and no randomness; the host times each HTTP request (`elapsed_ms`).

## Sign it

Make a key once, on your machine:

```
monitorflow-plugin keygen --out ~/.monitorflow/plugin-key.pem
```

It prints the public key. Keep the private key file to yourself: whoever has it can sign plugins in your name. Then sign each version:

```
monitorflow-plugin pack --manifest manifest.json \
  --module target/wasm32-unknown-unknown/release/disk_api.wasm \
  --key ~/.monitorflow/plugin-key.pem
```

It checks everything an install checks and writes `disk-api-1.0.0.mfplugin`. `monitorflow-plugin inspect disk-api-1.0.0.mfplugin` shows what administrators will see.

`monitorflow-plugin` is built with MonitorFlow: `cargo install --git https://github.com/Kidoly/monitorflow monitorflow-plugin`.

## Install it

A global administrator adds your public key once under Administration › Plugins › Publishers, then uploads the file on the same page, reads what it asks for, installs it and chooses where it may be used. An update is a newer version signed with the same key. Plugins signed with MonitorFlow's own key need no publisher.

Then, where it is turned on, a check plugin is among the checks operators can add to a server or a site, and a channel plugin among the kinds of channel administrators can create.

## Other languages

Any language that compiles to a WebAssembly module without WASI works. The module must export `memory` and `mf_run() -> i32` (0 for success), and may import only these functions of the module `mf`:

| Import | Does |
| --- | --- |
| `input_len() -> i32` | Length of the input JSON |
| `input_read(ptr)` | Copies the input JSON to `ptr` |
| `output(ptr, len)` | Sets the result JSON: `{"samples": [{"metric", "instance", "value", "labels"}]}` for a check, `{}` for a channel |
| `log(level, ptr, len)` | A log line, 0 error to 3 debug. When `mf_run` fails, its last error line is the error shown |
| `http(ptr, len) -> i32` | Sends the request JSON `{"method", "url", "headers": [[name, value]], "body"}`; returns the length of the answer |
| `http_read(ptr)` | Copies the answer: `{"status", "headers", "body", "elapsed_ms"}`, or `{"error"}` |

A module importing anything else is refused when it is signed and again when it is installed.

[^adr]: Design and reasons: ADR-0103.
