import { readFileSync, readdirSync } from "node:fs";
import { join, relative } from "node:path";

import { describe, expect, it } from "vitest";

/** Every component source under `dir`, tests left out. */
function sources(dir: string): string[] {
  return readdirSync(dir, { withFileTypes: true }).flatMap((entry) => {
    const path = join(dir, entry.name);
    if (entry.isDirectory()) return sources(path);
    return path.endsWith(".tsx") && !path.endsWith(".test.tsx") ? [path] : [];
  });
}

describe("password fields", () => {
  // Browsers ignore autocomplete="off" on a password field and fill in the
  // saved MonitorFlow sign-in: saved as a device's credentials, it would be
  // sent to that device. Only the sign-in itself may ask for the current one.
  it("never offer the saved sign-in, except where one signs in", () => {
    const root = __dirname;
    const found = sources(root).flatMap((path) => {
      const text = readFileSync(path, "utf8");
      return [...text.matchAll(/<input\b[^]*?\/>/g)]
        .map((match) => match[0])
        .filter((input) => /type="password"/.test(input))
        .map((input) => input.match(/autoComplete="([^"]*)"/)?.[1] ?? "none")
        .filter((value) => value !== "new-password" && value !== "current-password")
        .map((value) => `${relative(root, path)}: ${value}`);
    });
    expect(found).toEqual([]);
  });

  it("are all looked at", () => {
    const all = sources(__dirname).flatMap((path) =>
      [...readFileSync(path, "utf8").matchAll(/<input\b[^]*?\/>/g)].filter((m) => /type="password"/.test(m[0])),
    );
    expect(all.length).toBeGreaterThan(10);
  });
});
