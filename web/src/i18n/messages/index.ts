import type { Message } from "../catalog";

import * as auth from "./auth";
import * as checks from "./checks";
import * as common from "./common";
import * as fleet from "./fleet";
import * as format from "./format";
import * as incidents from "./incidents";
import * as notifications from "./notifications";
import * as sensors from "./sensors";
import * as server from "./server";
import * as shell from "./shell";
import * as silences from "./silences";
import * as ui from "./ui";

// One file per area of the interface, English and French side by side.
// An area added here is typed into `MessageKey`.
export const en = {
  auth: auth.en,
  checks: checks.en,
  common: common.en,
  fleet: fleet.en,
  format: format.en,
  incidents: incidents.en,
  notifications: notifications.en,
  sensors: sensors.en,
  server: server.en,
  shell: shell.en,
  silences: silences.en,
  ui: ui.en,
};

export const fr: { [A in keyof typeof en]: Record<string, Message> } = {
  auth: auth.fr,
  checks: checks.fr,
  common: common.fr,
  fleet: fleet.fr,
  format: format.fr,
  incidents: incidents.fr,
  notifications: notifications.fr,
  sensors: sensors.fr,
  server: server.fr,
  shell: shell.fr,
  silences: silences.fr,
  ui: ui.fr,
};
