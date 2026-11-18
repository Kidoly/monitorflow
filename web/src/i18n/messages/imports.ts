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
  "note.web_as_port":
    "MonitorFlow checks that port {port} of {host} answers; it does not read the page or its status code.",
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
  "note.udp": "MonitorFlow checks TCP ports only.",
  "note.network_traffic":
    "MonitorFlow does not measure network interface traffic yet.",
  "note.no_equivalent": "MonitorFlow has no check of this kind.",
  "note.expression":
    "MonitorFlow cannot read this expression; add a check and an alert by hand if you need it.",
  "note.per_cpu_load":
    "The source compares the load per processor; MonitorFlow compares the load itself.",
  "note.script":
    "Commands run by the source are not moved: put the script in the agent's scripts directory and add a custom script check.",
  "note.snmp":
    "MonitorFlow watches servers through its agent; SNMP devices cannot be moved.",
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
  "note.zabbix_partial":
    "Only the condition {kept} is moved; MonitorFlow does not check the rest of the expression.",
  "note.zabbix_no_measure":
    "MonitorFlow has no threshold on this measure ({key}).",
  "note.zabbix_bytes":
    "MonitorFlow compares shares of memory and disk space, not amounts such as {value}.",
  "note.zabbix_no_recipients":
    "An export has no users' media, so no recipients: connect to the Zabbix API to read them, or add the channel by hand.",
  "note.zabbix_not_set":
    "Its setting {setting} has no usable address in the source.",
  "note.scrape_as_port":
    "MonitorFlow checks that port {port} answers; it does not read the exporter's metrics.",
  "note.ping_as_offline":
    "The source pings the host; MonitorFlow's offline alert tells when its agent stops answering.",
  "note.ping":
    "MonitorFlow does not ping addresses; it watches servers through their agent.",
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

  "note.prtg_ping": "PRTG pinged it from a probe; MonitorFlow's offline alert tells when its agent stops answering. Ping times are not measured.",
  "note.prtg_from_server": "Checked by the server's own agent, not from a probe: a network problem between them is not seen.",
  "note.prtg_certificate": "MonitorFlow checks that port {port} answers; the certificate and its expiry are not checked.",
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
  "note.prtg_paused_by_rule": "Paused by a schedule or a dependency in PRTG, which MonitorFlow does not have.",
  "note.prtg_paused_sensor": "A paused sensor MonitorFlow has no check for: silencing its whole server would hide more than PRTG did.",
  "note.prtg_local_time": "The file does not say PRTG's time zone: the window is read as UTC.",
  "note.prtg_triggers": "MonitorFlow sends it every incident; PRTG used it only where a notification trigger named it.",
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
  "origin.time_interval": "Time interval",
  "origin.schedule": "Schedule",

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
  "kind.silence": "Silences",
  "kind.window": "Maintenance windows",
  "kind.channel": "Notification channels",
  "kind.builtin": "Measured on every server",
  "kind.other": "Other",
  "item.present": "Already there",
  "item.hosts": { one: "{count} server", other: "{count} servers" },
  "item.needsOut": "left out with what it needs",
  "item.silence": "From {from} to {to}: {reason}",
  "item.silenceChecks": "From {from} to {to}, {checks}: {reason}",
  "item.window": "{rule}: {reason}",
  "item.windowChecks": "{rule}, {checks}: {reason}",
  "item.channel": "{kind} to {target}",
  "channel.email": "Email",
  "channel.webhook": "Webhook",
  "count.server": { one: "{count} server", other: "{count} servers" },
  "count.check": { one: "{count} check", other: "{count} checks" },
  "count.threshold": { one: "{count} alert", other: "{count} alerts" },
  "count.silence": { one: "{count} silence", other: "{count} silences" },
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
  "field.username": "User name",
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
  "note.web_as_port":
    "MonitorFlow vérifie que le port {port} de {host} répond ; il ne lit ni la page ni son code de statut.",
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
  "note.udp": "MonitorFlow ne vérifie que les ports TCP.",
  "note.network_traffic":
    "MonitorFlow ne mesure pas encore le trafic des interfaces réseau.",
  "note.no_equivalent": "MonitorFlow n’a pas de vérification de ce genre.",
  "note.expression":
    "MonitorFlow ne sait pas lire cette expression ; ajoutez une vérification et une alerte à la main si vous en avez besoin.",
  "note.per_cpu_load":
    "La source compare la charge par processeur ; MonitorFlow compare la charge elle-même.",
  "note.script":
    "Les commandes lancées par la source ne sont pas reprises : placez le script dans le répertoire de scripts de l’agent et ajoutez une vérification par script.",
  "note.snmp":
    "MonitorFlow surveille les serveurs par son agent ; les équipements SNMP ne peuvent pas être repris.",
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
  "note.zabbix_partial":
    "Seule la condition {kept} est reprise ; MonitorFlow ne vérifie pas le reste de l’expression.",
  "note.zabbix_no_measure":
    "MonitorFlow n’a pas de seuil sur cette mesure ({key}).",
  "note.zabbix_bytes":
    "MonitorFlow compare des parts de mémoire et d’espace disque, pas des quantités comme {value}.",
  "note.zabbix_no_recipients":
    "Un export ne contient pas les médias des utilisateurs, donc aucun destinataire : connectez-vous à l’API de Zabbix pour les lire, ou ajoutez le canal à la main.",
  "note.zabbix_not_set":
    "Son paramètre {setting} n’a pas d’adresse utilisable dans la source.",
  "note.scrape_as_port":
    "MonitorFlow vérifie que le port {port} répond ; il ne lit pas les métriques de l’exportateur.",
  "note.ping_as_offline":
    "La source envoie un ping à l’hôte ; l’alerte hors ligne de MonitorFlow prévient quand son agent ne répond plus.",
  "note.ping":
    "MonitorFlow n’envoie pas de ping à des adresses ; il surveille les serveurs par leur agent.",
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

  "note.prtg_ping": "PRTG l’interrogeait par ping depuis une sonde ; l’alerte hors ligne de MonitorFlow prévient quand son agent ne répond plus. Les temps de ping ne sont pas mesurés.",
  "note.prtg_from_server": "Vérifié par l’agent du serveur lui-même, pas depuis une sonde : un problème réseau entre les deux n’est pas vu.",
  "note.prtg_certificate": "MonitorFlow vérifie que le port {port} répond ; le certificat et son expiration ne sont pas vérifiés.",
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
  "note.prtg_paused_by_rule": "En pause dans PRTG par un horaire ou une dépendance, que MonitorFlow n’a pas.",
  "note.prtg_paused_sensor": "Un capteur en pause sans contrôle équivalent dans MonitorFlow : mettre tout son serveur en silence cacherait plus que PRTG.",
  "note.prtg_local_time": "Le fichier ne dit pas le fuseau horaire de PRTG : la fenêtre est lue en UTC.",
  "note.prtg_triggers": "MonitorFlow lui envoie chaque incident ; PRTG ne l’utilisait que là où un déclencheur de notification le nommait.",
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
  "origin.time_interval": "Intervalle de temps",
  "origin.schedule": "Planning",
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
  "kind.silence": "Silences",
  "kind.window": "Fenêtres de maintenance",
  "kind.channel": "Canaux de notification",
  "kind.builtin": "Mesuré sur chaque serveur",
  "kind.other": "Autre",
  "item.present": "Déjà là",
  "item.hosts": { one: "{count} serveur", other: "{count} serveurs" },
  "item.needsOut": "laissé de côté avec ce dont il dépend",
  "item.silence": "Du {from} au {to} : {reason}",
  "item.silenceChecks": "Du {from} au {to}, {checks} : {reason}",
  "item.window": "{rule} : {reason}",
  "item.windowChecks": "{rule}, {checks} : {reason}",
  "item.channel": "{kind} vers {target}",
  "channel.email": "E-mail",
  "channel.webhook": "Webhook",
  "count.server": { one: "{count} serveur", other: "{count} serveurs" },
  "count.check": {
    one: "{count} vérification",
    other: "{count} vérifications",
  },
  "count.threshold": { one: "{count} alerte", other: "{count} alertes" },
  "count.silence": { one: "{count} silence", other: "{count} silences" },
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
  "field.username": "Nom d’utilisateur",
  "field.password": "Mot de passe",
  "waiting.title": "En attente de son agent",
  "waiting.note":
    "Ce serveur a été importé. Installez-y l’agent : dès qu’il s’inscrit sous le nom {name}, il reprend les vérifications et alertes ci-dessous.",
  "waiting.make": "Créer une commande d’installation",
  "waiting.making": "Création…",
};
