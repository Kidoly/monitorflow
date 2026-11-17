// Drives Chromium through MonitorFlow's sign-in providers (ADR-0145), for
// deploy/sso-test.sh, which starts everything and passes what it made in
// MF_SSO_* variables. Each check prints one line; the first failure stops
// the run with a screenshot in MF_SSO_ARTIFACTS (the working directory by
// default).
import { execFileSync } from "node:child_process";
import { readFileSync } from "node:fs";
import { createRequire } from "node:module";
import path from "node:path";

const env = (name) => {
  const value = process.env[name];
  if (!value) throw new Error(`${name} is not set`);
  return value;
};

const require = createRequire(path.join(env("MF_SSO_PW"), "index.js"));
const { chromium } = require("playwright-core");

const WEB = env("MF_SSO_WEB");
const CA = readFileSync(path.join(env("MF_SSO_CERTS"), "ca.pem"), "utf8");
const ROOT_PASSWORD = env("MF_SSO_ROOT_PASSWORD");

/** Asks deploy/sso-test.sh to change something at a provider. */
const ctl = (...args) => execFileSync(env("MF_SSO_CTL"), ["ctl", ...args], { stdio: "inherit" });

const browser = await chromium.launch({ executablePath: env("MF_SSO_CHROMIUM"), headless: true });
let page;

function check(condition, what) {
  if (!condition) throw new Error(`check failed: ${what}`);
  console.log(`ok   ${what}`);
}

/** A fresh browser, as someone who never signed in: no cookie of MonitorFlow or of a provider. */
async function fresh() {
  if (page) await page.context().close();
  // The browser does not trust the throwaway CA; MonitorFlow's server does,
  // through the providers' settings, and that link is what is tested.
  const context = await browser.newContext({ ignoreHTTPSErrors: true, locale: "en-US" });
  page = await context.newPage();
  return page;
}

/** `GET /api/auth/me` in the page, as the web interface asks it. */
async function me() {
  return page.evaluate(async () => {
    const response = await fetch("/api/auth/me", { credentials: "same-origin" });
    return { status: response.status, body: response.ok ? await response.json() : null };
  });
}

/** A JSON call of the API in the page, with the CSRF header. */
async function call(method, url, body) {
  return page.evaluate(
    async ({ method, url, body }) => {
      const response = await fetch(url, {
        method,
        credentials: "same-origin",
        headers: { "Content-Type": "application/json", "X-MonitorFlow-CSRF": "1" },
        body: body === undefined ? undefined : JSON.stringify(body),
      });
      const text = await response.text();
      return { status: response.status, body: text ? JSON.parse(text) : null };
    },
    { method, url, body },
  );
}

/** Waits for `locator` to show, then reports `what`. */
async function shows(locator, what) {
  await locator.waitFor({ timeout: 15_000 });
  check(true, what);
}

const roles = (body) => (body?.roles ?? []).map((r) => r.role).sort().join(",");

async function passwordSignIn(username, password) {
  await page.goto(`${WEB}/login/`);
  const fold = page.getByRole("button", { name: "Sign in with a password" });
  await page.getByLabel("Username").or(fold).first().waitFor();
  if (await fold.isVisible()) await fold.click();
  await page.getByLabel("Username").fill(username);
  await page.getByLabel("Password").fill(password);
  await page.getByRole("button", { name: "Sign in", exact: true }).click();
}

async function signInAsRoot() {
  await fresh();
  await passwordSignIn("root", ROOT_PASSWORD);
  await page.waitForURL(`${WEB}/`);
}

async function keycloakSignIn(username, password) {
  await fresh();
  await page.goto(`${WEB}/login/`);
  await page.getByRole("link", { name: "Sign in with Keycloak" }).click();
  await page.waitForURL(/localhost:28543/);
  await page.locator("#username").fill(username);
  await page.locator("#password").fill(password);
  await page.locator("#kc-login").click();
}

async function authentikSignIn(username, password) {
  await fresh();
  await page.goto(`${WEB}/login/`);
  await page.getByRole("link", { name: "Sign in with authentik" }).click();
  await page.waitForURL(/localhost:28643/);
  await page.locator('input[name="uidField"]').fill(username);
  await page.keyboard.press("Enter");
  // The password stage replaces the identification stage: wait for it.
  const field = page.getByPlaceholder("Please enter your password");
  await field.waitFor();
  await field.fill(password);
  await field.press("Enter");
}

/** Where the browser lands after a provider's sign-in: the fleet, or the login page with a reason. */
async function landed() {
  await page.waitForURL((url) => url.pathname === "/" || url.pathname === "/login/", { timeout: 30_000 });
  await page.waitForLoadState("networkidle");
  return new URL(page.url());
}

try {
  // 1. The local administrator sets Keycloak up from Settings › Sign-in.
  await signInAsRoot();
  check((await me()).body?.username === "root", "the local administrator signs in with a password");
  await page.goto(`${WEB}/settings/sign-in/`);
  await page.getByRole("button", { name: "Add OpenID Connect provider" }).click();
  const form = page.getByRole("form", { name: "New OpenID Connect provider" });
  await form.getByLabel(/^Name/).fill("Keycloak");
  await form.getByLabel(/^Issuer address/).fill("https://localhost:28543/realms/mf");
  await form.getByLabel(/^Client ID/).fill("monitorflow");
  await form.getByLabel(/^Client secret/).fill(env("MF_SSO_KC_SECRET"));
  check(
    await form.getByText(`${WEB}/api/auth/oidc/callback`).isVisible(),
    "the form shows the redirect URI to enter in the provider",
  );
  await form.getByText("Advanced").click();
  await form.getByLabel(/^Certificate authority/).fill(CA);
  const turnOn = form.getByRole("button", { name: "Save and turn on" });
  check(await turnOn.isDisabled(), "Save and turn on waits for a passing test");
  await form.getByRole("button", { name: "Test connection" }).click();
  await form.getByText("Connection works").waitFor();
  check(await form.getByText(/answered with \d+ signing key/).isVisible(), "Test connection reads Keycloak's discovery and keys");
  check(await form.getByText("The client ID and secret were accepted.").isVisible(), "Test connection checks the client secret");
  await turnOn.click();
  await page.getByRole("form", { name: "Groups and roles" }).waitFor();
  const groups = page.getByRole("form", { name: "Groups and roles" });
  await groups.getByRole("button", { name: "Add a group" }).click();
  await groups.getByLabel("Group").nth(0).fill("mf-admins");
  await groups.getByLabel("Role").nth(0).selectOption("administrator");
  await groups.getByRole("button", { name: "Add a group" }).click();
  await groups.getByLabel("Group").nth(1).fill("helpdesk");
  await groups.getByLabel("Role").nth(1).selectOption("viewer");
  await groups.getByRole("button", { name: "Save groups" }).click();
  await groups.getByText("Saved").waitFor();
  const providers = (await call("GET", "/api/identity-providers")).body.providers;
  const keycloak = providers.find((p) => p.name === "Keycloak");
  check(keycloak?.enabled && keycloak.mappings.length === 2, "Keycloak is on with two groups, from the web interface");
  check(!JSON.stringify(providers).includes(env("MF_SSO_KC_SECRET")), "the client secret is never shown back");

  // 2. First sign-in through Keycloak makes the account with its role.
  await keycloakSignIn("alice", "alice-keycloak-pw");
  check((await landed()).pathname === "/", "alice signs in through Keycloak and lands on the fleet");
  let who = await me();
  check(who.body?.username === "alice" && who.body?.provider?.name === "Keycloak", "the account was created at the first sign-in");
  check(roles(who.body) === "administrator", "alice is an administrator through mf-admins");

  // Her group administers everything, yet the last local administrator stays.
  const users = (await call("GET", "/api/users")).body.users;
  const root = users.find((u) => u.username === "root");
  const removal = await call("DELETE", `/api/users/${root.id}`);
  check(removal.status === 409, "the last local administrator cannot be removed, even by a provider's administrator");
  await page.goto(`${WEB}/settings/users/`);
  await shows(
    page.getByText("Roles follow this account's groups in Keycloak, under Settings › Sign-in."),
    "the Users page says alice's roles follow her Keycloak groups",
  );
  await page.goto(`${WEB}/settings/preferences/`);
  await shows(
    page.getByText("Your password is kept by Keycloak. Change it there."),
    "Preferences sends alice to Keycloak for her password",
  );

  // 3. A group change changes the role at the next sign-in.
  ctl("kc-groups", "alice", "helpdesk");
  await keycloakSignIn("alice", "alice-keycloak-pw");
  check((await landed()).pathname === "/", "alice signs in again");
  check(roles((await me()).body) === "viewer", "moved to helpdesk, alice is now a viewer");

  // 4. Without a mapped group, the sign-in is refused.
  ctl("kc-groups", "alice", "staff");
  await keycloakSignIn("alice", "alice-keycloak-pw");
  const refused = await landed();
  check(refused.pathname === "/login/" && refused.searchParams.get("sso") === "no_group", "in no mapped group, alice is refused");
  await shows(
    page.getByText("Your account is in none of the groups that give access to MonitorFlow."),
    "the login page says why",
  );
  check((await me()).status === 401, "and she has no session");

  // 5. authentik, the owner's provider at home.
  await signInAsRoot();
  const created = await call("POST", "/api/identity-providers", {
    name: "authentik",
    enabled: true,
    secret: env("MF_SSO_AK_SECRET"),
    settings: { kind: "oidc", issuer: "https://localhost:28643/application/o/monitorflow/", client_id: "monitorflow", ca_pem: CA },
  });
  check(created.status === 201, `authentik passes its test and is turned on (${created.status} ${JSON.stringify(created.body?.error ?? "")})`);
  await call("PUT", `/api/identity-providers/${created.body.id}/groups`, {
    mappings: [{ group: "mf-operators", role: "operator" }],
  });
  await authentikSignIn("carol", "carol-authentik-pw");
  check((await landed()).pathname === "/", "carol signs in through authentik");
  who = await me();
  check(who.body?.username === "carol" && roles(who.body) === "operator", "carol is an operator through mf-operators");
  ctl("ak-groups", "carol", "other");
  await authentikSignIn("carol", "carol-authentik-pw");
  check((await landed()).searchParams.get("sso") === "no_group", "out of mf-operators, carol is refused");

  // 6. An LDAP directory over LDAPS, with nested groups.
  await signInAsRoot();
  const ldaps = {
    kind: "ldap",
    url: "ldaps://localhost:28636",
    starttls: false,
    ca_pem: CA,
    bind_dn: "cn=monitorflow,dc=example,dc=org",
    user_base_dn: "ou=people,dc=example,dc=org",
    group_base_dn: "ou=groups,dc=example,dc=org",
    nested_groups: true,
  };
  const plain = await call("POST", "/api/identity-providers", {
    name: "Plain",
    secret: env("MF_SSO_LDAP_PASSWORD"),
    settings: { ...ldaps, url: "ldap://localhost:28389", starttls: false },
  });
  check(plain.status === 400 && /Plain LDAP is refused/.test(plain.body.error), "plain LDAP is refused when saving");
  const wrongPort = await call("POST", "/api/identity-providers/test", {
    secret: env("MF_SSO_LDAP_PASSWORD"),
    settings: { ...ldaps, url: "ldaps://localhost:28389" },
  });
  check(!wrongPort.body.ok, "LDAPS to a plain LDAP port fails rather than falling back");
  const directory = await call("POST", "/api/identity-providers", {
    name: "Directory",
    enabled: true,
    secret: env("MF_SSO_LDAP_PASSWORD"),
    settings: ldaps,
  });
  check(directory.status === 201, `the LDAPS directory passes its test and is turned on (${JSON.stringify(directory.body?.error ?? "")})`);
  await call("PUT", `/api/identity-providers/${directory.body.id}/groups`, {
    mappings: [
      { group: "mf-viewers", role: "viewer" },
      { group: "mf-operators", role: "operator" },
    ],
  });
  const tried = await call("POST", "/api/identity-providers/test", {
    id: directory.body.id,
    settings: ldaps,
    username: "dave",
    password: "dave-directory-pw",
  });
  check(
    tried.body.ok && tried.body.ldap.groups.includes("mf-viewers") && tried.body.roles.map((r) => r.role).join() === "viewer",
    "Test connection with a person shows dave's nested groups and role",
  );
  await fresh();
  await passwordSignIn("dave", "dave-directory-pw");
  await page.waitForURL(`${WEB}/`);
  who = await me();
  check(who.body?.provider?.kind === "ldap" && roles(who.body) === "viewer", "dave signs in over LDAPS, a viewer through a nested group");
  ctl("ldap-member", "add", "mf-operators", "uid=dave,ou=people,dc=example,dc=org");
  await fresh();
  await passwordSignIn("dave", "dave-directory-pw");
  await page.waitForURL(`${WEB}/`);
  check(roles((await me()).body) === "operator,viewer", "added to mf-operators, dave is an operator too");
  ctl("ldap-member", "delete", "mf-operators", "uid=dave,ou=people,dc=example,dc=org");
  ctl("ldap-member", "delete", "helpdesk", "uid=dave,ou=people,dc=example,dc=org");
  await fresh();
  await passwordSignIn("dave", "dave-directory-pw");
  await page.getByText("Your account is in none of the groups that give access to MonitorFlow.").waitFor();
  check((await me()).status === 401, "out of every mapped group, dave is refused with the reason");
  await passwordSignIn("dave", "wrong-password");
  await page.getByText("Wrong username or password.").waitFor();
  check(true, "a wrong directory password is a wrong password");

  // 7. The same directory over StartTLS.
  await signInAsRoot();
  const startTls = { ...ldaps, url: "ldap://localhost:28389", starttls: true };
  const moved = await call("PUT", `/api/identity-providers/${directory.body.id}`, {
    name: "Directory",
    enabled: true,
    secret: env("MF_SSO_LDAP_PASSWORD"),
    settings: startTls,
  });
  check(moved.status === 200, "the directory passes its test over StartTLS");
  await fresh();
  await passwordSignIn("erin", "erin-directory-pw");
  await page.waitForURL(`${WEB}/`);
  check(roles((await me()).body) === "operator", "erin signs in over StartTLS as an operator");

  // 8. Keycloak down: its button leads to a provider that does not answer,
  // and the local administrator still gets in with a password.
  ctl("stop", "keycloak");
  await fresh();
  await page.goto(`${WEB}/login/`);
  await page.getByRole("link", { name: "Sign in with Keycloak" }).click().catch(() => {});
  await page.waitForURL((url) => url.port === "28543" || url.pathname === "/login/", { timeout: 30_000 }).catch(() => {});
  check(page.url() !== `${WEB}/`, "with Keycloak down, nobody gets in through it");
  await passwordSignIn("root", ROOT_PASSWORD);
  await page.waitForURL(`${WEB}/`);
  check((await me()).body?.username === "root", "the local administrator signs in during the outage");
} catch (error) {
  const shot = path.join(process.env.MF_SSO_ARTIFACTS ?? ".", "sso-test-failure.png");
  if (page) await page.screenshot({ path: shot, fullPage: true }).catch(() => {});
  console.error(error);
  await browser.close();
  process.exit(1);
}
await browser.close();
