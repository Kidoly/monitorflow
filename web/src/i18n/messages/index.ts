import type { Message } from "../catalog";

import * as ai from "./ai";
import * as apiKeys from "./apiKeys";
import * as auth from "./auth";
import * as authority from "./authority";
import * as autoUpdate from "./autoUpdate";
import * as backups from "./backups";
import * as campaigns from "./campaigns";
import * as checks from "./checks";
import * as common from "./common";
import * as fleet from "./fleet";
import * as followUp from "./followUp";
import * as format from "./format";
import * as incidents from "./incidents";
import * as install from "./install";
import * as integrations from "./integrations";
import * as license from "./license";
import * as mirrors from "./mirrors";
import * as monitoring from "./monitoring";
import * as notifications from "./notifications";
import * as onCall from "./onCall";
import * as relays from "./relays";
import * as rollback from "./rollback";
import * as sensors from "./sensors";
import * as server from "./server";
import * as serverUpdate from "./serverUpdate";
import * as settings from "./settings";
import * as shell from "./shell";
import * as silences from "./silences";
import * as sites from "./sites";
import * as ui from "./ui";
import * as users from "./users";

// One file per area of the interface, English and French side by side.
// An area added here is typed into `MessageKey`.
export const en = {
  ai: ai.en,
  apiKeys: apiKeys.en,
  auth: auth.en,
  authority: authority.en,
  autoUpdate: autoUpdate.en,
  backups: backups.en,
  campaigns: campaigns.en,
  checks: checks.en,
  common: common.en,
  fleet: fleet.en,
  followUp: followUp.en,
  format: format.en,
  incidents: incidents.en,
  install: install.en,
  integrations: integrations.en,
  license: license.en,
  mirrors: mirrors.en,
  monitoring: monitoring.en,
  notifications: notifications.en,
  onCall: onCall.en,
  relays: relays.en,
  rollback: rollback.en,
  sensors: sensors.en,
  server: server.en,
  serverUpdate: serverUpdate.en,
  settings: settings.en,
  shell: shell.en,
  silences: silences.en,
  sites: sites.en,
  ui: ui.en,
  users: users.en,
};

export const fr: { [A in keyof typeof en]: Record<string, Message> } = {
  ai: ai.fr,
  apiKeys: apiKeys.fr,
  auth: auth.fr,
  authority: authority.fr,
  autoUpdate: autoUpdate.fr,
  backups: backups.fr,
  campaigns: campaigns.fr,
  checks: checks.fr,
  common: common.fr,
  fleet: fleet.fr,
  followUp: followUp.fr,
  format: format.fr,
  incidents: incidents.fr,
  install: install.fr,
  integrations: integrations.fr,
  license: license.fr,
  mirrors: mirrors.fr,
  monitoring: monitoring.fr,
  notifications: notifications.fr,
  onCall: onCall.fr,
  relays: relays.fr,
  rollback: rollback.fr,
  sensors: sensors.fr,
  server: server.fr,
  serverUpdate: serverUpdate.fr,
  settings: settings.fr,
  shell: shell.fr,
  silences: silences.fr,
  sites: sites.fr,
  ui: ui.fr,
  users: users.fr,
};
