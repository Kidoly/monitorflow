// The browser half of the report test (ADR-0155): after run-e2e.sh, opens
// the web interface in headless Chromium and checks what people see on
// Reports: the summary first, each customer's outages and service, the
// files to download, business services and scheduled reports, a
// customer's administrator seeing only its own, and French. Saves
// screenshots in OUT.
//
//   node browser.mjs BASE_URL ENV_FILE OUT
//
// ENV_FILE is the `env` run-e2e.sh writes. Needs playwright-core
// (NODE_PATH) and a Chromium (CHROMIUM, by default /usr/bin/chromium).
import { mkdirSync, readFileSync } from "node:fs";
import { createRequire } from "node:module";

const { chromium } = createRequire(import.meta.url)("playwright-core");

const [base, envFile, out] = process.argv.slice(2);
const env = Object.fromEntries(
  readFileSync(envFile, "utf8")
    .split("\n")
    .filter(Boolean)
    .map((line) => line.split("=", 2)),
);
mkdirSync(out, { recursive: true });
const browser = await chromium.launch({ executablePath: process.env.CHROMIUM ?? "/usr/bin/chromium" });
let page;
const fail = async (why) => {
  await page?.screenshot({ path: `${out}/failed.png`, fullPage: true });
  console.error(`FAIL: ${why}`);
  await browser.close();
  process.exit(1);
};
const ok = (what) => console.log(`ok: ${what}`);
const seen = async (text, what, timeout = 30_000) => {
  try {
    await page.getByText(text).first().waitFor({ timeout });
  } catch {
    await fail(`${what}: no ${text}`);
  }
};
const absent = async (text, what) => {
  if ((await page.getByText(text).count()) > 0) await fail(`${what}: ${text} is shown`);
};

/** A new page signed in as `username`, in `code`. */
async function signIn(username, code = "en") {
  const context = await browser.newContext({ viewport: { width: 1360, height: 900 }, locale: "en-GB" });
  page = await context.newPage();
  await page.goto(`${base}/login/`);
  await page.getByLabel("Username").fill(username);
  await page.getByLabel("Password").fill(env.PASSWORD);
  await page.getByRole("button", { name: "Sign in" }).click();
  await page.waitForURL((url) => !url.pathname.startsWith("/login"));
  await page.evaluate(async (code) => {
    localStorage.setItem("monitorflow.locale", code);
    await fetch("/api/me/preferences", {
      method: "PUT",
      headers: { "Content-Type": "application/json", "X-MonitorFlow-CSRF": "1" },
      body: JSON.stringify({ language: code }),
    });
  }, code);
}

// A global administrator, Acme chosen in the scope switcher.
await signIn("admin");
await page.goto(`${base}/reports/?customer=${env.ACME}`);
const summary = page.getByRole("region", { name: "Summary" });
await summary.waitFor({ timeout: 60_000 }).catch(() => fail("no summary on Reports"));
await seen(/1 server or device watched\. 3 outages, .* in all\./, "Acme's summary");
await seen("For Acme", "Acme's period line");
const services = page.getByRole("region", { name: "Business services", exact: true });
if (!(await services.getByText("Shop").count())) await fail("the Shop service is not in Acme's report");
await seen(/web-01: check port_shop/, "Acme's outages");
await seen("Shop patch", "Acme's planned maintenance");
await absent("db-01", "Acme's report");
// The PDF the button downloads.
const href = await page.getByRole("link", { name: "Download PDF" }).getAttribute("href");
const pdf = await page.request.get(`${base}${href}`);
if (pdf.status() !== 200 || pdf.headers()["content-type"] !== "application/pdf") await fail(`the PDF link answered ${pdf.status()}`);
await page.screenshot({ path: `${out}/acme.png`, fullPage: true });
ok("Acme: summary first, Shop, both outages, the patch window, and its PDF");

await page.goto(`${base}/reports/?customer=${env.GLOBEX}`);
await page.getByRole("region", { name: "Summary" }).waitFor();
await seen(/1 server or device watched\. 1 outage, .* in all\./, "Globex's summary");
await seen("db-01", "Globex's report");
await absent("web-01", "Globex's report");
await absent("Shop patch", "Globex's report");
ok("Globex: db-01 only");

// Everything: both schedules for the global administrator, with their last sending.
await page.goto(`${base}/reports/`);
const schedules = page.getByRole("region", { name: "Scheduled reports" });
await schedules.waitFor();
await seen("Every month", "scheduled reports");
if ((await schedules.getByRole("row").count()) < 3) await fail("both schedules should be listed");
await page.screenshot({ path: `${out}/everything.png`, fullPage: true });
ok("both scheduled reports listed");

// Acme's administrator: its own report without naming it, its service, its schedule.
await signIn("acme-admin");
await page.goto(`${base}/reports/`);
await page.getByRole("region", { name: "Summary" }).waitFor();
await seen("web-01", "Acme's administrator");
await absent("db-01", "Acme's administrator");
await absent("Globex", "Acme's administrator");
const managed = page.getByRole("region", { name: "Set up business services" });
if (!(await managed.getByText("Shop").count())) await fail("Acme's administrator does not see the Shop service");
ok("Acme's administrator sees only Acme");

// In French.
await signIn("globex-admin", "fr");
await page.goto(`${base}/reports/`);
await page.getByRole("region", { name: "Résumé" }).waitFor({ timeout: 30_000 });
await seen(/1 serveur ou équipement surveillé\. 1 panne, .* en tout\./, "French summary");
await seen("Télécharger le PDF", "French downloads");
await absent("web-01", "Globex in French");
await page.screenshot({ path: `${out}/globex-fr.png`, fullPage: true });
ok("Globex's administrator, in French");

await browser.close();
console.log("All passed.");
