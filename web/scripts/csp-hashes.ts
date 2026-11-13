// Pins the inline scripts of each exported page by hash (ADR-0124). Next
// writes inline scripts into every page: the theme script of the root
// layout, its bootstrap and the page's `self.__next_f.push` payload, which
// differs per page and per build. After `next build`, this adds to the
// <head> of each page a CSP <meta> allowing exactly that page's scripts:
//
//   node scripts/csp-hashes.ts out
//
// The browser enforces it on top of the proxy's header, so an injected
// inline script is blocked even though the header allows inline scripts.
// It fails on what a hash cannot allow (inline event handlers, javascript:
// URLs), so such markup is caught at build time, not blocked in a browser.

import { createHash } from "node:crypto";
import { readdirSync, readFileSync, writeFileSync } from "node:fs";
import { join } from "node:path";

// Next escapes "</script" inside its payloads, so the first closing tag ends
// the element. Scripts with a src attribute are covered by 'self'.
const SCRIPT = /<script\b([^>]*)>([\s\S]*?)<\/script\b[^>]*>/gi;
const SRC = /\ssrc\s*=/i;
// Inline handlers and javascript: URLs need 'unsafe-hashes' or
// 'unsafe-inline'; neither is allowed.
const FORBIDDEN = /<[a-z][^>]*\son[a-z]+\s*=|javascript:/i;
const HEAD = "<head>";
const CHARSET = '<meta charSet="utf-8"/>';
const MARKER = 'http-equiv="Content-Security-Policy"';

/** The CSP source for one inline script body. */
export function hashSource(body: string): string {
  return `'sha256-${createHash("sha256").update(body, "utf8").digest("base64")}'`;
}

/** The distinct hash sources of the inline scripts of one HTML page. */
export function pageHashes(html: string, name = "page"): string[] {
  const forbidden = FORBIDDEN.exec(html);
  if (forbidden) {
    throw new Error(`${name}: CSP cannot allow by hash: ${forbidden[0].slice(0, 80)}`);
  }
  const hashes = new Set<string>();
  for (const [, attrs = "", body = ""] of html.matchAll(SCRIPT)) {
    if (SRC.test(attrs) || body === "") continue;
    hashes.add(hashSource(body));
  }
  return [...hashes];
}

/**
 * The page with a CSP <meta> for its inline scripts at the top of its
 * <head>, after the charset, which must stay in the first bytes. A policy
 * applies only to what follows it, so no script may come before.
 */
export function pinPage(html: string, name = "page"): string {
  if (html.includes(MARKER)) throw new Error(`${name}: already has a CSP <meta>`);
  const head = html.indexOf(HEAD);
  if (head < 0) throw new Error(`${name}: no <head>`);
  let at = head + HEAD.length;
  if (html.startsWith(CHARSET, at)) at += CHARSET.length;
  if (/<script\b/i.test(html.slice(0, at))) throw new Error(`${name}: a script precedes <head>`);
  const sources = ["'self'", ...pageHashes(html, name)].join(" ");
  const meta = `<meta ${MARKER} content="script-src ${sources}"/>`;
  return html.slice(0, at) + meta + html.slice(at);
}

/** Pins every page under `dir`; returns how many. */
export function pinExport(dir: string): number {
  const files = readdirSync(dir, { recursive: true, encoding: "utf8" })
    .filter((path) => path.endsWith(".html"))
    .map((path) => join(dir, path));
  if (files.length === 0) throw new Error(`${dir}: no HTML files; run next build first`);
  for (const file of files) writeFileSync(file, pinPage(readFileSync(file, "utf8"), file));
  return files.length;
}

if (import.meta.main) {
  const [dir] = process.argv.slice(2);
  if (!dir) {
    console.error("usage: node scripts/csp-hashes.ts <export dir>");
    process.exit(2);
  }
  console.log(`CSP <meta> with script hashes added to ${pinExport(dir)} pages`);
}
