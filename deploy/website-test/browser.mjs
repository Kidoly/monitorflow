// Looks at website checks in Chromium (ADR-0150), for run-e2e.sh: signs
// in, opens the server page, checks what the Websites list says, adds a
// website check through the form, and leaves screenshots in MF_WEBSITE_OUT.
import { createRequire } from "node:module";
import path from "node:path";

const env = (name) => {
  const value = process.env[name];
  if (!value) throw new Error(`${name} is not set`);
  return value;
};

const require = createRequire(path.join(env("MF_WEBSITE_PW"), "index.js"));
const { chromium } = require("playwright-core");

const WEB = env("MF_WEBSITE_WEB");
const SERVER = env("MF_WEBSITE_SERVER");
const OUT = env("MF_WEBSITE_OUT");
const browser = await chromium.launch({ executablePath: env("MF_WEBSITE_CHROMIUM"), headless: true });
const page = await (await browser.newContext({ locale: "en-US", viewport: { width: 1400, height: 1800 } })).newPage();

function check(condition, what) {
  if (!condition) throw new Error(`check failed: ${what}`);
  console.log(`ok   browser: ${what}`);
}

try {
  await page.goto(`${WEB}/login/`);
  const fold = page.getByRole("button", { name: "Sign in with a password" });
  await page.getByLabel("Username").or(fold).first().waitFor();
  if (await fold.isVisible()) await fold.click();
  await page.getByLabel("Username").fill("admin");
  await page.getByLabel("Password").fill(env("MF_WEBSITE_PASSWORD"));
  await page.getByRole("button", { name: "Sign in", exact: true }).click();
  await page.waitForURL(`${WEB}/`);

  await page.goto(`${WEB}/servers/view/?id=${SERVER}`);
  const table = page.getByRole("table", { name: "Websites" });
  await table.waitFor({ timeout: 30_000 });
  // The row titled exactly `title`: a host, and a path when not the home page.
  // Several rows share a title when the agent, the central server and the
  // relay check one site: `by` picks one.
  const text = async (title, by = "This server's agent") => {
    for (const row of await table.getByRole("row").all()) {
      const lines = (await row.innerText()).split(/[\n\t]/).map((l) => l.trim()).filter(Boolean);
      if (lines[0] === title && lines.includes(by)) return lines.join(" ");
    }
    const seen = [];
    for (const row of await table.getByRole("row").all()) seen.push((await row.innerText()).replace(/\s+/g, " "));
    throw new Error(`no row ${title} checked by ${by} in: ${seen.join(" | ")}`);
  };
  check((await text("web.test:8443")).includes("Answers"), "the valid site answers");
  check(/\d+ days left/.test(await text("web.test:8443")), "the valid certificate shows its days left");
  check((await text("web.test:8444")).includes("Expired"), "the expired certificate says so");
  check((await text("web.test:8445")).includes("For another name"), "the wrong name says so");
  check(/[45] days left/.test(await text("web.test:8446")), "the certificate expiring soon shows 4 or 5 days left");
  check((await text("web.test:8447")).includes("Self-signed"), "the self-signed certificate says so");
  check((await text("127.0.0.1:18443", "Central server")).includes("Answers"), "a check of the central server says who runs it");
  check((await text("web.test:8443", "Relay web-relay")).includes("Answers"), "a check of the relay says who runs it");
  check((await text("web.test:8443", "Central server")).includes("The name has no address"), "the central server does not know web.test");
  await page.screenshot({ path: path.join(OUT, "websites.png"), fullPage: true });

  // Adds one through the form, with the alerts that come with it.
  await page.getByRole("button", { name: "Add a check" }).click();
  const catalogue = page.getByRole("group", { name: "Check catalogue" });
  await catalogue.getByLabel("Find a check").fill("certificate");
  await catalogue.getByRole("button", { name: /^A certificate does not expire/ }).click();
  const form = page.getByRole("form", { name: "Add a check" });
  await form.getByLabel("Page address").fill("https://web.test:8446/");
  await form.getByText("More options").click();
  await form.getByLabel("Status codes that pass").fill("200");
  await page.screenshot({ path: path.join(OUT, "add-website.png"), fullPage: true });
  await form.getByRole("button", { name: "Add check" }).click();
  await page.getByRole("button", { name: "Add a check" }).waitFor();
  const added = await page.evaluate(async (id) => {
    const response = await fetch(`/api/servers/${id}/sensor_configs`, { credentials: "same-origin" });
    return (await response.json()).entries.find((e) => e.name === "http_web_test_8446");
  }, SERVER);
  check(added?.kind === "http" && added.params.url === "https://web.test:8446/", "the form added the website check");
  const rules = await page.evaluate(async (id) => {
    const response = await fetch(`/api/servers/${id}/thresholds`, { credentials: "same-origin" });
    return (await response.json()).entries.map((e) => e.metric).filter((m) => m.includes("http_web_test_8446"));
  }, SERVER);
  check(rules.length === 4, `the form added its four alerts (${rules.join(", ")})`);
} catch (error) {
  await page.screenshot({ path: path.join(OUT, "failure.png"), fullPage: true }).catch(() => {});
  throw error;
} finally {
  await browser.close();
}
