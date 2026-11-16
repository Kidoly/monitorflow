import type { Catalog, Translation } from "../catalog";

/** The Backups page. */
export const en = {
  "kind.daily": "Daily",
  "kind.manual": "Manual",
  "kind.update": "Before an update",
  noPermission: "Backups need a global administrator",
  noPermissionNote: "A backup holds every customer's data and the server's keys. Ask a global administrator for a role.",
  noPermissionNoteOne: "A backup holds all the data and the server's keys. Ask a global administrator for a role.",
  "summary.running": "Backing up…",
  "summary.failed": "The last backup failed: {error}",
  "summary.unknownError": "unknown error",
  "summary.none": "No backup yet.",
  "summary.late": "The last backup is more than two days old.",
  "summary.ok": "Backed up.",
  cannot: "Cannot back up: {message}.",
  title: "Backups",
  what: "the backups",
  backingUp: "Backing up…",
  backUpNow: "Back up now",
  disabled:
    "This server takes no backups: they need the updater service of the production compose file (deploy/docker-compose.yml).",
  updaterDown: "The updater is not running, so no backup is taken. On the central server: {command}",
  secret:
    "A backup holds the database and the server's certificate authority and keys. Anyone with the file can read every customer's data and pose as this server: keep downloaded copies somewhere safe.",
  secretOne:
    "A backup holds the database and the server's certificate authority and keys. Anyone with the file can read all the data and pose as this server: keep downloaded copies somewhere safe.",
  "facts.last": "Last backup",
  "facts.notScheduled": "Not scheduled",
  "facts.next": "Next automatic backup",
  "facts.kept": "Kept",
  "facts.keptValue": "the {count} most recent, every day and before each update",
  "restore.title": "Restore",
  "restore.how": "Download a backup, copy it to the central server, and run there, on this one or a fresh install:",
  "restore.file": "<backup file>",
  "restore.after":
    "It replaces the database and the server's keys, then restarts MonitorFlow; agents reconnect by themselves. Data written after the backup is lost. Metrics, logs and mirror contents are not in backups.",
  "empty.title": "No backup yet",
  "empty.note": "The first one is taken a few minutes after the server starts, then every day.",
  "column.taken": "Taken",
  "column.kind": "Kind",
  "column.size": "Size",
  download: "Download",
  downloadFile: "Download {name}",
  cancel: "Cancel",
  inHostDir: "In BACKUP_DIR on the server",
  needsCompose: "Download needs the current compose file",
} as const satisfies Catalog;

export const fr: Translation<typeof en> = {
  "kind.daily": "Quotidienne",
  "kind.manual": "Manuelle",
  "kind.update": "Avant une mise à jour",
  noPermission: "Les sauvegardes relèvent d’un administrateur global",
  noPermissionNote:
    "Une sauvegarde contient les données de chaque client et les clés du serveur. Demandez un rôle à un administrateur global.",
  noPermissionNoteOne:
    "Une sauvegarde contient toutes les données et les clés du serveur. Demandez un rôle à un administrateur global.",
  "summary.running": "Sauvegarde en cours…",
  "summary.failed": "La dernière sauvegarde a échoué : {error}",
  "summary.unknownError": "erreur inconnue",
  "summary.none": "Aucune sauvegarde pour l’instant.",
  "summary.late": "La dernière sauvegarde date de plus de deux jours.",
  "summary.ok": "Sauvegardé.",
  cannot: "Sauvegarde impossible : {message}.",
  title: "Sauvegardes",
  what: "les sauvegardes",
  backingUp: "Sauvegarde en cours…",
  backUpNow: "Sauvegarder maintenant",
  disabled:
    "Ce serveur ne fait pas de sauvegardes : elles nécessitent le service updater du fichier compose de production (deploy/docker-compose.yml).",
  updaterDown: "Le programme de mise à jour ne tourne pas, aucune sauvegarde n’est donc faite. Sur le serveur central : {command}",
  secret:
    "Une sauvegarde contient la base de données ainsi que l’autorité de certification et les clés du serveur. Quiconque a le fichier peut lire les données de chaque client et se faire passer pour ce serveur : conservez les copies téléchargées en lieu sûr.",
  secretOne:
    "Une sauvegarde contient la base de données ainsi que l’autorité de certification et les clés du serveur. Quiconque a le fichier peut lire toutes les données et se faire passer pour ce serveur : conservez les copies téléchargées en lieu sûr.",
  "facts.last": "Dernière sauvegarde",
  "facts.notScheduled": "Non planifiée",
  "facts.next": "Prochaine sauvegarde automatique",
  "facts.kept": "Conservées",
  "facts.keptValue": "les {count} plus récentes, chaque jour et avant chaque mise à jour",
  "restore.title": "Restaurer",
  "restore.how":
    "Téléchargez une sauvegarde, copiez-la sur le serveur central, puis lancez sur place, sur celui-ci ou sur une nouvelle installation :",
  "restore.file": "<fichier de sauvegarde>",
  "restore.after":
    "Elle remplace la base de données et les clés du serveur, puis redémarre MonitorFlow ; les agents se reconnectent d’eux-mêmes. Les données écrites après la sauvegarde sont perdues. Les métriques, les journaux et le contenu des miroirs ne sont pas dans les sauvegardes.",
  "empty.title": "Aucune sauvegarde pour l’instant",
  "empty.note": "La première est faite quelques minutes après le démarrage du serveur, puis chaque jour.",
  "column.taken": "Faite",
  "column.kind": "Type",
  "column.size": "Taille",
  download: "Télécharger",
  downloadFile: "Télécharger {name}",
  cancel: "Annuler",
  inHostDir: "Dans BACKUP_DIR sur le serveur",
  needsCompose: "Le téléchargement nécessite le fichier compose actuel",
};
