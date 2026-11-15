import { readFileSync, readdirSync } from "node:fs";
import { join, relative } from "node:path";

import { describe, expect, it } from "vitest";

import type { Message } from "@/i18n/catalog";
import { en } from "@/i18n/messages";

/**
 * Customer wording only for someone who sees several customers (ADR-0104).
 * A file that says "customer" on screen must read the flag (`multiCustomer`,
 * `useMultiCustomer`) or word it through `@/lib/customers`. A cheap check,
 * not a parser: it reads string literals, lines of JSX text and the English
 * messages the file names (ADR-0136).
 */

const ROOT = join(__dirname, "..");

/** Files that may say "customer" whatever the flag, and why. */
const ALLOWED: Record<string, string> = {
  "lib/api.ts": "API types and their doc comments; no text on screen",
  "lib/scope.ts": "the URL parameter `?customer=`",
  "lib/campaigns.ts": "\"Choose a customer.\" needs the customer picker; with one customer it is chosen already",
};

/** What reads the flag, or words through its helpers. */
const GUARD = /\b(multiCustomer|useMultiCustomer|placeLabel|customerWide)\b/;
const WORD = /\b[Cc]ustomers?\b/;

function sources(dir: string): string[] {
  return readdirSync(dir, { withFileTypes: true }).flatMap((entry) => {
    const path = join(dir, entry.name);
    // The catalogs count where their messages are used.
    if (entry.isDirectory()) return entry.name === "test" || entry.name === "messages" ? [] : sources(path);
    return /\.tsx?$/.test(entry.name) && !/\.test\.tsx?$/.test(entry.name) ? [path] : [];
  });
}

const MESSAGES = Object.entries(en as Record<string, Record<string, Message>>).flatMap(([area, messages]) =>
  Object.entries(messages).map(([name, m]) => [`${area}.${name}`, typeof m === "string" ? [m] : Object.values(m)] as const),
);

/** The English text of the messages `code` names, a dynamic key naming all under its fixed part. */
function messageText(code: string): string[] {
  const keys = [...code.matchAll(/\b(?:t|rich)\(\s*(?:"([^"]+)"|`([^`$]+)\$\{)/g)];
  return keys.flatMap(([, exact, prefix]) =>
    MESSAGES.filter(([key]) => (exact ? key === exact : key.startsWith(prefix!))).flatMap(([, texts]) => texts),
  );
}

/** The text a file may show: string literals, JSX text and named messages, comments left out. */
export function shownText(source: string): string[] {
  const code = source.replace(/\/\*[\s\S]*?\*\//g, "").replace(/^\s*\/\/.*$/gm, "");
  const literals = code.match(/"(?:[^"\\\n]|\\.)*"|'(?:[^'\\\n]|\\.)*'|`(?:[^`\\]|\\.)*`/g) ?? [];
  const jsxText = code
    .split("\n")
    .map((line) => line.trim())
    .filter((line) => line !== "" && !/[=;(){}<>:"'`[\],]/.test(line));
  // A word on screen has a space or a capital; "customer" alone is a value.
  return [...literals.map((l) => l.slice(1, -1)), ...jsxText, ...messageText(code)].filter(
    (text) => WORD.test(text) && (/\s/.test(text) || /\bCustomers?\b/.test(text)),
  );
}

describe("customer wording (ADR-0104)", () => {
  it("finds wording, not values", () => {
    expect(shownText('label: `Customer ${c.name}`, value: "customer", id: `customer/${id}`')).toEqual([
      "Customer ${c.name}",
    ]);
    expect(shownText("  <label>\n    Customer name\n  </label>\n  // the customer id\n")).toEqual(["Customer name"]);
    expect(shownText('const x = "All customers";')).toEqual(["All customers"]);
    expect(shownText('const x = t("shell.scope.allCustomers");')).toEqual(["All customers"]);
  });

  it("is shown only where the flag is read", () => {
    const unguarded = sources(ROOT)
      .map((path) => relative(ROOT, path))
      .filter((path) => !(path in ALLOWED))
      .filter((path) => {
        const source = readFileSync(join(ROOT, path), "utf8");
        return shownText(source).length > 0 && !GUARD.test(source);
      });
    expect(unguarded, "say \"customer\" only with multiCustomer (docs/adr/0104-customers-out-of-sight.md)").toEqual([]);
  });
});
