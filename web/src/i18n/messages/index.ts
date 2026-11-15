import type { Message } from "../catalog";

import * as auth from "./auth";
import * as common from "./common";
import * as fleet from "./fleet";
import * as format from "./format";
import * as server from "./server";
import * as shell from "./shell";
import * as ui from "./ui";

// One file per area of the interface, English and French side by side.
// An area added here is typed into `MessageKey`.
export const en = {
  auth: auth.en,
  common: common.en,
  fleet: fleet.en,
  format: format.en,
  server: server.en,
  shell: shell.en,
  ui: ui.en,
};

export const fr: { [A in keyof typeof en]: Record<string, Message> } = {
  auth: auth.fr,
  common: common.fr,
  fleet: fleet.fr,
  format: format.fr,
  server: server.fr,
  shell: shell.fr,
  ui: ui.fr,
};
