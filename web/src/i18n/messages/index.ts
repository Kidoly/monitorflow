import type { Message } from "../catalog";

import * as common from "./common";
import * as format from "./format";
import * as shell from "./shell";
import * as ui from "./ui";

// One file per area of the interface, English and French side by side.
// An area added here is typed into `MessageKey`.
export const en = {
  common: common.en,
  format: format.en,
  shell: shell.en,
  ui: ui.en,
};

export const fr: { [A in keyof typeof en]: Record<string, Message> } = {
  common: common.fr,
  format: format.fr,
  shell: shell.fr,
  ui: ui.fr,
};
