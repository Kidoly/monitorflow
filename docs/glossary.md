# Glossary

The words MonitorFlow uses with people: on screen, in the installers, in the CLI and in the guides, in English and in French (ADR-0136). Internal names stay as they are, so API routes, configuration keys, database columns, protocol names and CLI flags never change for wording. A screen word and its internal name may differ.

| Say | French | Meaning | Avoid on screen | Internal name |
| --- | --- | --- | --- | --- |
| Server | Serveur | A machine with the agent, shown in the fleet | host, peer, agent (for the machine) | `servers`, `hostname` |
| Machine | Machine | Any computer, when "server" would be unclear | host | |
| Central server | Serveur central ; « ce serveur » | The machine that runs MonitorFlow itself. Pages of its web interface may say "this server" | server host | `server` |
| Relay | Relais | Carries the servers of a site that cannot reach the central server | peer | `relays` |
| Customer | Client | An MSP's client, owning sites. Said only to someone who sees two or more; a single company or a customer's own user never reads it (ADR-0104) | tenant, organisation, default | `customers`, `customer_id` |
| All sites | Tous les sites | For a single company: a choice covering its one customer as a whole | All of default | `customer_id` set, `site_id` null |
| Name | Nom | What an agent or relay is called | peer ID | `peer_id` |
| Addresses | Adresses | The names or IP addresses agents use to reach a relay | server names, SANs | `server_names`, `--server-name`, `SERVER_NAMES` |
| Install command | Commande d’installation | The line to paste on a machine | enrollment token, install key | `install_keys`, `MF_TOKEN` |
| Single-use token | Jeton à usage unique | A token for one machine, for strict environments | enrollment token, one-time token | `enrollment_tokens`, `token create` |
| Connect, first connection | Se connecter, première connexion | An agent or relay joining with its token | enroll, enrollment | `enroll`, `MF_ENROLL`, `enroll_url` |
| Added | Ajouté | When a server or relay joined | enrolled | `created_at` |
| Last contact | Dernier contact | When a server or relay last reported | heartbeat | `last_heartbeat` |
| Configuration up to date | Configuration à jour | The agent runs the current configuration | revision, in sync, drift | `revision`, `in_sync` |
| Accepts commands | Accepte les commandes | What a server's agent will run if it is asked. Say which commands: "AI commands", "package updates", "agent updates" | capability, opt-in, handler | `command_kinds` |
| AI provider | Fournisseur d’IA ; « Quelle IA » sur la page IA | Who answers for AI: "Anthropic (Claude)", "OpenAI", or "Another or a local model". "Which AI" on the AI page | vendor, backend, LLM | `provider`, `ai_provider`, `anthropic`, `openai`, `openai_compatible` |
| Local model | Modèle local | An AI model running on a machine of your own network, through Ollama, llama.cpp, vLLM or LM Studio. Its "address" is where its API is, like http://ollama.lan:11434/v1 | self-hosted LLM, endpoint, inference server | `base_url`, `ai_base_url` |
| AI commands | Commandes IA | The commands AI remediation sends a server: shell commands on Linux, PowerShell on Windows. On, Off, Not available (an agent too old for them, which updates by itself), or Not known yet while the agent or its relay has not updated | shell commands | `shell`, `commands.shell`, `MF_ALLOW_COMMANDS`, `ai-commands on` |
| Run AI again, attempt | Relancer l’IA, tentative | A new AI remediation of an alert still open, after the last one ended. "Run AI" for an alert that never had one. Attempts are numbered per alert | retry, rerun, restart | `POST /api/remediation/alerts/{id}/run`, `attempt` note |
| AI diagnosis, Diagnosed | Diagnostic de l’IA, Diagnostiqué | What the AI thinks caused an alert, and what to do next, shown on the incident. Every alert gets one; where no command can run there (AI commands off, a device, a host that stopped reporting), it is all the AI does, and the attempt shows as Diagnosed (ADR-0175) | root cause analysis, RCA, triage | `diagnosis`, `diagnosis_only` |
| Allow AI commands to be turned on from the web interface | Autoriser l’activation des commandes IA depuis l’interface web | A server's consent, given on the server, that administrators may turn its AI commands on from its page. They can always turn them off | remote control, remote opt-in | `commands.from_web`, `MF_AI_COMMANDS_FROM_WEB`, `AI_COMMANDS_FROM_WEB`, `ai-commands from-web on` |
| Device | Équipement | A switch, router, printer, UPS or any address watched without the agent, by checks run from the central server or a relay. It has its own page, alerts and dashboards like a server, and is never "waiting for its agent" | agentless host, target, node | `servers.kind = 'device'`, `address` |
| Checked from | Vérifié depuis | Where a network check runs: "the central server" or a relay of the site. "Ping, checked from relay-paris" | probe, poller, vantage point (on screen) | `probe`, `probe_relay_id` |
| Packet loss | Perte de paquets | The share of pings that got no answer, out of the few sent each time | drop rate | `ping_loss_ratio` |
| Round trip | Aller-retour | How long a ping took to come back, on average | RTT, latency (on screen) | `ping_rtt_seconds` |
| SNMP | SNMP | How switches, printers and UPSs say what they measure. Name the version: "SNMP v2c", "SNMP v3" | | `snmp` |
| Community | Communauté | The password of SNMP v2c, like `public`. Kept sealed and never shown again | community string | `community` |
| Custom SNMP check | Contrôle SNMP personnalisé | A check of the objects a person picks on a device, by their OIDs, rather than a group of the common MIBs | custom sensor, OID item | `snmp_custom` |
| Channel | Canal | One value a check reports, with its name, unit and limits: "Sessions", "RAID state". A custom SNMP check has up to 10 | metric (on screen), item, field | `oidN`, `nameN`, `keyN` |
| OID | OID | The address of an SNMP object, like 1.3.6.1.2.1.1.3.0. Show its name beside it when a MIB knows it | object identifier (spelled out), path | `oid` |
| MIB | MIB | A file from a device's maker that names its SNMP objects and their values. Uploading one names the objects in Browse and in checks | MIB module, compiled MIB | `snmp_mibs` |
| Lookup | Table de correspondance | Names for the values of an object, each with its state: "1 = OK, 2 = Warning, 3 = Failed" | value map, enumeration, mapping | `snmp_lookups`, `lookupN` |
| Browse | Parcourir | Listing what a device answers under an OID, to pick an object to watch | walk (on screen), MIB browser | `snmp/read`, `walk` |
| Template | Modèle | The checks and alerts a kind of device gets when added: Switch, Router, Firewall, Printer, UPS, NAS or Generic | profile, preset | `template` |
| Traffic | Trafic | What goes through a network interface, in and out, per second | bandwidth, throughput | `in_bytes_per_second`, `net_receive_bytes_per_second` |
| Watches | Surveille | The disk, port or service a sensor measures | instance | `instance` |
| Every 30 s, every 1 h | Toutes les 30 s, toutes les 5 min | How often a check runs: "Checked every 5 min". The built-in checks run at their site's interval | interval, frequency, polling rate, scrape interval (on screen) | `interval_ms`, `check_interval_ms` |
| Log folders | Dossiers de journaux | The folders whose files a server's log checks may read, set by its administrator in `agent.toml`: `/var/log` on Linux by default, none on Windows | log directories, allowlist | `sensors.log_dirs` |
| Network scan | Analyse du réseau | Looking for what answers in a range of addresses of a site, from the central server or a relay: "Scan now", "Scanned every day". It proposes what to add and adds nothing by itself | discovery (alone), sweep, crawl, auto-discovery | `network_scans`, `network_scan_runs` |
| Address range | Plage d’adresses | The IP addresses a scan tries: a list of addresses, blocks like 192.168.1.0/24 and ranges like 192.168.1.10-50 | subnet (alone), target list, scope | `targets` |
| Found | Trouvé | What answered in a scan, and what MonitorFlow proposes for it: "Install the agent", "Add as a device", "Already monitored". "New" and "Gone" compare with the scan before | discovered, detected host | `network_scan_hosts`, `proposal` |
| Recommended check | Contrôle recommandé | A check MonitorFlow suggests for a server from what its agent saw running: "PostgreSQL is running". Nothing is added until someone clicks Add | discovery, suggestion, auto-detected | `recommendations`, `server_discovery` |
| Dismiss | Écarter ; « Afficher les écartés » | Stop showing a recommended check for one server; "Show dismissed" brings it back | ignore, hide, mute | `check_recommendation_dismissals` |
| Port targets | Cibles de ports | The addresses, address ranges and host names, with ports, that a server's port checks may connect to, set by its administrator in `agent.toml`. Anywhere by default | port allowlist, scan scope | `sensors.port_targets` |
| Website check | Contrôle de site web | A check that requests a page as a visitor does: its status code, the text on it, how long it takes, its redirects and its certificate. "A website answers" in the catalogue | HTTP check, web monitor, synthetic check, uptime check | `http` |
| Days left (certificate) | Jours restants (certificat) | How long until a website's certificate expires; negative once it has | expiry, TTL | `http_cert_days_left` |
| Disk busy | Occupation du disque | Share of the time a disk had work in flight, per disk (`sda`, `vg0-root`, `PhysicalDrive0`) | utilisation, util, I/O % | `diskio_busy_ratio` |
| Disk wait | Attente du disque | Average time a disk took to answer one read or write, waiting in its queue included | latency, await | `diskio_wait_seconds` |
| Disk activity | Activité des disques | The check that reports how busy and how slow each disk is | disk I/O, diskio (on screen) | `diskio` |
| System logs | Journaux système | The warnings and errors of a server's journal, which its agent forwards; the Logs tab of the server's page | journald, journal entries, log forwarding, syslog | `LogBatch`, `source=journal`, `/api/servers/{id}/logs` |
| Recovery margin | Marge de rétablissement | How far past the threshold a value must return before the incident resolves; under Details | hysteresis | `hysteresis` |
| Set by | Défini par | Where a setting comes from; under Details | origin | `origin` |
| Update source | Source des mises à jour | Where a Windows server takes its updates from: Microsoft (Windows Update) or a WSUS server, as its policy sets it. "Updates come from WSUS at http://wsus.example:8530" | update server, WU endpoint, upstream | `windows_update_source` |
| Monthly updates | Mises à jour mensuelles | Windows quality updates: the security and fix updates Microsoft releases each month | quality updates, cumulative updates (on screen) | `quality_from_microsoft`, `quality_deferral_days` |
| OS updates | Mises à jour du système | Updates the operating system's own manager offers: apt, dnf or Windows Update | system packages, native updates | `include_os`, `skip_os`, `server_packages` |
| Third-party apps | Applications tierces | Software installed and updated by a third-party package manager: winget on Windows, Flatpak and Snap on Linux. Name the manager when there is one: "winget: 3 updates" | non-OS packages, 3rd-party software | `third_party`, `server_third_party_packages` |
| Software catalogue | Catalogue de logiciels | The software administrators can install on servers or remove from them: each entry a winget package and the source it installs from, shared by every customer or one customer's own. "Install software" and "Remove software" are its two actions (ADR-0188) | software library, app store, repository, deployment | `software_catalog`, `/api/software`, `software` command |
| Approved classifications | Classifications approuvées | The kinds of Windows updates a campaign or update installs on Windows servers, like Security updates or Critical updates. By default, every kind but feature upgrades (Upgrades) | categories, WSUS approvals, filters | `classifications` |
| Held update, held app | Mise à jour retenue, application retenue | A Windows update (KB number) or a winget app that a campaign or update never installs, whatever else selects it | blocked, excluded, blacklisted, pinned | `hold` |
| Allow installing and removing software from the web interface | Autoriser l’installation et la suppression de logiciels depuis l’interface web | A Windows server's consent, given on the server at install or later, that administrators may install and remove software of the catalogue there. Off unless given; nothing on the central server can give it | remote install, push, opt-in | `commands.software`, `SOFTWARE_FROM_WEB`, `software on` |
| Batch | Lot | Part of the servers of a campaign, updated together | wave | `waves` |
| Run, run again | Exécution, relancer | One pass of a campaign over its batches, from start to end. "Run again" starts another pass of a finished campaign, of all its servers or only the failed ones; the earlier runs stay listed as "Run 1", "Run 2" | rerun, retry campaign, restart, iteration | `campaign_runs`, `/campaigns/{id}/relaunch`, `/campaigns/{id}/runs` |
| Rollback | Retour arrière | What a change gets before it runs: a snapshot to go back to. "Rollback: LVM snapshot of / and /var"; without one, "Rollback limited" and why, like "/ is on plain ext4" | restore point, backup | `snapshots`, `/rollback` |
| Snapshot | Instantané | A copy of a server's system volumes (or its Proxmox VM) taken before a change, deleted after a day (classic LVM) or a week | checkpoint, LV, dataset (on screen) | `take_snapshot`, `server_storage` |
| Channel | Canal | Where notifications go: an email address, a webhook, or a Slack, Teams or Discord channel | destination, target, integration | `notification_channels` |
| Webhook URL | URL du webhook | The address a chat service gives for posting to one of its channels. It works as a password, so it is never shown again | token, connector | `secret` |
| Workflow | Workflow | What Teams calls the automation that gives such a URL, from the "Send webhook alerts to a channel" template | connector, except to name the retired ones | |
| Remove | Retirer | Revoke a server or relay and hide it from lists; for a mirror, delete it and its packages | delete | `revoked_at`, `deleting_at` |
| Revoke | Révoquer | Stop an install command, a token or a role from working | | |
| Notify about | Notifier pour | What a channel hears of: Everything, All of a customer, or one site | scope, coverage, routing | `customer_id`, `site_id`, `Coverage` |
| Everything | Tout | For a channel or a silence: all customers and sites, plus news about the central server: failed backups, automatic updates, new versions | global, all customers | both columns null; a silence with no target |
| Silence | Silence, mettre en silence | Stop notifications about a server, relay, site, customer or everything for a while, whatever channel they go to, or about chosen checks of a server or site: "Silence this check". Incidents still open and show; what is still open at the end is sent | mute, snooze, maintenance mode, downtime | `silences`, `checks` |
| Maintenance window | Fenêtre de maintenance ; « En maintenance jusqu’à 04:00 » | A silence that comes back on its own, every day, week or month, at local times in a time zone: "Every Sunday, 02:00 to 04:00 (Europe/Paris)". Covers a site, chosen servers or chosen checks. While one runs: "In maintenance until 04:00". For a campaign, the times updates may run in | recurring silence, downtime, schedule, blackout | `maintenance_windows`, `window_id` |
| Depends on, behind it | Dépend de, derrière | A server, device or check that cannot be checked when another one is down: "web-1 depends on sw-core", "12 servers are behind sw-core". Several are allowed. The relay of a server is one without being set: "Automatic" | parent, child, dependency (on screen), upstream | `dependencies`, `parent` |
| Unreachable | Injoignable | A server, device or check behind something that is down: "Unreachable: sw-core is down". It opens no incident and notifies nobody until 2 minutes after what it depends on is back | suppressed, masked, hidden | `state = 'unreachable'`, `unreachable_because` |
| Sent together | Envoyés ensemble | Incidents told in one notification: the same check on several machines, the rest of a site, or what a silence held back. Each incident still has its own page | group, batch, digest, bundle, aggregate | `notification_groups`, `?group=` |
| On call | Astreinte | Who is told of each alert of a place in person, besides the channels, in turns. A page under Monitoring | pager, duty, rota (on screen) | `on_call_schedules` |
| Schedule | Planning | The people who take turns being on call for everything, a customer or a site, and when they hand off | rotation, layer, policy | `on_call_schedules`, `on_call_members` |
| Hand-off | Relève | The moment one person's turn ends and the next one's starts: every day or every week, at a local time in the schedule's time zone | shift change, rollover | `handoff_time`, `anchor_date` |
| Cover | Remplacement ; « alice remplace bob » | Someone else is on call for a period, for a holiday or a swap: "alice covers bob" | override (on screen), substitute | `on_call_overrides` |
| On call at once | D’astreinte en même temps | How many people of a schedule are on call together in each turn, 1 to 5: "2 people at once", "alice and bob" | layer, team, pair (on screen) | `people_per_turn` |
| Gap | Période non couverte | A period when the person on call cannot be told: no email or chat, disabled, removed, or no longer sees the place. The nearest administrators are told instead | hole, uncovered | `problem` |
| How you are reached | Comment vous joindre | A person's own email address and optional chat or webhook, used when they are on call. Each person sets their own | contact method, notification rule | `user_contacts` |
| Acknowledge | Prendre en charge ; « Pris en charge par alice à 03:12 » | Say "I'm on it" about an alert: nobody else is told next. "Acknowledged by alice at 03:12". From the alert's page, the alerts list, or the link in a message sent to you | ack, accept, claim, take | `incident_acknowledgements`, `acknowledge` |
| Follow-up | Relance | Who is told next when nobody acknowledges a critical alert, step after step, for everything, a customer or a site. "bob will be told at 03:27 if nobody acknowledges" | escalation, escalate, escalation policy (an incident "escalated" got worse; the AI "escalates" to a person when it stops) | `follow_up_policies`, `incident_follow_ups` |
| Step | Étape | One part of a follow-up: after a wait without acknowledgement, tell the person on call of a schedule, named people, channels or the administrators | level, tier (tiers are the risk of AI commands) | `steps` |
| Only for follow-ups | Uniquement pour les relances | A channel or a schedule that hears only the alerts nobody acknowledged, when a follow-up step names it: a schedule like this is a second level behind the people told first | escalation channel, escalation level | `follow_up_only` |
| Integration | Intégration | Another tool that gets MonitorFlow's events, like a PSA or an automation: a URL that receives a signed JSON request per event. Under Settings › Integrations; for people, use a channel | endpoint, connector, hook, subscription | `integration_endpoints` |
| Signing secret | Secret de signature | What an integration's receiver checks each request with. Made by MonitorFlow and shown once; "New secret" replaces it at once | key, token, HMAC (on screen) | `secret`, `X-MonitorFlow-Signature` |
| Secrets key | Clé des secrets | The key, in the server's data, that opens the passwords, webhook URLs, signing secrets and AI key the server keeps to reach other services. It is in every backup and in the join file, never in the database | encryption key, master key, KEK | `secrets/keys`, `mfs1:` |
| Send again | Renvoyer | Deliver an event an integration already got, or every event of a time range, with the same IDs | replay, resend, redeliver | `/replay`, `X-MonitorFlow-Replay` |
| Turned off | Désactivée | An integration nothing reached for a day: it gets no new events until someone turns it on | disabled, suspended | `state = 'disabled'` |
| Custom webhook | Webhook personnalisé | In the catalogue of integrations, the card for an integration: a URL that receives a signed JSON request per event | generic integration, HTTP integration | `integration_endpoints` |
| PSA connection | Connexion PSA | MonitorFlow calling a PSA's API (HaloPSA first) with credentials it keeps, to open and close tickets. One per installation, set up by a global administrator | PSA integration, connector | `psa_connections` |
| Client ID, Client Secret | Client ID, Client Secret | What a HaloPSA API application gives for signing in. The secret is never shown again once saved | API key, password, token | `client_id`, `client_secret` |
| Test connection | Tester la connexion | Sign in to the PSA and read who MonitorFlow acts as, without changing anything there | ping, verify, check credentials | `/api/psa/connection/test` |
| Plugin | Plugin | A check or a notification channel someone else wrote, installed as a signed file under Settings › Plugins. "Asks for" lists what it may reach, like "Connects to api.pushover.net" | extension, add-on, module, wasm (on screen) | `plugins`, `.mfplugin` |
| Publisher | Éditeur | Who signed a plugin: MonitorFlow, or someone whose key a global administrator trusts | vendor, signer, author | `plugin_publishers`, `public_key` |
| Used where | Utilisable sur | Where a plugin may be used: every server (every customer), a customer or a site | scope, enabled for, allow-list | `plugin_places` |
| Certificate authority | Autorité de certification | What the central server uses to give every server and relay its certificate and to sign commands. Said in full on screen | CA, PKI, root | `ca`, `ca-cert.pem` |
| Change the certificate authority | Changer l’autorité de certification | Replace it with a new one while every server and relay keeps working: they learn the new one, move to it, then the old one is retired. An emergency change is for when its key may have leaked | rotate, rotation, roll over, re-key, compromise | `ca_rotation`, `ca.rotation.*` |
| Moved | Basculé | A server or relay whose certificate comes from the new certificate authority. "312 of 315 servers and relays moved" | migrated, re-issued | `moved`, `issuer_ca` |
| Needs a new install | À réinstaller | A server or relay that missed an emergency change and cannot connect until it is installed again | cut off, stranded, orphaned | `cut_off` |
| Dashboard | Tableau de bord | A page of charts and figures someone builds by picking from lists, under Monitoring | board, view (on screen), Grafana | `dashboards` |
| Panel | Panneau | One chart, figure, top list or list of open alerts on a dashboard | widget, tile, card | `panels` |
| Shared with | Partagé avec | Who sees a dashboard: Only me, a site or a customer. Operators of that site or customer change it | visibility, owner, scope | `shared_with` |
| Ready-made | Prêt à l’emploi | A dashboard MonitorFlow comes with, for every server you see. Make a copy to change it | built-in, template, default | |
| Show on a screen | Afficher sur un écran | A dashboard full screen, without menus, for a TV or a projector | kiosk mode, TV mode, presentation mode | `/display/` |
| Display link | Lien d’affichage | A link that shows dashboards on a screen without signing in, made by an administrator, turned off from the dashboard's page. "Reception TV" is its name | share link, public link, token, playlist | `display_links` |
| API key | Clé d’API | What a script or another tool (HaloPSA, Ansible) uses to work with MonitorFlow, made by an administrator under Settings › API keys and shown once. It does what its maker may, within what it reaches and its access: "Read only", "Read and change", "Approve commands", "Administer" | token, API token, secret, service account | `api_keys`, `mfk_` |
| Sign-in provider | Fournisseur de connexion | Where people of a company sign in, instead of a password kept by MonitorFlow: an OpenID Connect provider (Entra ID, Google, Keycloak, authentik) or an LDAP directory (Active Directory, OpenLDAP). Under Settings › Sign-in. "Sign in with Keycloak" on the login page | SSO, IdP, identity provider (on screen) | `identity_providers`, `oidc`, `ldap` |
| Directory | Annuaire | An LDAP sign-in provider, always over TLS: LDAPS or StartTLS | LDAP server, AD (unless the person picked Active Directory) | `kind: ldap` |
| Groups and roles | Groupes et rôles | Which role each group of a sign-in provider gives. Someone in none of them cannot sign in | mapping, claims, rules | `identity_provider_groups` |
| Local administrator | Administrateur local | An administrator of everything who signs in with a password kept by MonitorFlow, so a sign-in provider that is down never locks everyone out. There is always one | break-glass account, emergency account, root | `password_hash` |
| Test connection | Tester la connexion | Checking a sign-in provider before it is turned on; it must pass first | probe, validate | `/test` |
| Public API | API publique ; « Documentation de l’API » | The documented API under `/api/v1` for scripts and integrations; its page is "API documentation" | REST API, v1 (on screen) | `/api/v1` |
| Enterprise edition | Édition Enterprise | The features sold by subscription on top of the free core; today, more than one central server. Said only on the License page and in the guides of those features | EE, premium, pro, paid version | `ee/`, `enterprise` |
| License key | Clé de licence | The file that comes with a subscription: who it is for, the plan, how many managed and central servers it covers, until when. Pasted or uploaded under Settings › License | license file, activation code, serial | `license`, `MONITORFLOW LICENSE` |
| Subscription | Abonnement | What a company pays for: tested releases and support, and for Enterprise more central servers. Without one: the Community edition, with every feature. Named on the License page only (ADR-0147) | license (for the subscription itself), nag words like "unlicensed" or "trial" | `license` |
| Plan | Formule | Basic, Standard, Premium or Enterprise | tier, edition (on screen, except "Community edition" and "Enterprise edition") | `tier` |
| Tested releases | Versions testées | The releases a central server with a subscription is offered: promoted once they ran longer. Others are offered every release as soon as it is published | tested channel, regular channel, enterprise channel (on screen) | `channel`, `tested.json` |
| Add a central server | Ajouter un serveur central | Run another central server on the same database, so MonitorFlow keeps working when one machine stops. "2 of 3 central servers" | node, replica, cluster member, instance (on screen) | `server_instances`, `server_instance_admissions`, `--join`, `MONITORFLOW_ADDITIONAL_INSTANCE` |
| Turn off | Désactiver | Stop watching: no more alerts of a threshold, or offline incidents of a machine, until turned back on | mute | `offline_alerts`, `muted`, `enabled` |
| Rollback | Retour arrière | Going back to the snapshot taken before an AI change. "Snapshot first" when MonitorFlow takes one, "Rollback limited" when it cannot, with the reason: "the container's storage cannot take snapshots" | revert, restore point | `snapshots.available`, `reason` |
| VM, container | VM, conteneur | The Proxmox guest a server runs in: a virtual machine, or an LXC container. "VM 101 on pve1", "container 104 on pve2". The ID is Proxmox's | guest, CT, qemu, lxc (on screen) | `server_proxmox_vms`, `kind`, `vmid` |
| Matched automatically | Associé automatiquement | A server MonitorFlow tied to its Proxmox VM or container by itself: "Matched automatically to VM 104 on pve2 by its hardware ID". "Not this one" undoes it for good; "Change" picks another | discovered, auto-mapped | `matched_by` |
| Virtual machine | Machine virtuelle | A machine a hypervisor runs, listed from a vCenter, an ESXi host or a Hyper-V host whether or not it has the agent. "Virtual machine web-01 on host esx-02" | guest, VM object, instance (on screen) | `virtual_objects`, `kind = 'vm'` |
| Host | Hôte | A hypervisor machine of a vCenter, or the Hyper-V server itself, that runs virtual machines | hypervisor node, ESX box | `kind = 'host'` |
| Datastore | Banque de données | Where a vCenter keeps virtual machines' disks; "Datastore full" when under 10% is free | volume, LUN (on screen) | `kind = 'datastore'` |
| Snapshot (virtual machine) | Instantané | A saved state of a virtual machine kept by its hypervisor; Hyper-V calls them checkpoints, and the page says "Snapshots (checkpoints)" for Hyper-V. "Snapshot older than 7 days" | checkpoint alone | `vm_snapshots`, `vm_snapshot_age_seconds` |
| Install the agent | Installer l'agent | The button on a virtual machine without the agent: adds a server waiting for its agent and shows the command | deploy, onboard | `install-agent` |

Keep the technical word when it is the thing being asked for, such as a certificate, a mirror or a sudo rule, and put the rest under a "Details" disclosure. Certificate warnings speak of renewal, and appear only when it is late (under 2 days left) or failed.

ADR references stay out of screens and CLI help. In the install guides they are footnotes.

## French

The French column is what the French interface says for each word, so a term reads the same on every page. More words of the French interface:

| English | French |
| --- | --- |
| Fleet | Parc |
| Site | Site |
| Incident | Incident |
| Alert | Alerte |
| Sensor | Capteur |
| Check | Contrôle |
| Threshold | Seuil |
| Settings | Paramètres |
| Preferences | Préférences |
| Sidebar | Barre latérale |
| Monitoring | Supervision |
| Campaign | Campagne |
| Mirror (Linux mirrors in the menu) | Miroir (Miroirs Linux dans le menu) |
| Approval, approve | Approbation, approuver |
| Audit log | Journal d’audit |
| Administrator, operator, approver, viewer | Administrateur, opérateur, approbateur, lecteur |
| Critical, Warning, Unreachable, Offline, OK | Critique, Avertissement, Injoignable, Hors ligne, OK |

The French interface says "vous", keeps labels as short as the English ones, and follows French typography: a no-break space before `:` and inside `« »`, a narrow no-break space before `; ? !`, a curly apostrophe (’). Numbers, dates and units follow the language: "1 234,5", "85,3 %", "3 j 4 h", "Gio".
