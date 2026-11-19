import { readFileSync, readdirSync, statSync } from "node:fs";
import { join, relative } from "node:path";

import { describe, expect, it } from "vitest";

import { STATUS_TONES } from "@/components/ui/StatusLabel";
import { CATALOGUE } from "@/lib/catalogue";
import { KINDS as IMPORT_KINDS } from "@/lib/imports";
import { EVENT_GROUPS } from "@/lib/integrations";
import { SERVER_STATES } from "@/lib/serverState";

import type { Message } from "./catalog";
import { setActiveLocale } from "./locale";
import { en, fr } from "./messages";

type Catalogs = Record<string, Record<string, Message>>;

const flat = (catalogs: Catalogs) =>
  Object.entries(catalogs).flatMap(([area, messages]) =>
    Object.entries(messages).map(([name, message]) => [`${area}.${name}` as string, message] as const),
  );

const english = new Map(flat(en));
const french = new Map(flat(fr));

const texts = (message: Message) => (typeof message === "string" ? [message] : Object.values(message));
const placeholders = (message: Message) =>
  [...new Set(texts(message).flatMap((text) => [...text.matchAll(/\{(\w+)\}/g)].map((m) => m[1])))].sort();

describe("catalogs", () => {
  it("translate every English message into French, and nothing else", () => {
    expect([...french.keys()].sort()).toEqual([...english.keys()].sort());
    for (const [key, message] of french) {
      for (const text of texts(message)) expect(text.trim(), key).not.toBe("");
    }
  });

  it("keep the placeholders and plural forms of the English message", () => {
    for (const [key, message] of english) {
      const translation = french.get(key)!;
      expect(typeof translation, key).toBe(typeof message);
      expect(placeholders(translation), key).toEqual(placeholders(message));
      if (typeof message !== "string") {
        expect(Object.keys(message).sort(), key).toEqual(["one", "other"]);
      }
    }
  });

  it("follow French typography", () => {
    for (const [key, message] of french) {
      for (const text of texts(message)) {
        // A no-break space before : ; ? ! (but not in 14:30 or https://), « guillemets », a curly apostrophe.
        expect(text, key).not.toMatch(/ [:;?!»]|« /);
        expect(text, key).not.toMatch(/[\p{L}\p{N})}][;?!»](?=\s|$)|[\p{L})}]:(?=\s|$)|«\S/u);
        expect(text, key).not.toMatch(/["']/);
      }
    }
  });
});

// Keys built at run time from a list escape the type check when cast, and
// the source test below sees only their fixed part: one missing showed as
// the raw `common.state.waiting` in the fleet.
describe("keys built from a list", () => {
  it("are in both catalogs", () => {
    const keys = [
      ...SERVER_STATES.flatMap((state) => [`common.state.${state}`, `fleet.tree.pill.${state}`]),
      ...Object.entries(STATUS_TONES).flatMap(([kind, values]) => Object.keys(values).map((value) => `ui.status.${kind}.${value}`)),
      ...EVENT_GROUPS.flatMap((group) => [`integrations.group.${group.group}`, ...group.events.map((e) => `integrations.event.${e.event}`)]),
      ...IMPORT_KINDS.flatMap((kind) => [`imports.kind.${kind}`, `imports.count.${kind}`]),
    ];
    expect(keys.filter((key) => !english.has(key) || !french.has(key))).toEqual([]);
  });

  it("give every check of the catalogue its words", () => {
    for (const locale of ["en", "fr"] as const) {
      setActiveLocale(locale);
      const shown = CATALOGUE.flatMap((entry) => [entry.title, entry.description, JSON.stringify(entry.type)]);
      expect(shown.filter((text) => text.includes("checks.entry.")), locale).toEqual([]);
    }
    setActiveLocale("en");
  });
});

/** Every `.ts` and `.tsx` file under `dir`, tests and catalogs left out. */
function sources(dir: string): string[] {
  return readdirSync(dir).flatMap((name) => {
    const path = join(dir, name);
    if (statSync(path).isDirectory()) return name === "messages" ? [] : sources(path);
    return /\.tsx?$/.test(name) && !/\.test\.tsx?$/.test(name) ? [path] : [];
  });
}

describe("source", () => {
  const root = join(__dirname, "..");

  it("names only messages of the English catalog", () => {
    const missing: string[] = [];
    for (const file of sources(root)) {
      const text = readFileSync(file, "utf8");
      for (const match of text.matchAll(/\b(?:t|rich)\(\s*"([^"]+)"/g)) {
        if (!english.has(match[1]!)) missing.push(`${relative(root, file)}: ${match[1]}`);
      }
      // A dynamic key names its fixed part: `common.state.${state}` needs a common.state.* message.
      for (const match of text.matchAll(/\b(?:t|rich)\(\s*`([^`$]+)\$\{/g)) {
        if (![...english.keys()].some((key) => key.startsWith(match[1]!))) {
          missing.push(`${relative(root, file)}: ${match[1]}…`);
        }
      }
    }
    expect(missing).toEqual([]);
  });
});
