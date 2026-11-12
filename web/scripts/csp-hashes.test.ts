import { mkdtempSync, mkdirSync, readFileSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";

import { hashSource, pageHashes, pinExport, pinPage } from "./csp-hashes";

const page = (body: string) =>
  `<!DOCTYPE html><html lang="en"><head><meta charSet="utf-8"/>${body}</head><body></body></html>`;

describe("hashSource", () => {
  it("matches the CSP spec's example hash", () => {
    // The body of <script>alert('Hello, world.');</script>, CSP Level 3 §2.3.
    expect(hashSource("alert('Hello, world.');")).toBe(
      "'sha256-qznLcsROx4GACP2dm0UCKCzCG+HiZ1guq6ZZDob/Tng='",
    );
  });
});

describe("pageHashes", () => {
  it("hashes inline scripts once and skips external and empty ones", () => {
    const html =
      '<head><script>a()</script><script src="/_next/x.js" async></script>' +
      '<script async=""></script></head><body><script id="d">b()</script><script>a()</script></body>';
    expect(pageHashes(html)).toEqual([hashSource("a()"), hashSource("b()")]);
  });

  it("hashes the exact body, whitespace included", () => {
    expect(pageHashes("<script>\n a() \n</script>")).toEqual([hashSource("\n a() \n")]);
  });

  it("refuses inline event handlers", () => {
    expect(() => pageHashes('<img src="x" onerror="a()">', "p.html")).toThrow(/p\.html/);
  });

  it("refuses javascript: URLs", () => {
    expect(() => pageHashes('<a href="javascript:a()">x</a>')).toThrow(/javascript:/);
  });

  it("does not take data attributes or text for handlers", () => {
    expect(pageHashes('<div data-onboarding="1">click onclick= to go</div>')).toEqual([]);
  });
});

describe("pinPage", () => {
  it("puts the page's policy right after the charset", () => {
    const pinned = pinPage(page("<script>t()</script>"));
    expect(pinned).toBe(
      page(
        `<meta http-equiv="Content-Security-Policy" content="script-src 'self' ${hashSource("t()")}"/>` +
          "<script>t()</script>",
      ),
    );
  });

  it("allows only 'self' on a page without inline scripts", () => {
    expect(pinPage(page(""))).toContain(`content="script-src 'self'"`);
  });

  it("refuses a page pinned already", () => {
    expect(() => pinPage(pinPage(page("")), "p.html")).toThrow(/already/);
  });

  it("refuses a page without a head or with a script before it", () => {
    expect(() => pinPage("<html><body></body></html>")).toThrow(/no <head>/);
    expect(() => pinPage("<script>x()</script><head></head>")).toThrow(/precedes/);
  });
});

describe("pinExport", () => {
  it("pins every page, nested ones too, each with its own scripts", () => {
    const dir = mkdtempSync(join(tmpdir(), "csp-"));
    mkdirSync(join(dir, "hosts"));
    writeFileSync(join(dir, "index.html"), page("<script>p1()</script>"));
    writeFileSync(join(dir, "hosts", "index.html"), page("<script>p2()</script>"));
    writeFileSync(join(dir, "app.js"), "<head><script>ignored()</script>");
    expect(pinExport(dir)).toBe(2);
    const root = readFileSync(join(dir, "index.html"), "utf8");
    const hosts = readFileSync(join(dir, "hosts", "index.html"), "utf8");
    expect(root).toContain(hashSource("p1()"));
    expect(root).not.toContain(hashSource("p2()"));
    expect(hosts).toContain(hashSource("p2()"));
    expect(readFileSync(join(dir, "app.js"), "utf8")).not.toContain("Content-Security-Policy");
  });

  it("fails on a directory without pages", () => {
    const dir = mkdtempSync(join(tmpdir(), "csp-"));
    expect(() => pinExport(dir)).toThrow(/no HTML files/);
  });
});
