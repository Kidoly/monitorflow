// The browser half of the silence test (ADR-0151): after run-e2e.sh with
// MF_SILENCE_E2E_KEEP=1, opens the web interface in headless Chromium and
// checks what people see: the maintenance windows in plain words, the
// server in maintenance, the incident marked so, and one check silenced
// from its row in one click. Saves screenshots in OUT.
//
//   node browser.mjs BASE_URL PASSWORD OUT
//
// Needs playwright-core (NODE_PATH) and a Chromium (CHROMIUM, by default
// /usr/bin/chromium).
import { mkdirSync } from "node:fs";
import { createRequire } from "node:module";

// Through require, so that NODE_PATH finds it outside the repository.
const { chromium } = createRequire(import.meta.url)("playwright-core");

const [base, password, out] = process.argv.slice(2);
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
const seen = async (text, what, timeout = 20_000) => {
  try {
    await page.getByText(text).first().waitFor({ timeout });
  } catch {
    await fail(`${what}: no ${text}`);
  }
};

await page.goto(`${base}/login/`);
await page.getByLabel("Username").fill("admin");
await page.getByLabel("Password").fill(password);
await page.getByRole("button", { name: "Sign in" }).click();
await page.waitForURL((url) => !url.pathname.startsWith("/login"));
// In English, whatever an earlier run saved for this person.
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

// The Silences page: windows in plain words, the running one, the next one.
await page.goto(`${base}/silences/`);
const windows = page.getByRole("table", { name: "Maintenance windows" });
await windows.waitFor();
await seen(/Every day, \d\d:\d\d to \d\d:\d\d \(Europe\/Paris\)/, "silences page");
await seen(/In maintenance until/, "silences page");
await seen(/^Every \w+day, 03:00 to 05:00 \(Europe\/Paris\)$/, "silences page");
await seen(/Next: \w+day \d+ \w+, 03:00 to 05:00/, "silences page");
await page.screenshot({ path: `${out}/silences.png`, fullPage: true });
ok("the Silences page lists both windows in plain words, with the next window");

// db-01 is in maintenance, and its page says so.
await page.goto(`${base}/`);
await page.getByRole("link", { name: "db-01" }).first().click();
await seen(/In maintenance until .*\./, "db-01's page");
await seen(/Maintenance window: Every day/, "db-01's page");
await page.screenshot({ path: `${out}/server-in-maintenance.png`, fullPage: true });
ok("db-01's page says it is in maintenance, and which window");

// The incident list marks port_d.
await page.goto(`${base}/incidents/`);
const row = page.getByRole("row").filter({ hasText: "db-01" }).first();
try {
  await row.getByText("In maintenance").waitFor({ timeout: 20_000 });
} catch {
  await fail("the incident list does not mark db-01's incident in maintenance");
}
await page.screenshot({ path: `${out}/incidents.png`, fullPage: true });
ok("the incident list marks db-01's incident in maintenance");

// One click silences one check of web-01 from its row.
await page.goto(`${base}/`);
await page.getByRole("link", { name: "web-01" }).first().click();
const actions = page.getByRole("button", { name: /^Actions for .*port_c/i }).first();
await actions.waitFor();
await actions.click();
await page.getByRole("menuitem", { name: "Silence this check for 1 hour" }).click();
await seen(/^Silenced until/, "web-01's port_c row");
await seen(/Checks silenced: port_c\./, "web-01's page");
// The server itself is not silenced: its menu still offers a silence.
try {
  await page.getByRole("button", { name: "Silence notifications about web-01" }).waitFor({ timeout: 5_000 });
} catch {
  await fail("web-01's header has no Silence menu");
}
if ((await page.getByRole("button", { name: "Silence notifications about web-01" }).textContent())?.includes("longer")) {
  await fail("web-01 shows as silenced");
}
await page.screenshot({ path: `${out}/check-silenced.png`, fullPage: true });
ok("port_c silenced from its row; web-01 itself is not silenced");

// French.
await language("fr");
await page.goto(`${base}/silences/`);
await seen(/Chaque jour, de \d\d:\d\d à \d\d:\d\d \(Europe\/Paris\)/, "the French Silences page");
await seen(/En maintenance jusqu’à/, "the French Silences page");
await page.screenshot({ path: `${out}/silences-fr.png`, fullPage: true });
ok("the Silences page in French");
await language("en");

await browser.close();
