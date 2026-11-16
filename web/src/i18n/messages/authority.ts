import type { Catalog, Translation } from "../catalog";

/** The certificate authority page and its notice on every page. */
export const en = {
  "machine.server": "Server",
  "machine.relay": "Relay",
  "status.emergency": "Emergency change under way",
  "status.step": "Change under way: step {step} of 2",
  "status.restored": "A retired certificate authority is back",
  "status.expires": "Expires on {date}",
  "status.valid": "Valid until {date}",
  "expiry.auto":
    "The certificate authority expires on {expires}. Change it now: one click, nothing to do on each server. If nobody does, the change starts by itself on {auto}.",
  "expiry.soon":
    "The certificate authority expires on {expires}. Change it now: one click, nothing to do on each server. The change starts by itself within the next minutes.",
  "explain.emergencyOn":
    "Every server and relay gets a certificate from the new certificate authority within the hour. The old one stops being trusted on {day}, or as soon as every machine moved. Machines that miss it must be installed again.",
  "explain.emergency":
    "Every server and relay gets a certificate from the new certificate authority within the hour. The old one stops being trusted 24 hours after the change started, or as soon as every machine moved. Machines that miss it must be installed again.",
  "explain.introduced":
    "Step 1 of 2: every server and relay learns the new certificate authority over its secure connection, within about an hour. The next step starts by itself once every machine has.",
  "explain.switched":
    "Step 2 of 2: the new certificate authority gives every server and relay a new certificate, within about an hour. The old one is retired by itself once every machine moved. Install commands made before the switch no longer work: copy a new one from the Add servers page.",
  machines: { one: "server or relay", other: "servers and relays" },
  "progress.none": "No server or relay to wait for.",
  "progress.allLearned": {
    one: "All {count} server or relay learned the new certificate authority.",
    other: "All {count} servers and relays learned the new certificate authority.",
  },
  "progress.allMoved": {
    one: "All {count} server or relay moved to the new certificate authority.",
    other: "All {count} servers and relays moved to the new certificate authority.",
  },
  "progress.learnedLeft": "{done} of {total} {machines} learned the new certificate authority; {left} still to go.",
  "progress.movedLeft": "{done} of {total} {machines} moved to the new certificate authority; {left} still to go.",
  "progress.learnedOffline": {
    one: "{done} of {total} {machines} learned the new certificate authority; {count} has not connected since {when}.",
    other: "{done} of {total} {machines} learned the new certificate authority; {count} have not connected since {when}.",
  },
  "progress.movedOffline": {
    one: "{done} of {total} {machines} moved to the new certificate authority; {count} has not connected since {when}.",
    other: "{done} of {total} {machines} moved to the new certificate authority; {count} have not connected since {when}.",
  },
  "progress.sinceStart": "the change started",
  "progress.sinceSwitch": "the switch",
  "next.ready": "Every server and relay is ready.",
  "next.cutOff": {
    one: "The {count} server or relay not done yet will not be able to connect: each must be installed again.",
    other: "The {count} servers and relays not done yet will not be able to connect: each must be installed again.",
  },
  "next.switchLabel": "Switch now…",
  "next.switchTitle": "Switch to the new certificate authority now?",
  "next.switchConsequence": "It gives every machine a new certificate without waiting. {them}",
  "next.switchConfirm": "Switch now",
  "next.distrustLabel": "Stop trusting the old authority now…",
  "next.distrustTitle": "Stop trusting the old certificate authority now?",
  "next.distrustConsequence": "It retires the old certificate authority without waiting. {them}",
  "next.distrustConfirm": "Stop trusting it now",
  reinstall:
    "Install it again with a single-use token for its name, under Show single-use tokens on the Add servers page, to keep its history. An install command would add it as a new machine.",
  waitedUntil: "Stops being waited for on {date}",
  "notice.emergency": "An emergency change of the certificate authority is under way.",
  "notice.restored":
    "A restore brought back a certificate authority this server had retired: servers and relays that moved cannot connect.",
  "notice.cutOff": {
    one: "{count} server or relay needs a new install after a change of certificate authority.",
    other: "{count} servers and relays need a new install after a change of certificate authority.",
  },
  "notice.expires": "The certificate authority expires on {date}.",
  noPermission: "The certificate authority needs a global administrator",
  noPermissionNote: "It gives every server and relay its certificate. Ask a global administrator for a role.",
  noPermissionNoteCustomers:
    "It gives every server and relay of every customer its certificate. Ask a global administrator for a role.",
  failure: "{what}: {message}.",
  title: "Certificate authority",
  meta: "Global administrators only",
  what: "the certificate authority",
  restoredBanner:
    "A restore brought back a certificate authority this server retired on {date}. Servers and relays that moved to the newer one cannot connect until they are installed again. Restore a backup taken after that date if there is one, or start a change now.",
  about: "The certificate authority of this server gives every server and relay the certificate it uses to connect securely.",
  "facts.created": "Created",
  "facts.expires": "Expires",
  "facts.fingerprint": "Fingerprint (SHA-256)",
  "facts.new": "New certificate authority",
  "facts.old": "Old certificate authority",
  "start.title": "Change the certificate authority",
  "start.note":
    "A planned change takes two steps. First every server and relay learns the new certificate authority over its secure connection, within about an hour. Then the new one gives every machine a new certificate, within about an hour, and the old one is retired. Each step goes on by itself once every machine is done; a machine that is offline is waited for until its certificate expires, at most 7 days. The whole change takes from a couple of hours to about two weeks, with nothing to do on each machine. Install commands made before the switch stop working after it: copy a new one from the Add servers page.",
  "start.trigger": "Change the certificate authority…",
  "start.confirmTitle": "Change the certificate authority?",
  "start.consequence": "Servers and relays follow by themselves; nothing to do on each machine.",
  "start.confirm": "Start the change",
  starting: "Starting…",
  "start.failed": "Cannot start the change",
  "emergency.trigger": "Emergency change…",
  "emergency.title": "Start an emergency change?",
  "emergency.consequence":
    "Only if the key of the certificate authority may have leaked. The new one gives every server and relay a new certificate within the hour, and the old one stops being trusted 24 hours later, or as soon as every machine moved. Machines offline until then cannot connect: each must be installed again. Every install command made before stops working.",
  "emergency.acknowledge": "I understand that machines that miss it must be installed again.",
  "emergency.confirm": "Start the emergency change",
  "change.emergencyTitle": "Emergency change of the certificate authority",
  "change.title": "Change of the certificate authority",
  "change.progress": "Progress",
  "change.started": "Started",
  "change.switched": "Switched",
  "change.distrust": "Old one stops being trusted",
  "change.working": "Working…",
  "change.failed": "Cannot take the next step",
  never: "never",
  "column.name": "Name",
  "column.kind": "Kind",
  "column.seen": "Last contact",
  "column.until": "Stops being waited for",
  tooOld: "Too old to follow: updates itself",
  waitedFor: "Waited for",
  needsInstall: "Needs a new install",
  cutOffNote:
    "These servers and relays missed an emergency change: their certificate comes from a certificate authority this server no longer trusts, so they cannot connect. {hint} {link}",
  addServers: "Add servers",
} as const satisfies Catalog;

export const fr: Translation<typeof en> = {
  "machine.server": "Serveur",
  "machine.relay": "Relais",
  "status.emergency": "Changement d’urgence en cours",
  "status.step": "Changement en cours : étape {step} sur 2",
  "status.restored": "Une autorité de certification retirée est revenue",
  "status.expires": "Expire le {date}",
  "status.valid": "Valide jusqu’au {date}",
  "expiry.auto":
    "L’autorité de certification expire le {expires}. Changez-la maintenant : un clic, rien à faire sur chaque serveur. Sinon, le changement démarre de lui-même le {auto}.",
  "expiry.soon":
    "L’autorité de certification expire le {expires}. Changez-la maintenant : un clic, rien à faire sur chaque serveur. Le changement démarre de lui-même dans les prochaines minutes.",
  "explain.emergencyOn":
    "Chaque serveur et relais reçoit un certificat de la nouvelle autorité de certification dans l’heure. L’ancienne n’est plus approuvée à partir du {day}, ou dès que chaque machine a basculé. Les machines qui le manquent doivent être réinstallées.",
  "explain.emergency":
    "Chaque serveur et relais reçoit un certificat de la nouvelle autorité de certification dans l’heure. L’ancienne n’est plus approuvée 24 heures après le début du changement, ou dès que chaque machine a basculé. Les machines qui le manquent doivent être réinstallées.",
  "explain.introduced":
    "Étape 1 sur 2 : chaque serveur et relais apprend la nouvelle autorité de certification par sa connexion sécurisée, en une heure environ. L’étape suivante démarre d’elle-même une fois que toutes les machines l’ont apprise.",
  "explain.switched":
    "Étape 2 sur 2 : la nouvelle autorité de certification donne à chaque serveur et relais un nouveau certificat, en une heure environ. L’ancienne est retirée d’elle-même une fois que chaque machine a basculé. Les commandes d’installation créées avant la bascule ne fonctionnent plus : copiez-en une nouvelle depuis la page Ajouter des serveurs.",
  machines: { one: "serveur ou relais", other: "serveurs et relais" },
  "progress.none": "Aucun serveur ni relais à attendre.",
  "progress.allLearned": {
    one: "Le serveur ou relais ({count}) a appris la nouvelle autorité de certification.",
    other: "Les {count} serveurs et relais ont appris la nouvelle autorité de certification.",
  },
  "progress.allMoved": {
    one: "Le serveur ou relais ({count}) a basculé vers la nouvelle autorité de certification.",
    other: "Les {count} serveurs et relais ont basculé vers la nouvelle autorité de certification.",
  },
  "progress.learnedLeft": "{done} {machines} sur {total} ont appris la nouvelle autorité de certification ; il en reste {left}.",
  "progress.movedLeft":
    "{done} {machines} sur {total} ont basculé vers la nouvelle autorité de certification ; il en reste {left}.",
  "progress.learnedOffline": {
    one: "{done} {machines} sur {total} ont appris la nouvelle autorité de certification ; {count} ne s’est pas connecté depuis {when}.",
    other:
      "{done} {machines} sur {total} ont appris la nouvelle autorité de certification ; {count} ne se sont pas connectés depuis {when}.",
  },
  "progress.movedOffline": {
    one: "{done} {machines} sur {total} ont basculé vers la nouvelle autorité de certification ; {count} ne s’est pas connecté depuis {when}.",
    other:
      "{done} {machines} sur {total} ont basculé vers la nouvelle autorité de certification ; {count} ne se sont pas connectés depuis {when}.",
  },
  "progress.sinceStart": "le début du changement",
  "progress.sinceSwitch": "la bascule",
  "next.ready": "Chaque serveur et relais est prêt.",
  "next.cutOff": {
    one: "Le serveur ou relais pas encore prêt ({count}) ne pourra pas se connecter : il devra être réinstallé.",
    other: "Les {count} serveurs et relais pas encore prêts ne pourront pas se connecter : chacun devra être réinstallé.",
  },
  "next.switchLabel": "Basculer maintenant…",
  "next.switchTitle": "Basculer vers la nouvelle autorité de certification maintenant ?",
  "next.switchConsequence": "Chaque machine reçoit un nouveau certificat sans attendre. {them}",
  "next.switchConfirm": "Basculer maintenant",
  "next.distrustLabel": "Ne plus approuver l’ancienne autorité maintenant…",
  "next.distrustTitle": "Ne plus approuver l’ancienne autorité de certification maintenant ?",
  "next.distrustConsequence": "L’ancienne autorité de certification est retirée sans attendre. {them}",
  "next.distrustConfirm": "Ne plus l’approuver maintenant",
  reinstall:
    "Réinstallez-la avec un jeton à usage unique à son nom, sous Afficher les jetons à usage unique sur la page Ajouter des serveurs, pour garder son historique. Une commande d’installation l’ajouterait comme une nouvelle machine.",
  waitedUntil: "N’est plus attendu à partir du {date}",
  "notice.emergency": "Un changement d’urgence de l’autorité de certification est en cours.",
  "notice.restored":
    "Une restauration a ramené une autorité de certification que ce serveur avait retirée : les serveurs et relais qui ont basculé ne peuvent pas se connecter.",
  "notice.cutOff": {
    one: "{count} serveur ou relais est à réinstaller après un changement d’autorité de certification.",
    other: "{count} serveurs et relais sont à réinstaller après un changement d’autorité de certification.",
  },
  "notice.expires": "L’autorité de certification expire le {date}.",
  noPermission: "L’autorité de certification relève d’un administrateur global",
  noPermissionNote: "Elle donne à chaque serveur et relais son certificat. Demandez un rôle à un administrateur global.",
  noPermissionNoteCustomers:
    "Elle donne à chaque serveur et relais de chaque client son certificat. Demandez un rôle à un administrateur global.",
  failure: "{what} : {message}.",
  title: "Autorité de certification",
  meta: "Administrateurs globaux uniquement",
  what: "l’autorité de certification",
  restoredBanner:
    "Une restauration a ramené une autorité de certification que ce serveur a retirée le {date}. Les serveurs et relais qui ont basculé vers la plus récente ne peuvent pas se connecter avant d’être réinstallés. Restaurez une sauvegarde faite après cette date s’il y en a une, ou lancez un changement maintenant.",
  about:
    "L’autorité de certification de ce serveur donne à chaque serveur et relais le certificat qu’il utilise pour se connecter de façon sécurisée.",
  "facts.created": "Créée le",
  "facts.expires": "Expire le",
  "facts.fingerprint": "Empreinte (SHA-256)",
  "facts.new": "Nouvelle autorité de certification",
  "facts.old": "Ancienne autorité de certification",
  "start.title": "Changer l’autorité de certification",
  "start.note":
    "Un changement planifié se fait en deux étapes. D’abord, chaque serveur et relais apprend la nouvelle autorité de certification par sa connexion sécurisée, en une heure environ. Puis la nouvelle donne à chaque machine un nouveau certificat, en une heure environ, et l’ancienne est retirée. Chaque étape se poursuit d’elle-même une fois toutes les machines prêtes ; une machine hors ligne est attendue jusqu’à l’expiration de son certificat, 7 jours au plus. Le changement complet prend de quelques heures à deux semaines environ, sans rien à faire sur chaque machine. Les commandes d’installation créées avant la bascule cessent de fonctionner ensuite : copiez-en une nouvelle depuis la page Ajouter des serveurs.",
  "start.trigger": "Changer l’autorité de certification…",
  "start.confirmTitle": "Changer l’autorité de certification ?",
  "start.consequence": "Les serveurs et relais suivent d’eux-mêmes ; rien à faire sur chaque machine.",
  "start.confirm": "Lancer le changement",
  starting: "Lancement…",
  "start.failed": "Impossible de lancer le changement",
  "emergency.trigger": "Changement d’urgence…",
  "emergency.title": "Lancer un changement d’urgence ?",
  "emergency.consequence":
    "Uniquement si la clé de l’autorité de certification a pu fuiter. La nouvelle donne à chaque serveur et relais un nouveau certificat dans l’heure, et l’ancienne n’est plus approuvée 24 heures plus tard, ou dès que chaque machine a basculé. Les machines hors ligne d’ici là ne peuvent plus se connecter : chacune devra être réinstallée. Toute commande d’installation créée avant cesse de fonctionner.",
  "emergency.acknowledge": "Je comprends que les machines qui le manquent devront être réinstallées.",
  "emergency.confirm": "Lancer le changement d’urgence",
  "change.emergencyTitle": "Changement d’urgence de l’autorité de certification",
  "change.title": "Changement de l’autorité de certification",
  "change.progress": "Progression",
  "change.started": "Commencé",
  "change.switched": "Bascule",
  "change.distrust": "L’ancienne n’est plus approuvée",
  "change.working": "En cours…",
  "change.failed": "Impossible de passer à l’étape suivante",
  never: "jamais",
  "column.name": "Nom",
  "column.kind": "Type",
  "column.seen": "Dernier contact",
  "column.until": "N’est plus attendu le",
  tooOld: "Trop ancien pour suivre : se met à jour",
  waitedFor: "Attendus",
  needsInstall: "À réinstaller",
  cutOffNote:
    "Ces serveurs et relais ont manqué un changement d’urgence : leur certificat vient d’une autorité de certification que ce serveur n’approuve plus, ils ne peuvent donc pas se connecter. {hint} {link}",
  addServers: "Ajouter des serveurs",
};
