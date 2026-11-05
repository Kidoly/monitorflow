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
| AI provider | Who answers for AI: "Anthropic (Claude)", "OpenAI", or "Another or a local model". "Which AI" on the AI page | vendor, backend, LLM | `provider`, `ai_provider`, `anthropic`, `openai`, `openai_compatible` |
| Local model | An AI model running on a machine of your own network, through Ollama, llama.cpp, vLLM or LM Studio. Its "address" is where its API is, like http://ollama.lan:11434/v1 | self-hosted LLM, endpoint, inference server | `base_url`, `ai_base_url` |
| AI commands | The commands AI remediation sends a server: shell commands on Linux, PowerShell on Windows. On, Off, Not available (an agent too old for them, which updates by itself), or Not known yet while the agent or its relay has not updated | shell commands | `shell`, `commands.shell`, `MF_ALLOW_COMMANDS`, `ai-commands on` |
| Watches | The disk, port or service a sensor measures | instance | `instance` |
| Every 30 s, every 1 h | How often a check runs: "Checked every 5 min". The built-in checks run at their site's interval | interval, frequency, polling rate, scrape interval (on screen) | `interval_ms`, `check_interval_ms` |
| Log folders | The folders whose files a server's log checks may read, set by its administrator in `agent.toml`: `/var/log` on Linux by default, none on Windows | log directories, allowlist | `sensors.log_dirs` |
| Disk busy | Share of the time a disk had work in flight, per disk (`sda`, `vg0-root`, `PhysicalDrive0`) | utilisation, util, I/O % | `diskio_busy_ratio` |
| Disk wait | Average time a disk took to answer one read or write, waiting in its queue included | latency, await | `diskio_wait_seconds` |
| Disk activity | The check that reports how busy and how slow each disk is | disk I/O, diskio (on screen) | `diskio` |
| System logs | The warnings and errors of a server's journal, which its agent forwards; the Logs tab of the server's page | journald, journal entries, log forwarding, syslog | `LogBatch`, `source=journal`, `/api/servers/{id}/logs` |
| Recovery margin | How far past the threshold a value must return before the incident resolves; under Details | hysteresis | `hysteresis` |
| Set by | Where a setting comes from; under Details | origin | `origin` |
| Update source | Where a Windows server takes its updates from: Microsoft (Windows Update) or a WSUS server, as its policy sets it. "Updates come from WSUS at http://wsus.example:8530" | update server, WU endpoint, upstream | `windows_update_source` |
| Monthly updates | Windows quality updates: the security and fix updates Microsoft releases each month | quality updates, cumulative updates (on screen) | `quality_from_microsoft`, `quality_deferral_days` |
| Batch | Part of the servers of a campaign, updated together | wave | `waves` |
| Rollback | What a change gets before it runs: a snapshot to go back to. "Rollback: LVM snapshot of / and /var"; without one, "Rollback limited" and why, like "/ is on plain ext4" | restore point, backup | `snapshots`, `/rollback` |
| Snapshot | A copy of a server's system volumes (or its Proxmox VM) taken before a change, deleted after a day (classic LVM) or a week | checkpoint, LV, dataset (on screen) | `take_snapshot`, `server_storage` |
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
| Dashboard | A page of charts and figures someone builds by picking from lists, under Monitoring | board, view (on screen), Grafana | `dashboards` |
| Panel | One chart, figure, top list or list of open alerts on a dashboard | widget, tile, card | `panels` |
| Shared with | Who sees a dashboard: Only me, a site or a customer. Operators of that site or customer change it | visibility, owner, scope | `shared_with` |
| Ready-made | A dashboard MonitorFlow comes with, for every server you see. Make a copy to change it | built-in, template, default | |
| Show on a screen | A dashboard full screen, without menus, for a TV or a projector | kiosk mode, TV mode, presentation mode | `/display/` |
| Display link | A link that shows dashboards on a screen without signing in, made by an administrator, turned off from the dashboard's page. "Reception TV" is its name | share link, public link, token, playlist | `display_links` |
| Turn off | Stop watching: no more alerts of a threshold, or offline incidents of a machine, until turned back on | mute | `offline_alerts`, `muted`, `enabled` |
| Rollback | Going back to the snapshot taken before an AI change. "Snapshot first" when MonitorFlow takes one, "Rollback limited" when it cannot, with the reason: "the container's storage cannot take snapshots" | revert, restore point | `snapshots.available`, `reason` |
| VM, container | The Proxmox guest a server runs in: a virtual machine, or an LXC container. "VM 101 on pve1", "container 104 on pve2". The ID is Proxmox's | guest, CT, qemu, lxc (on screen) | `server_proxmox_vms`, `kind`, `vmid` |
| Matched automatically | A server MonitorFlow tied to its Proxmox VM or container by itself: "Matched automatically to VM 104 on pve2 by its hardware ID". "Not this one" undoes it for good; "Change" picks another | discovered, auto-mapped | `matched_by` |

Keep the technical word when it is the thing being asked for, such as a certificate, a mirror or a sudo rule, and put the rest under a "Details" disclosure. Certificate warnings speak of renewal, and appear only when it is late (under 2 days left) or failed.

ADR references stay out of screens and CLI help. In the install guides they are footnotes.
