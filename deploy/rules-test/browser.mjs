// The browser half of the richer rules test (ADR-0153): after run-e2e.sh
// (which leaves everything running), opens the web interface in headless
// Chromium and checks what people see: the site's alerts in plain words, a
// rule switched to a forecast in the editor with its live preview, the
// sensor page's richer rules, and the forecast incident with its expected
// date, in English and French. Saves screenshots in OUT.
//
//   node browser.mjs BASE_URL PASSWORD SITE_ID SERVER_ID INCIDENT_ID OUT
//
// Needs playwright-core (NODE_PATH) and a Chromium (CHROMIUM, by default
// /usr/bin/chromium).
import { mkdirSync } from "node:fs";
import { createRequire } from "node:module";

const { chromium } = createRequire(import.meta.url)("playwright-core");

const [base, password, site, server, incident, out] = process.argv.slice(2);
mkdirSync(out, { recursive: true });
const browser = await chromium.launch({ executablePath: process.env.CHROMIUM ?? "/usr/bin/chromium" });
const page = await browser.newPage({ viewport: { width: 1360, height: 1000 }, locale: "en-GB" });
const fail = async (why) => {
  await page.screenshot({ path: `${out}/failed.png`, fullPage: true });
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

// The site's alerts: the forecast added from the suggestion, in words.
await page.goto(`${base}/sites/view/?id=${site}`);
await seen(/Warn 3 d before a disk reaches 100% full, critical 1 d before\./, "site alerts");
ok("the site's disk forecast reads in plain words");

// Editing the disk alert: switch to a forecast, see the preview, cancel.
const line = page.locator("li", { hasText: "Warn when a disk is over 85% full" }).first();
await line.getByRole("button", { name: "Edit" }).click();
const form = page.getByRole("form", { name: "Edit alert" });
await form.getByLabel("Alert on").selectOption({ label: "When it will reach a value" });
await seen("This adds another alert on the same measure. The current one stays.", "editor");
const preview = form.getByRole("region", { name: "What this rule would have done" });
try {
  await preview.getByText(/would have opened|No incident over this period/).first().waitFor({ timeout: 60_000 });
} catch {
  await fail("the preview gave no answer");
}
ok(`the preview answers: ${(await preview.innerText()).split("\n").slice(0, 3).join(" / ")}`);
await page.screenshot({ path: `${out}/editor-forecast.png`, fullPage: true });
await form.getByRole("button", { name: "Cancel" }).click();

// The sensor page of /fill: its forecast listed under the threshold.
await page.goto(`${base}/servers/sensor/?id=${server}&sensor=disk_used_ratio&instance=${encodeURIComponent("/fill")}`);
await seen(/Also alerts/, "sensor page");
await seen(/before .*\/fill reaches 100%/, "sensor page");
await page.screenshot({ path: `${out}/sensor.png`, fullPage: true });
ok("the sensor page lists the forecast of /fill");

// The forecast incident: time left and the expected date.
await page.goto(`${base}/incidents/view/?id=${incident}`);
await seen(/left$|reached/, "incident tiles");
await seen(/Expected around \d{4}-\d\d-\d\d \d\d:\d\d/, "incident rule");
await page.screenshot({ path: `${out}/incident.png`, fullPage: true });
ok("the forecast incident says the time left and when it expects the disk full");

// In French.
await language("fr");
await page.goto(`${base}/incidents/view/?id=${incident}`);
await seen(/Prévu vers \d{4}-\d\d-\d\d \d\d:\d\d/, "incident in French");
await page.goto(`${base}/sites/view/?id=${site}`);
await seen(/Avertir quand un disque sera à 100\s%\sde remplissage dans moins de 3\sj/, "site alerts in French");
await page.screenshot({ path: `${out}/site-fr.png`, fullPage: true });
ok("French: the forecast and the expected date");
await language("en");

await browser.close();
console.log("browser: all passed");
