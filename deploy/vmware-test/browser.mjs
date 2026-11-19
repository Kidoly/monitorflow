// The browser half of the VMware test (ADR-0156): signs in to the server
// run-e2e.sh left running, through the web interface of this checkout,
// looks at the vCenter's page and at the page of a server matched to one
// of its virtual machines, and tests a connection from Add a device up to
// the certificate's fingerprint. Screenshots go to $SHOTS.
//
// Usage: MONITORFLOW_API_URL=http://127.0.0.1:27944 npx next dev -p 27990 (in web/), then
//        cp browser.mjs <a directory with playwright-core installed>/ and
//        node <that directory>/browser.mjs http://localhost:27990 <vcsim address> <password>
import { chromium } from "playwright-core";

const [base, vcsim, password] = process.argv.slice(2);
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
await see("vcenter-1", "the fleet lists the vCenter");
await page.getByRole("link", { name: "vcenter-1" }).first().click();
await see(/Read .*govmomi simulator/, "the vCenter's page says when it was read and what answered");
await see("DC0_C0_H1", "its hosts are listed");
// run-e2e.sh reconnected the host it had disconnected.
await see("Connected", "hosts say whether they are connected");
await see("matched by its hardware ID", "a virtual machine names the server it is");
await see("LocalDS_1", "its datastores are listed");
await see(/^2, oldest /, "snapshots show their count and the oldest's age");
await page.screenshot({ path: `${shots}/vcenter.png`, fullPage: true });

await page.getByRole("link", { name: "app-01" }).first().click();
await see(/Virtual machine DC0_H0_VM0 on host DC0_H0 of vcenter-1, matched by its hardware ID/, "app-01 says which virtual machine it is");
await page.screenshot({ path: `${shots}/app-01.png`, fullPage: true });

await page.goto(`${base}/devices/add/`);
await page.getByLabel(/^Name/).fill("vcenter-2");
await page.getByLabel(/^Address/).fill(vcsim);
await page.getByLabel(/It is a vCenter or an ESXi host/).check();
await page.getByLabel(/^User/).fill("reader");
await page.getByLabel(/^Password/).fill("reader-pass");
await page.getByLabel(/^Port/).fill("8989");
await page.getByRole("button", { name: "Test the connection" }).click();
await see(/govmomi simulator\) answers/, "the test reaches vcsim from the relay");
await see(/not signed by an authority/, "the self-signed certificate is shown, not trusted");
await page.getByLabel(/trust this certificate/).check();
await page.getByRole("button", { name: "Test the connection" }).click();
await see(/Signed in: 7 virtual machines, 5 hosts, 2 datastores/, "trusted, the login works and counts what it sees");
await page.screenshot({ path: `${shots}/add-vcenter.png`, fullPage: true });

await browser.close();
console.log("browser: all passed");
