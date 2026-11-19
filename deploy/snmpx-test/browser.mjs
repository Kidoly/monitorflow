// The browser half of the custom SNMP test (ADR-0157): signs in to the
// server run-e2e.sh left running, through the web interface of this
// checkout; on the firewall device, reads the custom check the API made,
// then adds one with a channel found in the library and one picked in
// Browse, tests it from the probe and saves it; on Settings, uploads a MIB
// file and makes a lookup of what it names. Screenshots go to $SHOTS.
//
// Usage: node browser.mjs <web base URL> <password> <firewall device ID> <MIB file>
import { chromium } from "playwright-core";

const [base, password, firewall, mibFile] = process.argv.slice(2);
const shots = process.env.SHOTS ?? ".";
const browser = await chromium.launch({ executablePath: process.env.CHROMIUM ?? "/usr/bin/chromium" });
const page = await browser.newPage({ viewport: { width: 1400, height: 1100 }, locale: "en-US" });
const ok = (what) => console.log(`ok: ${what}`);
const fail = async (what, error) => {
  await page.screenshot({ path: `${shots}/failed.png`, fullPage: true });
  console.error(`FAIL: ${what}${error ? `: ${error.message.split("\n")[0]}` : ""}`);
  await browser.close();
  process.exit(1);
};
const step = async (what, action) => {
  try {
    await action();
  } catch (error) {
    await fail(what, error);
  }
  ok(what);
};

await step("signed in", async () => {
  await page.goto(`${base}/login/`);
  await page.getByLabel("Username").fill("admin");
  await page.getByLabel("Password").fill(password);
  await page.getByRole("button", { name: "Sign in" }).click();
  await page.waitForURL((url) => !url.pathname.startsWith("/login"), { timeout: 30_000 });
});

await step("the firewall's custom check shows its channels with values and states", async () => {
  await page.goto(`${base}/servers/view/?id=${encodeURIComponent(firewall)}`);
  const table = page.getByRole("table", { name: "snmp_fw" });
  await table.waitFor({ timeout: 60_000 });
  await table.getByText("Sessions").first().waitFor();
  await table.getByText(/sessions$/).first().waitFor({ timeout: 60_000 });
  await table.getByText("Master").first().waitFor({ timeout: 60_000 });
  await table.getByText("v7.2.5").first().waitFor({ timeout: 60_000 });
  await page.screenshot({ path: `${shots}/device-checks.png`, fullPage: true });
});

const form = page.getByRole("form", { name: "Add a custom SNMP check" });
await step("the form opens with the site's probe chosen", async () => {
  await page.getByRole("button", { name: "Add a custom check" }).click();
  await form.waitFor();
  await form.getByLabel(/^Name\s*Lowercase/).fill("snmp_browser");
  // The site's relay is chosen by default; the firewall is checked from here.
  await page.waitForFunction(() => [...document.querySelectorAll("form select")].some((s) => s.value !== "" && s.options.length > 1));
  await form.getByLabel("Checked from").selectOption("central");
});

await step("the library names the firewall's processor use", async () => {
  await form.getByRole("button", { name: "Find in the library and MIBs…" }).click();
  const picker = form.getByRole("group", { name: "Find an object" });
  await picker.getByLabel("Name, words or OID").fill("fgSysCpu");
  await picker.getByRole("button", { name: "Use fgSysCpuUsage.0" }).click({ timeout: 30_000 });
  const channel = form.getByRole("group", { name: "Channel 1" });
  if ((await channel.getByLabel(/^OID/).inputValue()) !== "1.3.6.1.4.1.12356.101.4.1.3.0") throw new Error("the OID was not filled in");
  if ((await channel.getByLabel("Unit").inputValue()) !== "%") throw new Error("the unit was not filled in");
});

await step("Browse lists what the firewall answers, named by the uploaded MIB", async () => {
  await form.getByRole("button", { name: "Browse the device…" }).click();
  const browse = form.getByRole("group", { name: /^Browse / });
  await browse.getByLabel("Under OID").fill("1.3.6.1.4.1.12356.101.4.1");
  await browse.getByRole("button", { name: "List" }).click();
  await browse.getByText("testFwSessions.0").waitFor({ timeout: 60_000 });
  await page.screenshot({ path: `${shots}/browse.png`, fullPage: true });
  await browse.getByRole("button", { name: "Watch testFwVersion.0" }).click();
  const channel = form.getByRole("group", { name: "Channel 2" });
  if ((await channel.getByLabel("Kind").inputValue()) !== "text") throw new Error("the firmware is not a text");
  await channel.getByLabel("OK when the text").selectOption("matches");
  await channel.getByLabel("Text or pattern").fill("^v7\\.");
});

await step("Test reads both channels from the probe", async () => {
  await form.getByRole("button", { name: "Test", exact: true }).click();
  const tested = page.getByRole("table", { name: /^What .* read$/ });
  await tested.waitFor({ timeout: 60_000 });
  await tested.getByText("Gauge32 12").waitFor();
  await tested.getByText(/OK · v7\.2\.5/).waitFor();
  await page.screenshot({ path: `${shots}/test.png`, fullPage: true });
});

await step("the check is saved and listed with its values", async () => {
  await form.getByRole("button", { name: "Save the check" }).click();
  const table = page.getByRole("table", { name: "snmp_browser" });
  await table.waitFor({ timeout: 30_000 });
  await table.getByText("12 %").first().waitFor({ timeout: 90_000 });
});

await step("Settings › SNMP lookups and MIBs lists the uploaded MIB and the lookups", async () => {
  await page.goto(`${base}/settings/snmp/`);
  await page.getByRole("heading", { name: "SNMP lookups and MIBs" }).waitFor();
  await page.getByRole("table", { name: "MIB files" }).getByText("TEST-FW-MIB", { exact: true }).waitFor({ timeout: 30_000 });
  await page.getByRole("table", { name: "Lookups" }).getByText("HA role", { exact: true }).waitFor();
});

await step("a MIB uploaded from the page offers its values as a lookup", async () => {
  await page.getByLabel(/Upload MIB files/).setInputFiles(mibFile);
  await page.getByText("Added: TEST-FW-MIB.").waitFor({ timeout: 30_000 });
  await page.getByRole("button", { name: "Make a lookup of testFwHaRole" }).click();
  const lookup = page.getByRole("form", { name: "New lookup" });
  await lookup.getByLabel("Name", { exact: true }).fill("HA role from the MIB");
  await lookup.getByRole("button", { name: "Save the lookup" }).click();
  const row = page.getByRole("table", { name: "Lookups" }).getByRole("row", { name: /HA role from the MIB/ });
  await row.waitFor({ timeout: 30_000 });
  await row.getByText(/1 = master, 2 = backup, 3 = failed/).waitFor();
  await page.screenshot({ path: `${shots}/library.png`, fullPage: true });
});

await step("the page speaks French", async () => {
  await page.evaluate(() => localStorage.setItem("monitorflow.locale", "fr"));
  await page.reload();
  await page.getByRole("heading", { name: "Tables de correspondance et MIB SNMP" }).waitFor({ timeout: 30_000 });
});

await browser.close();
