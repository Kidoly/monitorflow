// The browser half of the probe test (ADR-0148): signs in to the server
// run-e2e.sh left running, through the web interface of this checkout,
// adds a device from the fleet page, adds a UDP check to it, and looks at
// the device that a relay cannot ping. Screenshots go to $SHOTS.
//
// Usage: MONITORFLOW_API_URL=http://127.0.0.1:17544 npx next dev -p 17600 (in web/), then
//        cp browser.mjs <a directory with playwright-core installed>/ and
//        node <that directory>/browser.mjs http://localhost:17600 <address> <password>
import { chromium } from "playwright-core";

const [base, address, password] = process.argv.slice(2);
const shots = process.env.SHOTS ?? ".";
const browser = await chromium.launch({ executablePath: process.env.CHROMIUM ?? "/usr/bin/chromium" });
const page = await browser.newPage({ viewport: { width: 1400, height: 1000 }, locale: "en-US" });
const ok = (what) => console.log(`ok: ${what}`);
const fail = async (what) => {
  await page.screenshot({ path: `${shots}/failed.png`, fullPage: true });
  console.error(`FAIL: ${what}`);
  await browser.close();
  process.exit(1);
};
const see = async (text, what, timeout = 60_000) => {
  try {
    await page.getByText(text).first().waitFor({ timeout });
  } catch {
    await fail(what);
  }
  ok(what);
};

await page.goto(`${base}/login/`);
await page.getByLabel("Username").fill("admin");
await page.getByLabel("Password").fill(password);
await page.getByRole("button", { name: "Sign in" }).click();
await see("Lossy link", "the fleet lists the devices, problems first");
await see(/Packet loss/, "the lossy link's packet loss shows as its problem");
// Healthy sites and devices are folded to one line.
await page.getByRole("button", { name: /^Show \d+ OK/ }).first().click();
await see("Branch NAS", "the healthy devices are there too");
await see(/Device · /, "devices read as devices, not as offline agents");
await page.screenshot({ path: `${shots}/fleet.png`, fullPage: true });

await page.getByRole("link", { name: "Add a device" }).click();
await page.getByLabel(/^Name/).fill("Floor switch");
await page.getByLabel(/^Address/).fill(address);
// The site run-e2e.sh gave a relay; it made another without one.
const site = page.getByRole("combobox", { name: /^Site/ });
if (await site.count()) await site.selectOption({ label: process.env.SITE ?? "main" });
await page.getByLabel("Checked from").waitFor();
await page.waitForFunction(() => [...document.querySelectorAll("select")].pop()?.value !== "");
await page.waitForTimeout(1000);
const from = await page.getByLabel("Checked from").inputValue();
if (from === "central") await fail("the relay is not chosen by default");
ok("the relay of the site is chosen by default");
await page.screenshot({ path: `${shots}/add-device.png`, fullPage: true });
await page.getByRole("button", { name: "Add the device" }).click();
await page.getByRole("heading", { level: 1, name: "Floor switch" }).waitFor({ timeout: 30_000 });
await see(address, "the new device's page gives its address");
await see("Device", "and says it is a device");
await see("Ping · from relay-branch", "its ping runs from the relay");
await see("answers", "its ping answers", 90_000);

await page.getByRole("button", { name: "Add a check" }).click();
await page.getByRole("button", { name: "A UDP port answers" }).click();
await page.getByLabel("Port").fill("53");
await page.getByLabel("Checked from").selectOption("central");
await page.getByRole("button", { name: "Add check" }).click();
await see("UDP port 53 · from central server", "a UDP check from the central server is added");
await page.screenshot({ path: `${shots}/device.png`, fullPage: true });

await page.goto(`${base}/`);
await page.getByRole("button", { name: /^Show \d+ OK/ }).first().click();
await page.getByRole("link", { name: "Branch NAS" }).click();
await see("Ping · from central server", "the NAS shows the central server's ping");
await see("Ping · from relay-branch", "and the relay's");
await page.screenshot({ path: `${shots}/nas.png`, fullPage: true });

// An SNMP device (ADR-0149): detected, then added with its kind's checks.
if (process.env.SNMPSIM) {
  await page.goto(`${base}/devices/add/`);
  const siteBox = page.getByRole("combobox", { name: /^Site/ });
  if (await siteBox.count()) await siteBox.selectOption({ label: process.env.SITE ?? "main" });
  await page.getByLabel(/^Address/).fill(process.env.SNMPSIM);
  await page.getByLabel(/Watch it over SNMP/).check();
  await page.getByLabel(/^Community/).fill("printer");
  await page.getByLabel("Checked from").selectOption("central");
  await page.getByRole("button", { name: "Detect" }).click();
  await see(/printer-2 answers: Test office laser printer/, "Detect says what the printer answers");
  await see(/It reports: .*Printer supplies/, "and what it reports");
  if ((await page.getByLabel(/^Kind of device/).inputValue()) !== "printer") await fail("the printer kind is not chosen");
  ok("the printer kind is chosen, and its name filled in");
  await page.screenshot({ path: `${shots}/add-snmp.png`, fullPage: true });
  await page.getByRole("button", { name: "Add the device" }).click();
  await page.getByRole("heading", { level: 1, name: "printer-2" }).waitFor({ timeout: 30_000 });
  await see(/Supply level Black Toner/, "its supplies are listed", 120_000);
  await see("6%", "the black toner reads 6%", 120_000);
  await page.getByRole("button", { name: "Details" }).click();
  await see("v2c, port 161", "Details say how it is reached, without the community");
  if ((await page.content()).includes(">printer<")) await fail("the community is shown");
  await page.screenshot({ path: `${shots}/snmp-device.png`, fullPage: true });
}

// The same page in French.
const fr = await browser.newPage({ viewport: { width: 1400, height: 1000 }, locale: "fr-FR" });
await fr.context().addCookies(await page.context().cookies());
await fr.goto(page.url());
try {
  await fr.getByText("Ping · depuis serveur central").first().waitFor({ timeout: 30_000 });
} catch {
  await fail("the French page names where the ping runs from");
}
ok("the French page names where the ping runs from");
await fr.screenshot({ path: `${shots}/nas-fr.png`, fullPage: true });
await browser.close();
console.log("browser: all passed");
