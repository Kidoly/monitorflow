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
| AI commands | The commands AI remediation sends a server: shell commands on Linux, PowerShell on Windows. On, Off, Not available (an agent too old for them, which updates by itself), or Not known yet while the agent or its relay has not updated | shell commands | `shell`, `commands.shell`, `MF_ALLOW_COMMANDS`, `ai-commands on` |
| Watches | The disk, port or service a sensor measures | instance | `instance` |
| Log folders | The folders whose files a server's log checks may read, set by its administrator in `agent.toml`: `/var/log` on Linux by default, none on Windows | log directories, allowlist | `sensors.log_dirs` |
| Recovery margin | How far past the threshold a value must return before the incident resolves; under Details | hysteresis | `hysteresis` |
| Set by | Where a setting comes from; under Details | origin | `origin` |
| Update source | Where a Windows server takes its updates from: Microsoft (Windows Update) or a WSUS server, as its policy sets it. "Updates come from WSUS at http://wsus.example:8530" | update server, WU endpoint, upstream | `windows_update_source` |
| Monthly updates | Windows quality updates: the security and fix updates Microsoft releases each month | quality updates, cumulative updates (on screen) | `quality_from_microsoft`, `quality_deferral_days` |
| Batch | Part of the servers of a campaign, updated together | wave | `waves` |
| Channel | Where notifications go: an email address, a webhook, or a Slack, Teams or Discord channel | destination, target, integration | `notification_channels` |
| Webhook URL | The address a chat service gives for posting to one of its channels. It works as a password, so it is never shown again | token, connector | `secret` |
| Workflow | What Teams calls the automation that gives such a URL, from the "Send webhook alerts to a channel" template | connector, except to name the retired ones | |
| Remove | Revoke a server or relay and hide it from lists; for a mirror, delete it and its packages | delete | `revoked_at`, `deleting_at` |
| Revoke | Stop an install command, a token or a role from working | | |
| Notify about | What a channel hears of: Everything, All of a customer, or one site | scope, coverage, routing | `customer_id`, `site_id`, `Coverage` |
| Everything | For a channel or a silence: all customers and sites, plus news about the central server: failed backups, automatic updates, new versions | global, all customers | both columns null; a silence with no target |
| Silence | Stop notifications about a server, relay, site, customer or everything for a while, whatever channel they go to. Incidents still open and show; what is still open at the end is sent | mute, snooze, maintenance mode, downtime | `silences` |
| Sent together | Incidents told in one notification: the same check on several machines, the rest of a site, or what a silence held back. Each incident still has its own page | group, batch, digest, bundle, aggregate | `notification_groups`, `?group=` |
| On call | Who is told of each alert of a place in person, besides the channels, in turns. A page under Monitoring | pager, duty, rota (on screen) | `on_call_schedules` |
| Schedule | The people who take turns being on call for everything, a customer or a site, and when they hand off | rotation, layer, policy | `on_call_schedules`, `on_call_members` |
| Hand-off | The moment one person's turn ends and the next one's starts: every day or every week, at a local time in the schedule's time zone | shift change, rollover | `handoff_time`, `anchor_date` |
| Cover | Someone else is on call for a period, for a holiday or a swap: "alice covers bob" | override (on screen), substitute | `on_call_overrides` |
| Gap | A period when the person on call cannot be told: no email or chat, disabled, removed, or no longer sees the place. The nearest administrators are told instead | hole, uncovered | `problem` |
| How you are reached | A person's own email address and optional chat or webhook, used when they are on call. Each person sets their own | contact method, notification rule | `user_contacts` |
| Acknowledge | Say "I'm on it" about an alert: nobody else is told next. "Acknowledged by alice at 03:12". From the alert's page, the alerts list, or the link in a message sent to you | ack, accept, claim, take | `incident_acknowledgements`, `acknowledge` |
| Follow-up | Who is told next when nobody acknowledges a critical alert, step after step, for everything, a customer or a site. "bob will be told at 03:27 if nobody acknowledges" | escalation, escalate, escalation policy (an incident "escalated" got worse; the AI "escalates" to a person when it stops) | `follow_up_policies`, `incident_follow_ups` |
| Step | One part of a follow-up: after a wait without acknowledgement, tell the person on call of a schedule, named people, channels or the administrators | level, tier (tiers are the risk of AI commands) | `steps` |
| Only for follow-ups | A channel that hears only the alerts nobody acknowledged, when a follow-up step names it | escalation channel | `follow_up_only` |
| Certificate authority | What the central server uses to give every server and relay its certificate and to sign commands. Said in full on screen | CA, PKI, root | `ca`, `ca-cert.pem` |
| Change the certificate authority | Replace it with a new one while every server and relay keeps working: they learn the new one, move to it, then the old one is retired. An emergency change is for when its key may have leaked | rotate, rotation, roll over, re-key, compromise | `ca_rotation`, `ca.rotation.*` |
| Moved | A server or relay whose certificate comes from the new certificate authority. "312 of 315 servers and relays moved" | migrated, re-issued | `moved`, `issuer_ca` |
| Needs a new install | A server or relay that missed an emergency change and cannot connect until it is installed again | cut off, stranded, orphaned | `cut_off` |
| Turn off | Stop watching: no more alerts of a threshold, or offline incidents of a machine, until turned back on | mute | `offline_alerts`, `muted`, `enabled` |

Keep the technical word when it is the thing being asked for, such as a certificate, a mirror or a sudo rule, and put the rest under a "Details" disclosure. Certificate warnings speak of renewal, and appear only when it is late (under 2 days left) or failed.

ADR references stay out of screens and CLI help. In the install guides they are footnotes.
