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
  "kind.channel": "Notification channels",
  "kind.builtin": "Measured on every server",
  "kind.other": "Other",
  "item.present": "Already there",
  "item.hosts": { one: "{count} server", other: "{count} servers" },
  "item.needsOut": "left out with what it needs",
  "item.silence": "From {from} to {to}: {reason}",
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
  "kind.channel": "Canaux de notification",
  "kind.builtin": "Mesuré sur chaque serveur",
  "kind.other": "Autre",
  "item.present": "Déjà là",
  "item.hosts": { one: "{count} serveur", other: "{count} serveurs" },
  "item.needsOut": "laissé de côté avec ce dont il dépend",
  "item.silence": "Du {from} au {to} : {reason}",
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
