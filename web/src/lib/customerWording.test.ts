import { readFileSync, readdirSync } from "node:fs";
import { join, relative } from "node:path";

import { describe, expect, it } from "vitest";

/**
 * Customer wording only for someone who sees several customers (ADR-0104).
 * A file that says "customer" on screen must read the flag (`multiCustomer`,
 * `useMultiCustomer`) or word it through `@/lib/customers`. A cheap check,
 * not a parser: it reads string literals and lines of JSX text.
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
    if (entry.isDirectory()) return entry.name === "test" ? [] : sources(path);
    return /\.tsx?$/.test(entry.name) && !/\.test\.tsx?$/.test(entry.name) ? [path] : [];
  });
}

/** The text a file may show: string literals and JSX text, comments left out. */
export function shownText(source: string): string[] {
  const code = source.replace(/\/\*[\s\S]*?\*\//g, "").replace(/^\s*\/\/.*$/gm, "");
  const literals = code.match(/"(?:[^"\\\n]|\\.)*"|'(?:[^'\\\n]|\\.)*'|`(?:[^`\\]|\\.)*`/g) ?? [];
  const jsxText = code
    .split("\n")
    .map((line) => line.trim())
    .filter((line) => line !== "" && !/[=;(){}<>:"'`[\],]/.test(line));
  // A word on screen has a space or a capital; "customer" alone is a value.
  return [...literals.map((l) => l.slice(1, -1)), ...jsxText].filter(
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
