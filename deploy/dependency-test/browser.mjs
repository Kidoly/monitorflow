// The browser half of the dependency test (ADR-0152), run by run-e2e.sh
// while gw-host is down: opens the web interface in headless Chromium and
// checks what people see: the fleet counting what is unreachable, app-01
// saying why with a link to gw-host's incident, its Depends on tab, the
// incident listing what it hides, and the same banner in French. Saves
// screenshots in OUT.
//
//   node browser.mjs BASE_URL PASSWORD OUT APP_ID GW_ID INCIDENT_ID
//
// Needs playwright-core (NODE_PATH) and a Chromium (CHROMIUM, by default
// /usr/bin/chromium).
import { mkdirSync } from "node:fs";
import { createRequire } from "node:module";

// Through require, so that NODE_PATH finds it outside the repository.
const { chromium } = createRequire(import.meta.url)("playwright-core");

const [base, password, out, app, gw, incident] = process.argv.slice(2);
mkdirSync(out, { recursive: true });
const browser = await chromium.launch({ executablePath: process.env.CHROMIUM ?? "/usr/bin/chromium" });
const page = await browser.newPage({ viewport: { width: 1360, height: 900 }, locale: "en-GB" });
const fail = async (why) => {
  await page.screenshot({ path: `${out}/failed.png`, fullPage: true });
  console.error(`FAIL: ${why}`);
  await browser.close();
  process.exit(1);
};
const ok = (what) => console.log(`ok: ${what}`);
const seen = async (text, what, timeout = 60_000) => {
  try {
    await page.getByText(text).first().waitFor({ timeout });
  } catch {
    await fail(`${what}: no ${text}`);
  }
};
const shot = (name) => page.screenshot({ path: `${out}/${name}.png`, fullPage: true });

await page.goto(`${base}/login/`);
await page.getByLabel("Username").fill("admin");
await page.getByLabel("Password").fill(password);
await page.getByRole("button", { name: "Sign in" }).click();
await page.waitForURL((url) => !url.pathname.startsWith("/login"));
const language = (code) =>
  page.evaluate(async (code) => {
    localStorage.setItem("monitorflow.locale", code);
    await fetch("/api/me/preferences", {
      method: "PUT",
      headers: { "Content-Type": "application/json", "X-MonitorFlow-CSRF": "1" },
      body: JSON.stringify({ language: code }),
    });
  }, code);
await language("en");
ok("signed in");

// The fleet: the status strip counts them, each row says why.
await page.goto(`${base}/`);
await seen(/^3 Unreachable$/, "status strip");
await seen("gw-host is down", "fleet row");
await shot("fleet");
ok("the fleet counts 3 unreachable, and says gw-host is down");

// app-01's page: why, with gw-host's incident.
await page.goto(`${base}/servers/view/?id=${app}`);
await seen("Unreachable: gw-host is down.", "app-01's page");
const link = page.getByRole("link", { name: "See its incident" });
if (!(await link.getAttribute("href"))?.includes(incident)) await fail("the banner does not link gw-host's incident");
await shot("app-01");
await page.getByRole("tab", { name: "Depends on" }).click();
const parents = page.getByRole("table", { name: "Depends on" });
await parents.waitFor();
if (!(await parents.innerText()).includes("gw-host")) await fail("app-01's Depends on tab does not list gw-host");
await shot("app-01-depends-on");
ok("app-01 says it is unreachable because gw-host is down, and depends on it");

// gw-host's incident: what it hides.
await page.goto(`${base}/incidents/view/?id=${incident}`);
await seen("While this is down, 3 are unreachable behind it", "gw-host's incident");
const hidden = await page.getByRole("region", { name: "While this is down, 3 are unreachable behind it" }).innerText();
for (const name of ["app-01", "app-02", "nas"]) {
  if (!hidden.includes(name)) await fail(`the incident does not list ${name}`);
}
await shot("gw-host-incident");
ok("gw-host's incident lists app-01, app-02 and the nas");

// gw-host's own page: what is behind it.
await page.goto(`${base}/servers/view/?id=${gw}&tab=dependencies`);
const behind = page.getByRole("table", { name: "Behind it" });
await behind.waitFor({ timeout: 60_000 });
const rows = await behind.getByRole("row").count();
if (rows !== 4) await fail(`gw-host's Behind it table has ${rows - 1} rows`);
await shot("gw-host-behind-it");
ok("gw-host's page lists the 3 behind it");

// In French.
await language("fr");
await page.goto(`${base}/servers/view/?id=${app}`);
await seen(/Injoignable\s:\sgw-host est en panne\./, "app-01 in French");
await shot("app-01-fr");
await language("en");
ok("and in French");

await browser.close();
