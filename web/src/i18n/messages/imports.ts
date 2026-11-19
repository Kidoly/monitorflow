import type { Catalog, Translation } from "../catalog";

/**
 * Moving from another monitoring tool (ADR-0146): the Import page, its
 * preview, and the notes readers attach to items (`note.<code>`, the codes
 * of `crates/import/src/notes.rs`).
 */
export const en = {
  "note.builtin":
    "Every MonitorFlow server measures this already; nothing to add.",
  "note.offline_alert":
    "MonitorFlow's offline alert tells when the agent stops answering, after {minutes} minutes.",
  "note.web_password":
    "Passwords are not imported: set the password of {username} in the check after the import.",
  "note.web_token":
    "Tokens are not imported: set the token as the check's password after the import.",
  "note.web_password_http":
    "The source sends a password over plain http; MonitorFlow sends one only over https, so the check runs without it.",
  "note.web_auth":
    "MonitorFlow uses basic authentication, not {kind}: the check runs without it.",
  "note.web_method":
    "MonitorFlow sends GET or HEAD requests only: the check sends GET instead of {method}, without a body.",
  "note.web_regex":
    "MonitorFlow looks for plain text, not a pattern: the condition {pattern} is not moved.",
  "note.web_steps":
    "Only the first of {count} steps is moved: a MonitorFlow website check reads one page.",
  "note.web_setting":
    "The setting {setting} has no equivalent in a MonitorFlow website check; the rest is moved.",
  "note.web_header":
    "The header {name} is not moved: MonitorFlow sets it itself or does not keep it in clear.",
  "note.web_certificate_only":
    "The source checks only the certificate; MonitorFlow's website check also requests the page, whatever its status.",
  "note.web_response_phase":
    "MonitorFlow measures the whole response time, redirects included, not one phase of it.",
  "note.web_no_check":
    "It is about a website this import does not check.",
  "note.silence_next_window":
    "Recurring in the source: only the next window is added, from {starts} to {ends}.",
  "note.silence_cut":
    "MonitorFlow silences last at most 30 days: this one ends on {ends}.",
  "note.silence_over": "The window is over already.",
  "note.silence_whole_server":
    "MonitorFlow silences every alert of a server, not only the ones the source matched ({matchers}).",
  "note.smtp_password":
    "The mail server password is not in the source: type it in the channel after the import.",
  "note.no_smtp":
    "No mail server is set for this address in the source; add an email channel with yours.",
  "note.webhook_payload":
    "MonitorFlow sends its own signed JSON, not the source's message format.",
  "note.duration_rounded":
    "Waits {seconds} seconds before alerting, the nearest MonitorFlow allows.",
  "note.interval_clamped":
    "Runs every {seconds} seconds, the nearest interval MonitorFlow allows.",
  "note.severity_mapped": "The source's severity {severity} becomes {mapped}.",
  "note.process_as_service":
    "MonitorFlow watches the systemd service {name} instead of counting processes.",
  "note.disk_free_as_used":
    "Read as space used, which counts the space kept for the system as used.",
  "note.log_dirs":
    "The agent reads only logs under the directories its configuration allows.",
  "note.server_elsewhere":
    "A server of this name is in a site you do not administer.",
  "note.refused": "MonitorFlow refuses it: {reason}.",
  "note.unknown_host": "It refers to a host that is not part of this import.",
  "note.network_traffic":
    "MonitorFlow measures interface traffic but does not move thresholds on it: add one on the server's page.",
  "note.snmp_object": "An SNMP object outside the common MIBs MonitorFlow reads: interfaces, host resources, UPS, temperature sensors and printer supplies.",
  "note.dns_as_udp": "MonitorFlow checks that the DNS server answers a query; it does not compare the record.",
  "note.no_address": "There is no address MonitorFlow can check it at.",
  "note.device_no_agent": "It becomes a device, which has no agent to measure this.",
  "note.snmp_of_agent": "MonitorFlow polls SNMP on devices; on a server, its agent measures this itself.",
  "note.snmp_credentials": "SNMP credentials are not moved: set them on the device's page, and its SNMP checks start.",
  "note.vmware_password": "The vCenter password is not moved: set the credentials on the device's page (a Read-only account is enough), and its check starts with the alerts that come with it.",
  "note.vmware_discovered": "The source found it through its vCenter: it is listed on the vCenter's page, with the vCenter's alerts, and is not added again.",
  "note.vmware_every_datastore": "MonitorFlow applies this to every datastore of the vCenter, not to one.",
  "note.vmware_wbem": "MonitorFlow reads the host's hardware sensors through the vCenter API, not WBEM.",
  "note.vmware_of_agent": "MonitorFlow reads a vCenter or an ESXi host as a device, and this host has an agent.",
  "note.hyperv_group": "The agent's account must join Hyper-V Administrators on the host; then its Hyper-V check starts.",
  "note.hyperv_windows": "Hyper-V is read by the agent of a Windows server.",
  "note.virtual_partial": "MonitorFlow reads the state, processor, memory, storage and snapshots of each virtual machine; traffic per network adapter and storage latency are not measured.",
  "note.builtin_net": "Every server measures the traffic of its network interfaces already.",
  "note.no_equivalent": "MonitorFlow has no check of this kind.",
  "note.expression":
    "MonitorFlow cannot read this expression; add a check and an alert by hand if you need it.",
  "note.per_cpu_load":
    "The source compares the load per processor; MonitorFlow compares the load itself.",
  "note.script":
    "Commands run by the source are not moved: put the script in the agent's scripts directory and add a custom script check.",
  "note.no_target": "Notifications of this kind cannot be sent by MonitorFlow.",
  "note.chat_token":
    "The source posts with an app token; MonitorFlow needs an incoming webhook URL. Add a channel with one.",
  "note.no_hosts": "It covers no host of this import.",
  "note.disabled": "Turned off in the source.",
  "note.unresolved_macro":
    "It uses a value that is not in the source: {macro}.",
  "note.zabbix_stock_template":
    "Read from the defaults of the stock template {template}, which the export does not include: export it too if it was changed.",
  "note.zabbix_template_missing":
    "The template {template} is not among the uploaded files: export it too, or connect to the Zabbix API.",
  "note.zabbix_change":
    "Zabbix compares the last two values; MonitorFlow compares the change over {seconds} seconds.",
  "note.zabbix_fit":
    "Zabbix fits a {fit} trend; MonitorFlow fits a straight line.",
  "note.zabbix_trend":
    "Zabbix averages its hourly trends; MonitorFlow averages the values themselves over the same {seconds} seconds.",
  "note.zabbix_values_window":
    "Zabbix reads the last {count} values; MonitorFlow reads the last {seconds} seconds, the time they take.",
  "note.window_rounded":
    "MonitorFlow reads windows of 1 minute to 1 day; this one is {seconds} seconds.",
  "note.prometheus_deriv":
    "Prometheus fits a line to the samples (deriv); MonitorFlow divides the change by the time.",
  "note.prometheus_compare_window":
    "Prometheus compares single values; MonitorFlow compares averages over 5 minutes.",
  "note.prometheus_absent":
    "Prometheus alerts when no series at all is left (absent); MonitorFlow alerts on each series that stops.",
  "note.kind_taken":
    "MonitorFlow keeps one rule of each kind per measure, and moves {kept} on it.",
  "note.zabbix_partial":
    "Only the condition {kept} is moved; MonitorFlow does not check the rest of the expression.",
  "note.zabbix_no_measure":
    "MonitorFlow has no threshold on this measure ({key}).",
  "note.zabbix_bytes":
    "MonitorFlow compares shares of memory and disk space, not amounts such as {value}.",
  "note.zabbix_no_recipients":
    "An export has no users' media, so no recipients: connect to the Zabbix API to read them, or add the channel by hand.",
  "note.zabbix_web_step":
    "Only the first step of a web scenario is moved; this is about the step {step}.",
  "note.zabbix_not_set":
    "Its setting {setting} has no usable address in the source.",
  "note.scrape_as_port":
    "MonitorFlow checks that port {port} answers; it does not read the exporter's metrics.",
  "note.disk_wait":
    "MonitorFlow compares the average wait of the disk's operations, reads and writes together, which differs from what the source measures.",
  "note.cpu_busy_modes":
    "MonitorFlow counts every busy mode of the processors, also the ones the source leaves out.",
  "note.extra_condition":
    "The source adds a condition MonitorFlow does not check; only the threshold is moved.",
  "note.service_failed":
    "MonitorFlow alerts when the service is not active, not only when it failed.",
  "note.service_discovery":
    "The targets come from {kind}, which only a running Prometheus resolves: connect to it to read them.",
  "note.file_missing":
    "The configuration refers to {file}, which is not among the uploaded files.",
  "note.secret_hidden":
    "The source's API does not show this secret: upload its configuration file to move it, or add the channel by hand.",
  "note.secret_in_file":
    "The source reads this secret from {file}, which is not part of the import: add the channel by hand.",
  "note.routes":
    "MonitorFlow sends every alert to every channel of its site; the source's routing by labels is not moved.",

  "note.prtg_from_server": "Checked by the server's own agent, not from a probe: a network problem between them is not seen.",
  "note.prtg_sensor_type": "MonitorFlow has no check like this PRTG sensor ({type}).",
  "note.prtg_no_setting": "The sensor's settings do not say what it checks.",
  "note.prtg_limit": "MonitorFlow has no threshold for the limits of the channel {channel}.",
  "note.prtg_every_disk": "No drive or mount point in its name: the threshold applies to every disk of the server.",
  "note.prtg_event_levels": "No event type could be read: MonitorFlow counts critical events and errors.",
  "note.prtg_event_filter": "A filter on the message or on event IDs could not be moved: more events may match.",
  "note.prtg_event_count": "MonitorFlow alerts on the first matching event; PRTG alerted above {count}.",
  "note.prtg_same_port": "Another sensor of this device checks port {port} already.",
  "note.prtg_not_on_device": "It belongs to a probe or a group, not to a device.",
  "note.prtg_paused": "Paused in PRTG with no end: the silence lasts 30 days, the longest MonitorFlow allows.",
  "note.prtg_paused_by_rule": "Paused by a schedule in PRTG, which MonitorFlow does not have.",
  "note.dependency_whole_check": "The whole check depends on it: in the source only {trigger} did.",
  "note.dependency_unmapped": "It depends on {parent}, which is not moved.",
  "note.dependency_group": "Group {group} depends on it in PRTG: each of its devices does.",
  "note.prtg_paused_sensor": "A paused sensor MonitorFlow has no check for: silencing its whole server would hide more than PRTG did.",
  "note.service_hosts": "MonitorFlow counts each host whose problems the service's tags select as a whole member: {hosts}.",
  "note.service_children": "Child services are flattened: their hosts are members of this service, under its rule.",
  "note.service_rule": "The rule {rule} has no equivalent: the service is down while any member is down.",
  "note.service_weights": "Weights are not moved: every member counts the same.",
  "note.service_no_members": "No host of the import has a problem these tags select.",
  "note.sla_schedule": "The SLA counts only the hours of its schedule; MonitorFlow counts every hour, without planned maintenance.",
  "note.sla_downtimes": "The SLA's excluded downtimes are not moved; maintenances moved as maintenance windows are left out instead.",
  "note.sla_several": "{count} SLAs select this service: the strictest target, {target} %, is kept.",
  "note.prtg_business_process": "PRTG does not give which objects a business process channel watches: make it a business service under Reports. Its channels: {channels}.",
  "note.prtg_local_time": "The file does not say PRTG's time zone: the window is read as UTC.",
  "note.prtg_triggers": "MonitorFlow sends it every incident; PRTG used it only where a notification trigger named it.",
  "note.scan_disabled": "Turned off in the source: the scan is imported without its schedule, to run when asked.",
  "note.scan_cut": "A scan tries at most 1,024 addresses: the first 1,024 of the {count} the source scanned are kept.",
  "note.scan_skipped": "A scan takes IP addresses, blocks and ranges only: {entries} is left out.",
  "note.scan_ports_cut": "A scan tries at most 32 ports: the first 32 of {count} are kept.",
  "note.scan_every_hour": "The source scanned every {delay}; MonitorFlow scans every hour, the nearest it allows.",
  "note.scan_every_day": "The source scanned every {delay}; MonitorFlow scans every day, the nearest it allows.",
  "note.scan_every_week": "The source scanned every {delay}; MonitorFlow scans every week, the nearest it allows.",
  "note.scan_proposes": "The source added what it found by itself ({action}); MonitorFlow proposes what a scan finds, and a person adds it.",
  "note.scan_snmp_refused": "The SNMP credentials are not moved, as MonitorFlow refuses them: {reason}.",
  "note.zabbix_snmp_old": "MonitorFlow does not use SNMPv1: the scan tries the community with SNMPv2c.",
  "note.zabbix_snmp_protocol": "MonitorFlow does not use the SNMPv3 protocol {protocol}: the SNMP credentials are not moved.",
  "note.zabbix_snmp_first": "A scan tries one set of SNMP credentials besides those of the site's devices: only the first check's are kept.",
  "note.zabbix_autoregistration": "MonitorFlow agents connect with the install command of their site: no rule is needed to add them.",
  "note.prtg_scan_monthly": "PRTG scanned once a month; MonitorFlow scans every week, the least often it allows.",
  "note.prtg_discovery_ad": "PRTG takes these devices from Active Directory; a MonitorFlow scan needs IP addresses, blocks or ranges.",
  "note.window_cut": "MonitorFlow maintenance windows last at most 7 days: this one ends 7 days after it starts.",
  "note.window_offline_kept": "In MonitorFlow a server going offline still notifies: only its checks are silenced.",
  "note.zabbix_no_data": "Zabbix stopped collecting data during this maintenance; MonitorFlow keeps watching and only holds notifications.",
  "note.zabbix_tags_unmatched": "No imported check comes from a trigger with these tags ({tags}): the whole server is silenced.",
  "note.am_active_intervals": "Alertmanager sends this route only within its active time intervals, which MonitorFlow does not have.",
  "note.am_interval": "This time interval cannot be said as a MonitorFlow maintenance window: {detail}.",
  "note.am_months_weekly": "Weekdays of some months only: MonitorFlow's weekly windows apply every month.",
  "note.am_years": "Limited to some years: the window applies from the first of them to the last.",
  "note.am_route_labels": "The route also matched other labels ({matchers}): MonitorFlow holds every notification of these servers.",
  "note.prtg_schedule_unknown": "Paused by the schedule {schedule}, whose periods are not in the source or are a time table: it cannot be moved.",
  "origin.host": "Host",
  "origin.item": "Item",
  "origin.trigger": "Trigger",
  "origin.template": "Template",
  "origin.web_scenario": "Web scenario",
  "origin.maintenance": "Maintenance",
  "origin.media_type": "Media type",
  "origin.user_media": "User media",
  "origin.scrape_target": "Scrape target",
  "origin.alerting_rule": "Alerting rule",
  "origin.receiver": "Receiver",
  "origin.silence": "Silence",
  "origin.sensor": "Sensor",
  "origin.notification": "Notification",
  "origin.device": "Device",
  "origin.group": "Group",
  "origin.probe": "Probe",
  "origin.service": "Service",
  "origin.sla": "SLA",
  "origin.time_interval": "Time interval",
  "origin.schedule": "Schedule",
  "origin.discovery_rule": "Discovery rule",
  "origin.action": "Action",

  title: "Move from another tool",
  intro:
    "Read the configuration of the monitoring tool you use today, from an export file or its API. You see first what MonitorFlow does the same, what it does differently and what cannot be moved; nothing is created until you confirm. Servers then only need the agent, and the other tool can keep running until you switch.",
  noPermission: "Administrators only",
  noPermissionNote: "Ask an administrator to import from another tool.",
  "what.sources": "the tools you can import from",
  "what.history": "the imports",
  "what.run": "this import",
  noSources: "No tool can be imported from on this server.",
  "start.title": "Read a configuration",
  "start.source": "Import from",
  "start.site": "Servers go to the site",
  "start.zone": "Time zone of the source",
  "start.zoneNote": "Zabbix and PRTG keep their server’s local time: maintenance windows that do not say their time zone are read in this one.",
  "start.how": "How to read it",
  "start.files": "Upload export files",
  "start.connect": "Connect to its API",
  "start.chooseFiles": "Files",
  "start.many": "several at once",
  "start.optional": "(optional)",
  "start.readOnly":
    "MonitorFlow only reads from it, and does not keep the token.",
  "start.read": "Read and preview",
  "start.reading": "Reading…",
  "history.title": "Earlier imports",
  "history.open": "Open",
  "state.preview": "Preview",
  "state.confirmed": "Imported",
  "state.undone": "Undone",
  back: "Back to imports",
  "preview.title": "{source}: {input}",
  "preview.where": "Servers go to the site {site}.",
  "preview.until": "This preview can be confirmed until {when}.",
  "preview.present": {
    one: "{count} already in MonitorFlow",
    other: "{count} already in MonitorFlow",
  },
  "preview.create": {
    one: "Create {count} item",
    other: "Create {count} items",
  },
  "preview.creating": "Creating…",
  "preview.cancel": "Cancel",
  "preview.nothingYet":
    "Nothing is created until you confirm. Untick what you do not want; what needs it is left out too.",
  "list.exact": "Same in MonitorFlow",
  "list.approximated": "Approximated",
  "list.not_movable": "Cannot be moved",
  "listNote.exact": "MonitorFlow does the same thing.",
  "listNote.approximated":
    "MonitorFlow does something close; each line says how it differs.",
  showLines: { one: "Show the line", other: "Show the {count} lines" },
  "listNote.not_movable": "These stay behind; each line says why.",
  "kind.server": "Servers",
  "kind.check": "Checks",
  "kind.threshold": "Alerts",
  "kind.dependency": "Depends on",
  "kind.silence": "Silences",
  "kind.window": "Maintenance windows",
  "kind.network_scan": "Network scans",
  "kind.service": "Business services",
  "kind.channel": "Notification channels",
  "kind.builtin": "Measured on every server",
  "kind.other": "Other",
  "item.present": "Already there",
  "item.hosts": { one: "{count} server", other: "{count} servers" },
  "item.needsOut": "left out with what it needs",
  "item.silence": "From {from} to {to}: {reason}",
  "item.dependency": "{child} depends on {parent}",
  "item.silenceChecks": "From {from} to {to}, {checks}: {reason}",
  "item.window": "{rule}: {reason}",
  "item.windowChecks": "{rule}, {checks}: {reason}",
  "item.channel": "{kind} to {target}",
  "item.networkScan": "{name}: {addresses}, {every}, from {probe}",
  "item.addresses": { one: "{count} address", other: "{count} addresses" },
  "item.service": "{name}: {rule}",
  "item.serviceTarget": "{name}: {rule}, target {target}",
  "item.serviceAll": "down while any of {members} is",
  "item.serviceAtLeast": { one: "up while at least {count} of {members} is", other: "up while at least {count} of {members} are" },
  "channel.email": "Email",
  "channel.webhook": "Webhook",
  "count.server": { one: "{count} server", other: "{count} servers" },
  "count.check": { one: "{count} check", other: "{count} checks" },
  "count.threshold": { one: "{count} alert", other: "{count} alerts" },
  "count.service": { one: "{count} business service", other: "{count} business services" },
  "count.dependency": { one: "{count} dependency", other: "{count} dependencies" },
  "count.silence": { one: "{count} silence", other: "{count} silences" },
  "count.network_scan": { one: "{count} network scan", other: "{count} network scans" },
  "count.window": { one: "{count} maintenance window", other: "{count} maintenance windows" },
  "count.channel": { one: "{count} channel", other: "{count} channels" },
  "count.builtin": { one: "{count} item", other: "{count} items" },
  "count.other": { one: "{count} item", other: "{count} items" },
  "count.nothing": "nothing",
  "done.created": "Created {what}.",
  "done.nothing": "Nothing new was created: MonitorFlow had it all already.",
  "done.present": {
    one: "{count} was there already.",
    other: "{count} were there already.",
  },
  "done.skipped": { one: "{count} left out.", other: "{count} left out." },
  "done.failed": {
    one: "{count} item could not be created:",
    other: "{count} items could not be created:",
  },
  "done.agents": {
    one: "The server waits for its agent: install it with the server's name, and it takes over its checks.",
    other:
      "{count} servers wait for their agent: install it on each with its name, and it takes over its checks.",
  },
  "done.addServers": "Install commands",
  "done.removed": "Removed {what}.",
  "done.kept": "Kept, as their agent took them over: {names}.",
  "undo.trigger": "Undo this import…",
  "undo.title": "Undo this import?",
  "undo.consequence":
    "Removes the servers still waiting for their agent and the checks, alerts, silences and channels this import created. Servers whose agent arrived stay, with their checks.",
  "undo.confirm": "Undo the import",
  "field.url": "Address",
  "field.token": "API token",
  "field.alertmanager_url": "Alertmanager address",
  "field.apitoken": "API token",
  "field.username": "User name",
  "field.passhash": "Passhash",
  "field.password": "Password",
  "waiting.title": "Waiting for its agent",
  "waiting.note":
    "This server was imported. Install the agent on it: once it enrolls as {name}, it takes over the checks and alerts below.",
  "waiting.make": "Make an install command",
  "waiting.making": "Making…",
} as const satisfies Catalog;

export const fr: Translation<typeof en> = {
  "note.builtin": "Chaque serveur MonitorFlow le mesure déjà ; rien à ajouter.",
  "note.offline_alert":
    "L’alerte hors ligne de MonitorFlow prévient quand l’agent ne répond plus, après {minutes} minutes.",
  "note.web_password":
    "Les mots de passe ne sont pas importés : saisissez le mot de passe de {username} dans le contrôle après l’import.",
  "note.web_token":
    "Les jetons ne sont pas importés : saisissez le jeton comme mot de passe du contrôle après l’import.",
  "note.web_password_http":
    "La source envoie un mot de passe en http non chiffré ; MonitorFlow n’en envoie qu’en https, donc le contrôle tourne sans.",
  "note.web_auth":
    "MonitorFlow utilise l’authentification basique, pas {kind} : le contrôle tourne sans.",
  "note.web_method":
    "MonitorFlow n’envoie que des requêtes GET ou HEAD : le contrôle envoie GET au lieu de {method}, sans corps.",
  "note.web_regex":
    "MonitorFlow cherche du texte simple, pas un motif : la condition {pattern} n’est pas reprise.",
  "note.web_steps":
    "Seule la première des {count} étapes est reprise : un contrôle de site web MonitorFlow lit une seule page.",
  "note.web_setting":
    "Le réglage {setting} n’a pas d’équivalent dans un contrôle de site web MonitorFlow ; le reste est repris.",
  "note.web_header":
    "L’en-tête {name} n’est pas repris : MonitorFlow le pose lui-même ou ne le garde pas en clair.",
  "note.web_certificate_only":
    "La source ne vérifie que le certificat ; le contrôle de site web de MonitorFlow demande aussi la page, quel que soit son statut.",
  "note.web_response_phase":
    "MonitorFlow mesure le temps de réponse entier, redirections comprises, pas une de ses phases.",
  "note.web_no_check":
    "Il porte sur un site web que cet import ne contrôle pas.",
  "note.silence_next_window":
    "Récurrente dans la source : seule la prochaine fenêtre est ajoutée, du {starts} au {ends}.",
  "note.silence_cut":
    "Un silence MonitorFlow dure au plus 30 jours : celui-ci finit le {ends}.",
  "note.silence_over": "La fenêtre est déjà passée.",
  "note.silence_whole_server":
    "MonitorFlow met en silence toutes les alertes d’un serveur, pas seulement celles que la source visait ({matchers}).",
  "note.smtp_password":
    "Le mot de passe du serveur de messagerie n’est pas dans la source : saisissez-le dans le canal après l’import.",
  "note.no_smtp":
    "Aucun serveur de messagerie n’est indiqué pour cette adresse dans la source ; ajoutez un canal e-mail avec le vôtre.",
  "note.webhook_payload":
    "MonitorFlow envoie son propre JSON signé, pas le format de message de la source.",
  "note.duration_rounded":
    "Attend {seconds} secondes avant d’alerter, la valeur la plus proche que MonitorFlow permet.",
  "note.interval_clamped":
    "S’exécute toutes les {seconds} secondes, l’intervalle le plus proche que MonitorFlow permet.",
  "note.severity_mapped":
    "La gravité {severity} de la source devient {mapped}.",
  "note.process_as_service":
    "MonitorFlow surveille le service systemd {name} au lieu de compter les processus.",
  "note.disk_free_as_used":
    "Lu comme l’espace utilisé, qui compte comme utilisé l’espace réservé au système.",
  "note.log_dirs":
    "L’agent ne lit que les journaux des répertoires que sa configuration autorise.",
  "note.server_elsewhere":
    "Un serveur de ce nom est dans un site que vous n’administrez pas.",
  "note.refused": "MonitorFlow le refuse : {reason}.",
  "note.unknown_host":
    "Il désigne un hôte qui ne fait pas partie de cet import.",
  "note.network_traffic":
    "MonitorFlow mesure le trafic des interfaces mais n’en reprend pas les seuils : ajoutez-en un sur la page du serveur.",
  "note.snmp_object": "Un objet SNMP hors des MIB courantes que lit MonitorFlow : interfaces, ressources de l’hôte, onduleur, capteurs de température et consommables d’imprimante.",
  "note.dns_as_udp": "MonitorFlow vérifie que le serveur DNS répond à une requête ; il ne compare pas l’enregistrement.",
  "note.no_address": "Aucune adresse à laquelle MonitorFlow peut le vérifier.",
  "note.device_no_agent": "Il devient un équipement, sans agent pour mesurer cela.",
  "note.snmp_of_agent": "MonitorFlow interroge SNMP sur les équipements ; sur un serveur, son agent le mesure lui-même.",
  "note.snmp_credentials": "Les identifiants SNMP ne sont pas repris : saisissez-les sur la page de l’équipement et ses contrôles SNMP démarrent.",
  "note.vmware_password": "Le mot de passe du vCenter n’est pas repris : saisissez les identifiants sur la page de l’équipement (un compte en lecture seule suffit) et son contrôle démarre avec les alertes qui l’accompagnent.",
  "note.vmware_discovered": "La source l’a trouvé par son vCenter : il est listé sur la page du vCenter, avec les alertes du vCenter, et n’est pas ajouté une seconde fois.",
  "note.vmware_every_datastore": "MonitorFlow l’applique à toutes les banques de données du vCenter, pas à une seule.",
  "note.vmware_wbem": "MonitorFlow lit les capteurs matériels de l’hôte par l’API du vCenter, pas par WBEM.",
  "note.vmware_of_agent": "MonitorFlow lit un vCenter ou un hôte ESXi comme un équipement, et cet hôte a un agent.",
  "note.hyperv_group": "Le compte de l’agent doit rejoindre les Administrateurs Hyper-V de l’hôte ; son contrôle Hyper-V démarre ensuite.",
  "note.hyperv_windows": "Hyper-V est lu par l’agent d’un serveur Windows.",
  "note.virtual_partial": "MonitorFlow lit l’état, le processeur, la mémoire, le stockage et les instantanés de chaque machine virtuelle ; le trafic par carte réseau et la latence du stockage ne sont pas mesurés.",
  "note.builtin_net": "Chaque serveur mesure déjà le trafic de ses interfaces réseau.",
  "note.no_equivalent": "MonitorFlow n’a pas de vérification de ce genre.",
  "note.expression":
    "MonitorFlow ne sait pas lire cette expression ; ajoutez une vérification et une alerte à la main si vous en avez besoin.",
  "note.per_cpu_load":
    "La source compare la charge par processeur ; MonitorFlow compare la charge elle-même.",
  "note.script":
    "Les commandes lancées par la source ne sont pas reprises : placez le script dans le répertoire de scripts de l’agent et ajoutez une vérification par script.",
  "note.no_target":
    "MonitorFlow ne sait pas envoyer de notifications de ce genre.",
  "note.chat_token":
    "La source publie avec un jeton d’application ; MonitorFlow a besoin d’une URL de webhook entrant. Ajoutez un canal avec la vôtre.",
  "note.no_hosts": "Il ne couvre aucun hôte de cet import.",
  "note.disabled": "Désactivé dans la source.",
  "note.unresolved_macro":
    "Il utilise une valeur absente de la source : {macro}.",
  "note.zabbix_stock_template":
    "Lu dans les valeurs par défaut du modèle fourni {template}, absent de l’export : exportez-le aussi s’il a été modifié.",
  "note.zabbix_template_missing":
    "Le modèle {template} n’est pas parmi les fichiers envoyés : exportez-le aussi, ou connectez-vous à l’API de Zabbix.",
  "note.zabbix_change":
    "Zabbix compare les deux dernières valeurs, MonitorFlow la variation sur {seconds} secondes.",
  "note.zabbix_fit":
    "Zabbix ajuste une tendance {fit}, MonitorFlow une droite.",
  "note.zabbix_trend":
    "Zabbix fait la moyenne de ses tendances horaires, MonitorFlow celle des valeurs elles-mêmes sur les mêmes {seconds} secondes.",
  "note.zabbix_values_window":
    "Zabbix lit les {count} dernières valeurs, MonitorFlow les {seconds} dernières secondes, le temps qu’elles prennent.",
  "note.window_rounded":
    "MonitorFlow lit des fenêtres de 1 minute à 1 jour, celle-ci fait {seconds} secondes.",
  "note.prometheus_deriv":
    "Prometheus ajuste une droite aux mesures (deriv), MonitorFlow divise la variation par la durée.",
  "note.prometheus_compare_window":
    "Prometheus compare des valeurs isolées, MonitorFlow des moyennes sur 5 minutes.",
  "note.prometheus_absent":
    "Prometheus alerte quand il ne reste aucune série (absent), MonitorFlow sur chaque série qui s’arrête.",
  "note.kind_taken":
    "MonitorFlow garde une règle de chaque sorte par mesure, et y reprend {kept}.",
  "note.zabbix_partial":
    "Seule la condition {kept} est reprise ; MonitorFlow ne vérifie pas le reste de l’expression.",
  "note.zabbix_no_measure":
    "MonitorFlow n’a pas de seuil sur cette mesure ({key}).",
  "note.zabbix_bytes":
    "MonitorFlow compare des parts de mémoire et d’espace disque, pas des quantités comme {value}.",
  "note.zabbix_no_recipients":
    "Un export ne contient pas les médias des utilisateurs, donc aucun destinataire : connectez-vous à l’API de Zabbix pour les lire, ou ajoutez le canal à la main.",
  "note.zabbix_web_step":
    "Seule la première étape d’un scénario web est reprise ; celui-ci porte sur l’étape {step}.",
  "note.zabbix_not_set":
    "Son paramètre {setting} n’a pas d’adresse utilisable dans la source.",
  "note.scrape_as_port":
    "MonitorFlow vérifie que le port {port} répond ; il ne lit pas les métriques de l’exportateur.",
  "note.disk_wait":
    "MonitorFlow compare l’attente moyenne des opérations du disque, lectures et écritures ensemble, ce qui diffère de ce que mesure la source.",
  "note.cpu_busy_modes":
    "MonitorFlow compte tous les modes occupés des processeurs, y compris ceux que la source laisse de côté.",
  "note.extra_condition":
    "La source ajoute une condition que MonitorFlow ne vérifie pas ; seul le seuil est repris.",
  "note.service_failed":
    "MonitorFlow alerte quand le service n’est pas actif, pas seulement quand il a échoué.",
  "note.service_discovery":
    "Les cibles viennent de {kind}, que seul un Prometheus en marche résout : connectez-vous à lui pour les lire.",
  "note.file_missing":
    "La configuration désigne {file}, qui ne fait pas partie des fichiers envoyés.",
  "note.secret_hidden":
    "L’API de la source ne montre pas ce secret : envoyez son fichier de configuration pour le reprendre, ou ajoutez le canal à la main.",
  "note.secret_in_file":
    "La source lit ce secret dans {file}, qui ne fait pas partie de l’import : ajoutez le canal à la main.",
  "note.routes":
    "MonitorFlow envoie chaque alerte à chaque canal de son site ; le routage par étiquettes de la source n’est pas repris.",

  "note.prtg_from_server": "Vérifié par l’agent du serveur lui-même, pas depuis une sonde : un problème réseau entre les deux n’est pas vu.",
  "note.prtg_sensor_type": "MonitorFlow n’a pas de vérification comme ce capteur PRTG ({type}).",
  "note.prtg_no_setting": "Les réglages du capteur ne disent pas ce qu’il vérifie.",
  "note.prtg_limit": "MonitorFlow n’a pas de seuil pour les limites du canal {channel}.",
  "note.prtg_every_disk": "Pas de lecteur ni de point de montage dans son nom : le seuil s’applique à tous les disques du serveur.",
  "note.prtg_event_levels": "Aucun type d’événement n’a pu être lu : MonitorFlow compte les événements critiques et les erreurs.",
  "note.prtg_event_filter": "Un filtre sur le message ou sur les ID d’événement n’a pas pu être repris : plus d’événements peuvent correspondre.",
  "note.prtg_event_count": "MonitorFlow alerte dès le premier événement correspondant ; PRTG alertait au-delà de {count}.",
  "note.prtg_same_port": "Un autre capteur de cet équipement vérifie déjà le port {port}.",
  "note.prtg_not_on_device": "Il appartient à une sonde ou à un groupe, pas à un équipement.",
  "note.prtg_paused": "En pause dans PRTG sans fin prévue : le silence dure 30 jours, le plus long que MonitorFlow permet.",
  "note.prtg_paused_by_rule": "En pause dans PRTG par un horaire, que MonitorFlow n’a pas.",
  "note.dependency_whole_check": "Tout le contrôle en dépend : dans la source, seul {trigger} en dépendait.",
  "note.dependency_unmapped": "Il dépend de {parent}, qui n’est pas repris.",
  "note.dependency_group": "Le groupe {group} en dépend dans PRTG : chacun de ses équipements aussi.",
  "note.prtg_paused_sensor": "Un capteur en pause sans contrôle équivalent dans MonitorFlow : mettre tout son serveur en silence cacherait plus que PRTG.",
  "note.service_hosts": "MonitorFlow compte chaque hôte dont le service sélectionne les problèmes par ses tags comme un membre entier\u00a0: {hosts}.",
  "note.service_children": "Les services enfants sont aplatis\u00a0: leurs hôtes sont membres de ce service, sous sa règle.",
  "note.service_rule": "La règle {rule} n’a pas d’équivalent\u00a0: le service est en panne dès qu’un membre l’est.",
  "note.service_weights": "Les poids ne sont pas repris\u00a0: chaque membre compte autant.",
  "note.service_no_members": "Aucun hôte de l’import n’a de problème que ces tags sélectionnent.",
  "note.sla_schedule": "Le SLA ne compte que les heures de son horaire\u00a0; MonitorFlow compte toutes les heures, hors maintenance planifiée.",
  "note.sla_downtimes": "Les arrêts exclus du SLA ne sont pas repris\u00a0; les maintenances reprises comme fenêtres de maintenance sont exclues à la place.",
  "note.sla_several": "{count} SLA sélectionnent ce service\u00a0: l’objectif le plus strict, {target}\u00a0%, est gardé.",
  "note.prtg_business_process": "PRTG ne donne pas les objets que surveille un canal de processus métier\u00a0: créez un service métier dans Rapports. Ses canaux\u00a0: {channels}.",
  "note.prtg_local_time": "Le fichier ne dit pas le fuseau horaire de PRTG : la fenêtre est lue en UTC.",
  "note.prtg_triggers": "MonitorFlow lui envoie chaque incident ; PRTG ne l’utilisait que là où un déclencheur de notification le nommait.",
  "note.scan_disabled": "Désactivée dans la source : l’analyse est importée sans sa fréquence, à lancer à la demande.",
  "note.scan_cut": "Une analyse essaie au plus 1 024 adresses : les 1 024 premières des {count} que la source analysait sont gardées.",
  "note.scan_skipped": "Une analyse ne prend que des adresses IP, des blocs et des plages : {entries} est laissé de côté.",
  "note.scan_ports_cut": "Une analyse essaie au plus 32 ports : les 32 premiers des {count} sont gardés.",
  "note.scan_every_hour": "La source analysait avec un délai de {delay} ; MonitorFlow analyse toutes les heures, la fréquence la plus proche qu’il permet.",
  "note.scan_every_day": "La source analysait avec un délai de {delay} ; MonitorFlow analyse tous les jours, la fréquence la plus proche qu’il permet.",
  "note.scan_every_week": "La source analysait avec un délai de {delay} ; MonitorFlow analyse toutes les semaines, la fréquence la plus proche qu’il permet.",
  "note.scan_proposes": "La source ajoutait elle-même ce qu’elle trouvait ({action}) ; MonitorFlow propose ce qu’une analyse trouve, et une personne l’ajoute.",
  "note.scan_snmp_refused": "Les identifiants SNMP ne sont pas repris, car MonitorFlow les refuse : {reason}.",
  "note.zabbix_snmp_old": "MonitorFlow n’utilise pas SNMPv1 : l’analyse essaie la communauté en SNMPv2c.",
  "note.zabbix_snmp_protocol": "MonitorFlow n’utilise pas le protocole SNMPv3 {protocol} : les identifiants SNMP ne sont pas repris.",
  "note.zabbix_snmp_first": "Une analyse essaie un seul jeu d’identifiants SNMP en plus de ceux des équipements du site : seuls ceux de la première vérification sont gardés.",
  "note.zabbix_autoregistration": "Les agents MonitorFlow se connectent avec la commande d’installation de leur site : aucune règle n’est nécessaire pour les ajouter.",
  "note.prtg_scan_monthly": "PRTG analysait une fois par mois ; MonitorFlow analyse toutes les semaines, le moins souvent qu’il permet.",
  "note.prtg_discovery_ad": "PRTG prend ces équipements dans Active Directory ; une analyse MonitorFlow demande des adresses IP, des blocs ou des plages.",
  "note.window_cut": "Les fenêtres de maintenance de MonitorFlow durent au plus 7 jours : celle-ci se termine 7 jours après son début.",
  "note.window_offline_kept": "Dans MonitorFlow, un serveur hors ligne prévient toujours : seuls ses contrôles sont mis en silence.",
  "note.zabbix_no_data": "Zabbix arrêtait la collecte pendant cette maintenance ; MonitorFlow continue de surveiller et retient seulement les notifications.",
  "note.zabbix_tags_unmatched": "Aucun contrôle importé ne vient d’un déclencheur avec ces étiquettes ({tags}) : tout le serveur est mis en silence.",
  "note.am_active_intervals": "Alertmanager n’envoie cette route que pendant ses intervalles actifs, que MonitorFlow n’a pas.",
  "note.am_interval": "Cet intervalle ne peut pas s’écrire comme une fenêtre de maintenance MonitorFlow : {detail}.",
  "note.am_months_weekly": "Des jours de la semaine de certains mois seulement : les fenêtres hebdomadaires de MonitorFlow valent tous les mois.",
  "note.am_years": "Limité à certaines années : la fenêtre vaut de la première à la dernière.",
  "note.am_route_labels": "La route visait aussi d’autres étiquettes ({matchers}) : MonitorFlow retient toutes les notifications de ces serveurs.",
  "note.prtg_schedule_unknown": "En pause selon le planning {schedule}, dont les périodes ne sont pas dans la source ou sont une grille horaire : il ne peut pas être déplacé.",
  "origin.host": "Hôte",
  "origin.item": "Élément",
  "origin.trigger": "Déclencheur",
  "origin.template": "Modèle",
  "origin.web_scenario": "Scénario web",
  "origin.maintenance": "Maintenance",
  "origin.media_type": "Type de média",
  "origin.user_media": "Média d’utilisateur",
  "origin.scrape_target": "Cible de collecte",
  "origin.alerting_rule": "Règle d’alerte",
  "origin.receiver": "Destinataire",
  "origin.silence": "Silence",
  "origin.sensor": "Capteur",
  "origin.notification": "Notification",
  "origin.device": "Équipement",
  "origin.group": "Groupe",
  "origin.probe": "Sonde",
  "origin.service": "Service",
  "origin.sla": "SLA",
  "origin.time_interval": "Intervalle de temps",
  "origin.schedule": "Planning",
  "origin.discovery_rule": "Règle de découverte",
  "origin.action": "Action",
  title: "Venir d’un autre outil",
  intro:
    "Lisez la configuration de l’outil de supervision que vous utilisez aujourd’hui, depuis un fichier d’export ou son API. Vous voyez d’abord ce que MonitorFlow fait pareil, ce qu’il fait autrement et ce qui ne peut pas être repris ; rien n’est créé avant votre confirmation. Les serveurs n’ont ensuite besoin que de l’agent, et l’autre outil peut continuer de tourner jusqu’à la bascule.",
  noPermission: "Réservé aux administrateurs",
  noPermissionNote:
    "Demandez à un administrateur d’importer depuis un autre outil.",
  "what.sources": "les outils depuis lesquels importer",
  "what.history": "les imports",
  "what.run": "cet import",
  noSources: "Ce serveur ne sait importer depuis aucun outil.",
  "start.title": "Lire une configuration",
  "start.source": "Importer depuis",
  "start.site": "Les serveurs vont dans le site",
  "start.zone": "Fuseau horaire de la source",
  "start.zoneNote": "Zabbix et PRTG gardent l’heure locale de leur serveur : les fenêtres de maintenance qui ne disent pas leur fuseau sont lues dans celui-ci.",
  "start.how": "Comment la lire",
  "start.files": "Envoyer des fichiers d’export",
  "start.connect": "Se connecter à son API",
  "start.chooseFiles": "Fichiers",
  "start.many": "plusieurs à la fois",
  "start.optional": "(facultatif)",
  "start.readOnly": "MonitorFlow ne fait que lire, et ne garde pas le jeton.",
  "start.read": "Lire et prévisualiser",
  "start.reading": "Lecture…",
  "history.title": "Imports précédents",
  "history.open": "Ouvrir",
  "state.preview": "Aperçu",
  "state.confirmed": "Importé",
  "state.undone": "Annulé",
  back: "Retour aux imports",
  "preview.title": "{source} : {input}",
  "preview.where": "Les serveurs vont dans le site {site}.",
  "preview.until": "Cet aperçu peut être confirmé jusqu’au {when}.",
  "preview.present": {
    one: "{count} déjà dans MonitorFlow",
    other: "{count} déjà dans MonitorFlow",
  },
  "preview.create": {
    one: "Créer {count} élément",
    other: "Créer {count} éléments",
  },
  "preview.creating": "Création…",
  "preview.cancel": "Annuler",
  "preview.nothingYet":
    "Rien n’est créé avant votre confirmation. Décochez ce que vous ne voulez pas ; ce qui en dépend est laissé de côté aussi.",
  "list.exact": "Pareil dans MonitorFlow",
  "list.approximated": "Approché",
  "list.not_movable": "Ne peut pas être repris",
  "listNote.exact": "MonitorFlow fait la même chose.",
  "listNote.approximated":
    "MonitorFlow fait quelque chose de proche ; chaque ligne dit en quoi il diffère.",
  showLines: { one: "Afficher la ligne", other: "Afficher les {count} lignes" },
  "listNote.not_movable": "Ceci reste derrière ; chaque ligne dit pourquoi.",
  "kind.server": "Serveurs",
  "kind.check": "Vérifications",
  "kind.threshold": "Alertes",
  "kind.dependency": "Dépend de",
  "kind.silence": "Silences",
  "kind.window": "Fenêtres de maintenance",
  "kind.network_scan": "Analyses du réseau",
  "kind.service": "Services métier",
  "kind.channel": "Canaux de notification",
  "kind.builtin": "Mesuré sur chaque serveur",
  "kind.other": "Autre",
  "item.present": "Déjà là",
  "item.hosts": { one: "{count} serveur", other: "{count} serveurs" },
  "item.needsOut": "laissé de côté avec ce dont il dépend",
  "item.silence": "Du {from} au {to} : {reason}",
  "item.dependency": "{child} dépend de {parent}",
  "item.silenceChecks": "Du {from} au {to}, {checks} : {reason}",
  "item.window": "{rule} : {reason}",
  "item.windowChecks": "{rule}, {checks} : {reason}",
  "item.channel": "{kind} vers {target}",
  "item.networkScan": "{name} : {addresses}, {every}, depuis {probe}",
  "item.addresses": { one: "{count} adresse", other: "{count} adresses" },
  "item.service": "{name}\u00a0: {rule}",
  "item.serviceTarget": "{name}\u00a0: {rule}, objectif {target}",
  "item.serviceAll": "en panne dès que l’un de {members} l’est",
  "item.serviceAtLeast": { one: "fonctionne tant qu’au moins {count} de {members} fonctionne", other: "fonctionne tant qu’au moins {count} de {members} fonctionnent" },
  "channel.email": "E-mail",
  "channel.webhook": "Webhook",
  "count.server": { one: "{count} serveur", other: "{count} serveurs" },
  "count.check": {
    one: "{count} vérification",
    other: "{count} vérifications",
  },
  "count.threshold": { one: "{count} alerte", other: "{count} alertes" },
  "count.service": { one: "{count} service métier", other: "{count} services métier" },
  "count.dependency": { one: "{count} dépendance", other: "{count} dépendances" },
  "count.silence": { one: "{count} silence", other: "{count} silences" },
  "count.network_scan": { one: "{count} analyse du réseau", other: "{count} analyses du réseau" },
  "count.window": { one: "{count} fenêtre de maintenance", other: "{count} fenêtres de maintenance" },
  "count.channel": { one: "{count} canal", other: "{count} canaux" },
  "count.builtin": { one: "{count} élément", other: "{count} éléments" },
  "count.other": { one: "{count} élément", other: "{count} éléments" },
  "count.nothing": "rien",
  "done.created": "Créé : {what}.",
  "done.nothing": "Rien de nouveau n’a été créé : MonitorFlow avait déjà tout.",
  "done.present": {
    one: "{count} était déjà là.",
    other: "{count} étaient déjà là.",
  },
  "done.skipped": {
    one: "{count} laissé de côté.",
    other: "{count} laissés de côté.",
  },
  "done.failed": {
    one: "{count} élément n’a pas pu être créé :",
    other: "{count} éléments n’ont pas pu être créés :",
  },
  "done.agents": {
    one: "Le serveur attend son agent : installez-le avec le nom du serveur, et il reprend ses vérifications.",
    other:
      "{count} serveurs attendent leur agent : installez-le sur chacun avec son nom, et il reprend ses vérifications.",
  },
  "done.addServers": "Commandes d’installation",
  "done.removed": "Supprimé : {what}.",
  "done.kept": "Gardés, car leur agent les a repris : {names}.",
  "undo.trigger": "Annuler cet import…",
  "undo.title": "Annuler cet import ?",
  "undo.consequence":
    "Supprime les serveurs qui attendent encore leur agent et les vérifications, alertes, silences et canaux créés par cet import. Les serveurs dont l’agent est arrivé restent, avec leurs vérifications.",
  "undo.confirm": "Annuler l’import",
  "field.url": "Adresse",
  "field.token": "Jeton d’API",
  "field.alertmanager_url": "Adresse d’Alertmanager",
  "field.apitoken": "Jeton d’API",
  "field.username": "Nom d’utilisateur",
  "field.passhash": "Passhash",
  "field.password": "Mot de passe",
  "waiting.title": "En attente de son agent",
  "waiting.note":
    "Ce serveur a été importé. Installez-y l’agent : dès qu’il s’inscrit sous le nom {name}, il reprend les vérifications et alertes ci-dessous.",
  "waiting.make": "Créer une commande d’installation",
  "waiting.making": "Création…",
};
