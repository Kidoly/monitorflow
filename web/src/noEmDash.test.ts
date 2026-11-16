import { readFileSync, readdirSync } from "node:fs";
import { join, relative } from "node:path";

import { describe, expect, it } from "vitest";

// Built from its code so this file passes its own check.
const EM_DASH = String.fromCharCode(0x2014);

/** Every file under `dir`. */
function files(dir: string): string[] {
  return readdirSync(dir, { withFileTypes: true }).flatMap((entry) => {
    const path = join(dir, entry.name);
    return entry.isDirectory() ? files(path) : [path];
  });
}

describe("text", () => {
  // The owner's rule: in prose, a comma, a colon, parentheses or a period;
  // for an empty value, a word from components/ui/Missing.
  it("has no em dash, in English or French, code or tests", () => {
    const root = __dirname;
    const found = files(root).flatMap((path) =>
      readFileSync(path, "utf8")
        .split("\n")
        .flatMap((line, index) => (line.includes(EM_DASH) ? [`${relative(root, path)}:${index + 1}`] : [])),
    );
    expect(found).toEqual([]);
  });
});
