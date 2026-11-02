# Glossary

The words MonitorFlow uses with people: on screen, in the installers, in the CLI and in the guides. Internal names stay as they are, so API routes, configuration keys, database columns, protocol names and CLI flags never change for wording. A screen word and its internal name may differ.

| Say | Meaning | Avoid on screen | Internal name |
| --- | --- | --- | --- |
| Server | A machine with the agent, shown in the fleet | host, peer, agent (for the machine) | `servers`, `hostname` |
| Machine | Any computer, when "server" would be unclear | host | |
| Central server | The machine that runs MonitorFlow itself. Pages of its web interface may say "this server" | server host | `server` |
| Relay | Carries the servers of a site that cannot reach the central server | peer | `relays` |
| Name | What an agent or relay is called | peer ID | `peer_id` |
| Addresses | The names or IP addresses agents use to reach a relay | server names, SANs | `server_names`, `--server-name`, `SERVER_NAMES` |
| Install command | The line to paste on a machine | enrollment token, install key | `install_keys`, `MF_TOKEN` |
| Single-use token | A token for one machine, for strict environments | enrollment token, one-time token | `enrollment_tokens`, `token create` |
| Connect, first connection | An agent or relay joining with its token | enroll, enrollment | `enroll`, `MF_ENROLL`, `enroll_url` |
| Added | When a server or relay joined | enrolled | `created_at` |
| Last contact | When a server or relay last reported | heartbeat | `last_heartbeat` |
| Configuration up to date | The agent runs the current configuration | revision, in sync, drift | `revision`, `in_sync` |
| Accepts commands | What a server's agent will run if it is asked. Say which commands: "AI commands", "package updates", "agent updates" | capability, opt-in, handler | `command_kinds` |
| AI commands | The commands AI remediation sends a server. On, Off, Not available, or Not known yet while the agent or its relay has not updated | shell commands | `shell`, `commands.shell`, `MF_ALLOW_COMMANDS` |
| Watches | The disk, port or service a sensor measures | instance | `instance` |
| Recovery margin | How far past the threshold a value must return before the incident resolves; under Details | hysteresis | `hysteresis` |
| Set by | Where a setting comes from; under Details | origin | `origin` |
| Batch | Part of the servers of a campaign, updated together | wave | `waves` |
| Remove | Revoke a server or relay and hide it from lists; for a mirror, delete it and its packages | delete | `revoked_at`, `deleting_at` |
| Revoke | Stop an install command, a token or a role from working | | |
| Channel | Where notifications go: an email or a webhook | destination, target | `notification_channels` |
| Notify about | What a channel hears of: Everything, All of a customer, or one site | scope, coverage, routing | `customer_id`, `site_id`, `Coverage` |
| Everything | A channel for all alerts, plus news about the central server: failed backups, automatic updates, new versions | global, all customers | both columns null |

Keep the technical word when it is the thing being asked for, such as a certificate, a mirror or a sudo rule, and put the rest under a "Details" disclosure. Certificate warnings speak of renewal, and appear only when it is late (under 2 days left) or failed.

ADR references stay out of screens and CLI help. In the install guides they are footnotes.
