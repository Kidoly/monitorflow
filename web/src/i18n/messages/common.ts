import type { Catalog, Translation } from "../catalog";

/** Words many pages share: error sentences, roles, statuses. */
export const en = {
  "error.unknown": "Something went wrong.",
  "error.unreachable": "Cannot reach the MonitorFlow server.",
  "error.signedOut": "Your session has ended. Sign in again.",
  "error.forbidden": "You do not have access to this. Ask an administrator for a role.",
  "error.server": "The server could not handle the request. Try again later.",
  "error.failed": "The request failed ({message}).",
  "role.viewer": "viewer",
  "role.operator": "operator",
  "role.approver": "approver",
  "role.administrator": "administrator",
  "role.none": "no role",
  "state.critical": "Critical",
  "state.warning": "Warning",
  "state.offline": "Offline",
  "state.acknowledged": "Acknowledged",
  "state.silenced": "Silenced",
  "state.ok": "OK",
  loading: "Loading…",
} as const satisfies Catalog;

export const fr: Translation<typeof en> = {
  "error.unknown": "Une erreur s’est produite.",
  "error.unreachable": "Impossible de joindre le serveur MonitorFlow.",
  "error.signedOut": "Votre session a pris fin. Reconnectez-vous.",
  "error.forbidden": "Vous n’avez pas accès à ceci. Demandez un rôle à un administrateur.",
  "error.server": "Le serveur n’a pas pu traiter la demande. Réessayez plus tard.",
  "error.failed": "La demande a échoué ({message}).",
  "role.viewer": "lecteur",
  "role.operator": "opérateur",
  "role.approver": "approbateur",
  "role.administrator": "administrateur",
  "role.none": "aucun rôle",
  "state.critical": "Critique",
  "state.warning": "Avertissement",
  "state.offline": "Hors ligne",
  "state.acknowledged": "Pris en charge",
  "state.silenced": "En silence",
  "state.ok": "OK",
  loading: "Chargement…",
};
