import type { Message } from "../catalog";

import * as common from "./common";
import * as shell from "./shell";

// One file per area of the interface, English and French side by side.
// An area added here is typed into `MessageKey`.
export const en = {
  common: common.en,
  shell: shell.en,
};

export const fr: { [A in keyof typeof en]: Record<string, Message> } = {
  common: common.fr,
  shell: shell.fr,
};
