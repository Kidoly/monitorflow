// The browser half of the discovery test (ADR-0154): signs in to the
// server run-e2e.sh left running, through the web interface of this
// checkout, opens the scan from the fleet page, reads what it proposes,
// adds one address, dismisses another, and creates a scan of its own.
// Screenshots go to $SHOTS.
//
// Usage (run-e2e.sh does it): node browser.mjs <web URL> <password> <new address> <SSH host address>
import { chromium } from "playwright-core";

const [base, password, newcomer, sshHost] = process.argv.slice(2);
const shots = process.env.SHOTS ?? ".";
const browser = await chromium.launch({ executablePath: process.env.CHROMIUM ?? "/usr/bin/chromium" });
const page = await browser.newPage({ viewport: { width: 1400, height: 1100 }, locale: "en-US" });
const ok = (what) => console.log(`ok: ${what}`);
const fail = async (what) => {
  await page.screenshot({ path: `${shots}/failed.png`, fullPage: true });
  console.error(`FAIL: ${what}`);
  await browser.close();
  process.exit(1);
};
const see = async (text, what, timeout = 60_000, within = page) => {
  try {
    await within.getByText(text).first().waitFor({ timeout });
  } catch {
    await fail(what);
  }
  ok(what);
};

await page.goto(`${base}/login/`);
await page.getByLabel("Username").fill("admin");
await page.getByLabel("Password").fill(password);
await page.getByRole("button", { name: "Sign in" }).click();
await page.getByRole("link", { name: "Scan the network" }).click({ timeout: 60_000 });
await see("Office network", "the fleet page leads to the network scans");
await see("Office from the relay", "both scans are listed");
await page.screenshot({ path: `${shots}/scans.png`, fullPage: true });

await page.getByRole("link", { name: "Office network" }).click();
await page.getByRole("heading", { level: 1, name: "Office network" }).waitFor({ timeout: 30_000 });
const proposed = page.getByRole("table", { name: "Proposed" });
await see("Install the agent: a Linux or Unix server, SSH is open", "the SSH host is proposed for the agent", 30_000, proposed);
await see("Install the agent: a Windows machine, RDP or WinRM is open", "the RDP host too", 30_000, proposed);
await see(newcomer, "the newcomer is listed", 30_000, proposed);
await see("New", "and marked new", 30_000, proposed);
await see(/Since the scan before: 1 new, 1 gone/, "the change since the scan before is said");
const monitored = page.getByRole("table", { name: "Already monitored" });
await see("Watched as app-01 (same MAC address)", "the agent is recognised by its MAC address", 30_000, monitored);
await see(/Watched as .*\(same address\)/, "the devices added are recognised by their address", 30_000, monitored);
await page.screenshot({ path: `${shots}/results.png`, fullPage: true });

await page.getByRole("button", { name: `Add ${newcomer}` }).click();
await see("1 device added.", "one click adds the newcomer");
await see(/Watched as .*\(same address\)/, "and it moves to Already monitored");
await page.getByRole("button", { name: `Dismiss ${sshHost}` }).click();
await see(/Show dismissed \(1\)/, "the SSH host can be dismissed");
await page.screenshot({ path: `${shots}/after-add.png`, fullPage: true });
await page.getByRole("button", { name: /Show dismissed/ }).click();
await page.getByRole("button", { name: `Restore ${sshHost}` }).click();
await see("Install the agent: a Linux or Unix server, SSH is open", "and restored", 30_000, proposed);

await page.getByRole("link", { name: "Network scans" }).first().click();
await page.getByRole("button", { name: "New scan" }).click();
await page.getByLabel(/^Name/).fill("From the browser");
await page.getByLabel(/^Addresses/).fill(`${newcomer}, ${sshHost}`);
await page.waitForFunction(() => [...document.querySelectorAll("select")].some((s) => s.value !== ""));
await page.getByRole("button", { name: "Scan now" }).click();
await page.getByRole("heading", { level: 1, name: "From the browser" }).waitFor({ timeout: 30_000 });
await see("2 addresses answered.", "a scan made in the browser runs and finds both", 150_000);
await page.screenshot({ path: `${shots}/new-scan.png`, fullPage: true });

await browser.close();
