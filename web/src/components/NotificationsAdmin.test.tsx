import { render, screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { beforeEach, describe, expect, it, vi } from "vitest";

import type { ChatKind, NotificationChannel } from "@/lib/api";
import { As, customerAdmin, me, single, siteAdmin, viewer } from "@/test/admin";
import { stubFetch } from "@/test/fetch";

import {
  NotificationsPage,
  destination,
  newSecret,
  pluginAllows,
} from "./NotificationsAdmin";

vi.mock("next/navigation", () => import("@/test/navigation"));

const globalAdmin = me({
  role: "administrator",
  customer_id: null,
  site_id: null,
});
const CHANNELS = "/api/notifications/channels";
const customers = {
  status: 200,
  body: { customers: [{ id: "c1", name: "default", site_count: 1 }] },
};
const noSites = { status: 200, body: { sites: [] } };
const site = (
  id: string,
  name: string,
  customer_id: string,
  customer_name: string,
) => ({
  id,
  name,
  customer_id,
  customer_name,
});
const twoCustomers = {
  status: 200,
  body: {
    customers: [
      { id: "c1", name: "acme", site_count: 2 },
      { id: "c2", name: "globex", site_count: 1 },
    ],
  },
};
const threeSites = {
  status: 200,
  body: {
    sites: [
      site("site-1", "paris", "c1", "acme"),
      site("site-2", "lyon", "c1", "acme"),
      site("site-3", "berlin", "c2", "globex"),
    ],
  },
};

function hook(
  overrides: Partial<NotificationChannel> = {},
): NotificationChannel {
  return {
    id: "n1",
    kind: "webhook",
    settings: { origin: "https://hooks.slack.com" },
    has_secret: true,
    customer_id: null,
    customer_name: null,
    site_id: null,
    site_name: null,
    created_at: "2026-10-29T10:00:00Z",
    last_sent_at: null,
    last_error: null,
    last_error_at: null,
    ...overrides,
  } as NotificationChannel;
}

function chat(
  overrides: Partial<NotificationChannel> = {},
): NotificationChannel {
  return hook({
    id: "n5",
    kind: "slack",
    settings: { name: "#ops", host: "hooks.slack.com" },
    ...overrides,
  } as Partial<NotificationChannel>);
}

const page = (user = globalAdmin) =>
  render(
    <As user={user}>
      <NotificationsPage />
    </As>,
  );

const posted = (fetchMock: ReturnType<typeof stubFetch>) =>
  JSON.parse(
    String(
      fetchMock.mock.calls.find(
        ([url, init]) => url === CHANNELS && init?.method === "POST",
      )?.[1]?.body,
    ),
  );

describe("NotificationsPage", () => {
  beforeEach(() => vi.unstubAllGlobals());

  it("is for administrators", () => {
    page(viewer);
    expect(
      screen.getByText("Notifications need an administrator"),
    ).toBeInTheDocument();
  });

  it("starts with the next action when nothing is set up", async () => {
    stubFetch({
      [`GET ${CHANNELS}`]: { status: 200, body: { channels: [] } },
      "GET /api/customers": customers,
    });
    page();
    expect(
      await screen.findByText("No one is notified yet"),
    ).toBeInTheDocument();
    expect(
      screen.getByRole("button", { name: "Add email" }),
    ).toBeInTheDocument();
    expect(
      screen.getByRole("button", { name: "Add webhook" }),
    ).toBeInTheDocument();
    for (const name of ["Add Slack", "Add Teams", "Add Discord"]) {
      expect(screen.getByRole("button", { name })).toBeInTheDocument();
    }
  });

  it.each([
    [
      "Slack",
      "slack",
      "https://hooks.slack.com/services/T0/B0/X",
      "Incoming Webhooks",
    ],
    [
      "Teams",
      "teams",
      "https://a.api.powerplatform.com/w?sig=x",
      "Send webhook alerts to a channel",
    ],
    [
      "Discord",
      "discord",
      "https://discord.com/api/webhooks/1/x",
      "Copy Webhook URL",
    ],
  ])(
    "adds a %s channel from its URL, then tests it",
    async (label, kind, url, help) => {
      const settings = { name: "", host: new URL(url).host };
      const saved = chat({
        id: "n4",
        kind: kind as ChatKind,
        settings,
      } as Partial<NotificationChannel>);
      const fetchMock = stubFetch({
        [`GET ${CHANNELS}`]: [
          { status: 200, body: { channels: [] } },
          { status: 200, body: { channels: [saved] } },
        ],
        "GET /api/customers": customers,
        [`POST ${CHANNELS}`]: { status: 201, body: saved },
        [`POST ${CHANNELS}/n4/test`]: {
          status: 200,
          body: { ok: true, error: null },
        },
      });
      const user = userEvent.setup();
      page();
      await user.click(
        await screen.findByRole("button", { name: `Add ${label}` }),
      );
      const input = screen.getByLabelText(/^Webhook URL/);
      expect(input).toBeRequired();
      expect(input).toHaveAccessibleName(expect.stringContaining(help));
      await user.type(input, url);
      await user.click(
        screen.getByRole("button", { name: "Save and send test" }),
      );

      expect(await screen.findByText("Test sent")).toBeInTheDocument();
      expect(screen.getByText("Check that it arrived.")).toBeInTheDocument();
      const body = JSON.parse(
        String(
          fetchMock.mock.calls.find(
            ([u, init]) => u === CHANNELS && init?.method === "POST",
          )?.[1]?.body,
        ),
      );
      expect(body).toEqual({
        kind,
        settings: { name: "" },
        secret: url,
        customer_id: null,
        site_id: null,
        follow_up_only: false,
      });
      expect(
        screen.getByText(`${label}: ${new URL(url).host}`),
      ).toBeInTheDocument();
    },
  );

  it("shows why the server refused a URL", async () => {
    stubFetch({
      [`GET ${CHANNELS}`]: { status: 200, body: { channels: [] } },
      "GET /api/customers": customers,
      [`POST ${CHANNELS}`]: {
        status: 400,
        body: { error: "This is an old Teams connector URL" },
      },
    });
    const user = userEvent.setup();
    page();
    await user.click(await screen.findByRole("button", { name: "Add Teams" }));
    await user.type(
      screen.getByLabelText(/^Webhook URL/),
      "https://a.webhook.office.com/x",
    );
    await user.click(
      screen.getByRole("button", { name: "Save and send test" }),
    );
    expect(await screen.findByRole("alert")).toHaveTextContent(
      "old Teams connector URL",
    );
    expect(
      screen.getByRole("heading", { name: "Add Teams" }),
    ).toBeInTheDocument();
  });

  it("keeps a chat channel's URL unless a new one is pasted, and never shows it", async () => {
    const saved = chat();
    const fetchMock = stubFetch({
      [`GET ${CHANNELS}`]: { status: 200, body: { channels: [saved] } },
      "GET /api/customers": customers,
      [`PUT ${CHANNELS}/n5`]: { status: 200, body: saved },
      [`POST ${CHANNELS}/n5/test`]: {
        status: 200,
        body: { ok: true, error: null },
      },
    });
    const user = userEvent.setup();
    page();
    await user.click(await screen.findByRole("button", { name: "Edit" }));
    const input = screen.getByLabelText(/^Webhook URL/);
    expect(input).toHaveValue("");
    expect(input).not.toBeRequired();
    expect(input).toHaveAttribute(
      "placeholder",
      "Saved; leave empty to keep it",
    );
    const name = screen.getByLabelText(/^Name/);
    expect(name).toHaveValue("#ops");
    await user.clear(name);
    await user.type(name, "#alerts");
    await user.click(
      screen.getByRole("button", { name: "Save and send test" }),
    );
    await screen.findByText("Test sent");
    const put = fetchMock.mock.calls.find(([, init]) => init?.method === "PUT");
    expect(JSON.parse(String(put?.[1]?.body))).toEqual({
      kind: "slack",
      settings: { name: "#alerts" },
      secret: null,
      customer_id: null,
      site_id: null,
      follow_up_only: false,
    });
  });

  it("saves an email channel, then sends a test and shows its result", async () => {
    const saved = {
      ...hook(),
      id: "n2",
      kind: "email",
      settings: {
        host: "smtp.example.com",
        port: 587,
        username: "ops@example.com",
        from: "",
        to: ["a@example.com"],
      },
    } as NotificationChannel;
    const fetchMock = stubFetch({
      [`GET ${CHANNELS}`]: [
        { status: 200, body: { channels: [] } },
        { status: 200, body: { channels: [saved] } },
      ],
      "GET /api/customers": customers,
      "GET /api/sites": noSites,
      [`POST ${CHANNELS}`]: { status: 201, body: saved },
      [`POST ${CHANNELS}/n2/test`]: {
        status: 200,
        body: {
          ok: false,
          error:
            "smtp.example.com:587 refused the username or password (535 bad credentials).",
        },
      },
    });
    const user = userEvent.setup();
    page();
    await user.click(await screen.findByRole("button", { name: "Add email" }));
    await user.type(screen.getByLabelText("Send to"), "a@example.com");
    await user.type(screen.getByLabelText("Mail server"), "smtp.example.com");
    await user.type(screen.getByLabelText("Username"), "ops@example.com");
    await user.type(screen.getByLabelText(/^Password/), "hunter2");
    await user.click(
      screen.getByRole("button", { name: "Save and send test" }),
    );

    expect(await screen.findByRole("alert")).toHaveTextContent(
      "refused the username or password",
    );
    expect(posted(fetchMock)).toEqual({
      kind: "email",
      settings: {
        to: ["a@example.com"],
        host: "smtp.example.com",
        port: 587,
        username: "ops@example.com",
        from: "",
      },
      secret: "hunter2",
      customer_id: null,
      site_id: null,
      follow_up_only: false,
    });
    expect(
      within(screen.getByRole("table")).getByText(/Email to a@example.com/),
    ).toBeInTheDocument();
  });

  it("adds a channel of a plugin, with its settings and secret settings", async () => {
    const ntfy = {
      name: "ntfy",
      kind: "channel",
      version: "1.0.0",
      title: "ntfy",
      description: "Sends notifications to an ntfy topic.",
      capabilities: [],
      settings: [
        { key: "server", label: "Server", type: "url", required: true, default: "https://ntfy.sh" },
        { key: "topic", label: "Topic", type: "text", required: true },
        { key: "token", label: "Access token", type: "secret" },
      ],
      everywhere: true,
      customer_ids: [],
      site_ids: [],
    };
    const saved = {
      ...hook(),
      id: "n3",
      kind: "plugin",
      settings: { plugin: "ntfy", name: "", values: { server: "https://ntfy.sh", topic: "ops" } },
    } as NotificationChannel;
    const fetchMock = stubFetch({
      [`GET ${CHANNELS}`]: { status: 200, body: { channels: [] } },
      "GET /api/customers": customers,
      "GET /api/sites": noSites,
      "GET /api/plugins/usable": { status: 200, body: { plugins: [ntfy] } },
      [`POST ${CHANNELS}`]: { status: 201, body: saved },
      [`POST ${CHANNELS}/n3/test`]: { status: 200, body: { ok: true, error: null } },
    });
    const user = userEvent.setup();
    page();
    await user.click(await screen.findByRole("button", { name: "Add ntfy" }));
    expect(screen.getByLabelText("Server")).toHaveValue("https://ntfy.sh");
    await user.type(screen.getByLabelText("Topic"), "ops");
    await user.type(screen.getByLabelText(/^Access token/), "tk_1");
    await user.click(screen.getByRole("button", { name: "Save and send test" }));
    await waitFor(() =>
      expect(posted(fetchMock)).toEqual({
        kind: "plugin",
        settings: { plugin: "ntfy", name: "", values: { server: "https://ntfy.sh", topic: "ops" } },
        secret: JSON.stringify({ token: "tk_1" }),
        customer_id: null,
        site_id: null,
        follow_up_only: false,
      }),
    );
    expect(destination(saved)).toBe("ntfy");
  });

  it("shows each channel's state and tests it on demand", async () => {
    stubFetch({
      [`GET ${CHANNELS}`]: {
        status: 200,
        body: {
          channels: [
            hook(),
            hook({
              id: "n3",
              settings: { origin: "http://n8n.lan" },
              last_error: "Could not connect to n8n.lan",
            }),
          ],
        },
      },
      "GET /api/customers": customers,
      "GET /api/sites": noSites,
      [`POST ${CHANNELS}/n1/test`]: {
        status: 200,
        body: { ok: true, error: null },
      },
    });
    const user = userEvent.setup();
    page();
    expect(
      await screen.findByText("Could not connect to n8n.lan"),
    ).toBeInTheDocument();
    expect(screen.getByText("Nothing sent yet")).toBeInTheDocument();
    const row = screen.getByText("Webhook to hooks.slack.com").closest("tr")!;
    await user.click(within(row).getByRole("button", { name: "Send test" }));
    expect(await within(row).findByText("Test sent")).toBeInTheDocument();
  });

  it("keeps a saved secret unless a new one is typed, and generates one", async () => {
    const fetchMock = stubFetch({
      [`GET ${CHANNELS}`]: { status: 200, body: { channels: [hook()] } },
      "GET /api/customers": customers,
      "GET /api/sites": noSites,
      [`PUT ${CHANNELS}/n1`]: { status: 200, body: hook() },
      [`POST ${CHANNELS}/n1/test`]: {
        status: 200,
        body: { ok: true, error: null },
      },
    });
    const user = userEvent.setup();
    page();
    await user.click(await screen.findByRole("button", { name: "Edit" }));
    // The path of the URL is never shown: empty keeps it.
    const url = screen.getByLabelText(/^URL/);
    expect(url).toHaveValue("");
    expect(url).not.toBeRequired();
    expect(url).toHaveAttribute(
      "placeholder",
      "https://hooks.slack.com/…, saved; leave empty to keep it",
    );
    const secret = screen.getByLabelText(/Signing secret/);
    expect(secret).toHaveValue("");
    expect(secret).toHaveAttribute(
      "placeholder",
      "Saved; leave empty to keep it",
    );
    await user.click(
      screen.getByRole("button", { name: "Save and send test" }),
    );
    await screen.findByText("Test sent");
    const put = fetchMock.mock.calls.find(([, init]) => init?.method === "PUT");
    expect(JSON.parse(String(put?.[1]?.body))).toMatchObject({
      secret: null,
      settings: { url: "" },
    });

    await user.click(screen.getByRole("button", { name: "Edit" }));
    await user.click(screen.getByRole("button", { name: "Generate" }));
    expect(
      String(
        (screen.getByLabelText(/Signing secret/) as HTMLInputElement).value,
      ),
    ).toMatch(/^[0-9a-f]{64}$/);
  });
});

describe("Notify about", () => {
  beforeEach(() => vi.unstubAllGlobals());

  const empty = {
    [`GET ${CHANNELS}`]: { status: 200, body: { channels: [] } },
  };

  it("does not ask with one customer and one site", async () => {
    stubFetch({
      ...empty,
      "GET /api/customers": customers,
      "GET /api/sites": {
        status: 200,
        body: { sites: [site("site-1", "paris", "c1", "default")] },
      },
    });
    const user = userEvent.setup();
    page(single(globalAdmin));
    await user.click(
      await screen.findByRole("button", { name: "Add webhook" }),
    );
    await screen.findByLabelText(/^URL/);
    expect(screen.queryByLabelText(/Notify about/)).not.toBeInTheDocument();
  });

  it("offers sites, not customers, with one customer", async () => {
    stubFetch({
      ...empty,
      "GET /api/customers": customers,
      "GET /api/sites": {
        status: 200,
        body: {
          sites: [
            site("site-1", "paris", "c1", "default"),
            site("site-2", "lyon", "c1", "default"),
          ],
        },
      },
    });
    const user = userEvent.setup();
    page(single(globalAdmin));
    await user.click(
      await screen.findByRole("button", { name: "Add webhook" }),
    );
    const scope = await screen.findByRole("combobox", { name: /Notify about/ });
    await within(scope).findByRole("option", { name: "paris" });
    expect(
      within(scope)
        .getAllByRole("option")
        .map((o) => o.textContent),
    ).toEqual(["Everything", "paris", "lyon"]);
  });

  it("says what each choice means and saves a site", async () => {
    const saved = hook({
      id: "n2",
      site_id: "site-2",
      site_name: "lyon",
      customer_id: "c1",
      customer_name: "acme",
    });
    const fetchMock = stubFetch({
      [`GET ${CHANNELS}`]: [
        { status: 200, body: { channels: [] } },
        { status: 200, body: { channels: [saved] } },
      ],
      "GET /api/customers": twoCustomers,
      "GET /api/sites": threeSites,
      [`POST ${CHANNELS}`]: { status: 201, body: saved },
      [`POST ${CHANNELS}/n2/test`]: {
        status: 200,
        body: { ok: true, error: null },
      },
    });
    const user = userEvent.setup();
    page();
    await user.click(
      await screen.findByRole("button", { name: "Add webhook" }),
    );
    await user.type(screen.getByLabelText(/^URL/), "https://hooks.example/x");
    const scope = await screen.findByRole("combobox", { name: /Notify about/ });
    await within(scope).findByRole("option", { name: "globex / berlin" });
    expect(
      within(scope)
        .getAllByRole("option")
        .map((o) => o.textContent),
    ).toEqual([
      "Everything",
      "All of acme",
      "acme / paris",
      "acme / lyon",
      "All of globex",
      "globex / berlin",
    ]);
    expect(
      screen.getByText(/news about the central server/),
    ).toBeInTheDocument();
    await user.selectOptions(scope, "All of acme");
    expect(
      screen.getByText(
        "Alerts from acme and all its sites. Nothing from other customers.",
      ),
    ).toBeInTheDocument();
    await user.selectOptions(scope, "acme / lyon");
    expect(screen.getByText("Alerts from lyon only.")).toBeInTheDocument();
    await user.click(
      screen.getByRole("button", { name: "Save and send test" }),
    );
    await screen.findByText("Test sent");
    expect(posted(fetchMock)).toMatchObject({
      customer_id: null,
      site_id: "site-2",
    });
    expect(
      within(screen.getByRole("table")).getByText("acme / lyon"),
    ).toBeInTheDocument();
  });

  it("covers a site with a chat channel like any other", async () => {
    const saved = chat({
      id: "n6",
      kind: "discord",
      site_id: "site-2",
      site_name: "lyon",
      customer_id: "c1",
    } as Partial<NotificationChannel>);
    const fetchMock = stubFetch({
      [`GET ${CHANNELS}`]: [
        { status: 200, body: { channels: [] } },
        { status: 200, body: { channels: [saved] } },
      ],
      "GET /api/customers": twoCustomers,
      "GET /api/sites": threeSites,
      [`POST ${CHANNELS}`]: { status: 201, body: saved },
      [`POST ${CHANNELS}/n6/test`]: {
        status: 200,
        body: { ok: true, error: null },
      },
    });
    const user = userEvent.setup();
    page();
    await user.click(
      await screen.findByRole("button", { name: "Add Discord" }),
    );
    await user.type(
      screen.getByLabelText(/^Webhook URL/),
      "https://discord.com/api/webhooks/1/x",
    );
    const scope = await screen.findByRole("combobox", { name: /Notify about/ });
    await within(scope).findByRole("option", { name: "acme / lyon" });
    await user.selectOptions(scope, "acme / lyon");
    await user.click(
      screen.getByRole("button", { name: "Save and send test" }),
    );
    await screen.findByText("Test sent");
    expect(posted(fetchMock)).toEqual({
      kind: "discord",
      settings: { name: "" },
      secret: "https://discord.com/api/webhooks/1/x",
      customer_id: null,
      site_id: "site-2",
      follow_up_only: false,
    });
  });

  it("offers a customer administrator its own customer and sites, starting with the customer", async () => {
    const saved = hook({ id: "n2", customer_id: "c1", customer_name: "acme" });
    const fetchMock = stubFetch({
      ...empty,
      "GET /api/customers": twoCustomers,
      "GET /api/sites": threeSites,
      [`POST ${CHANNELS}`]: { status: 201, body: saved },
      [`POST ${CHANNELS}/n2/test`]: {
        status: 200,
        body: { ok: true, error: null },
      },
    });
    const user = userEvent.setup();
    page(customerAdmin);
    await user.click(
      await screen.findByRole("button", { name: "Add webhook" }),
    );
    await user.type(screen.getByLabelText(/^URL/), "https://hooks.example/x");
    const scope = await screen.findByRole("combobox", { name: /Notify about/ });
    await within(scope).findByRole("option", { name: "acme / paris" });
    expect(
      within(scope)
        .getAllByRole("option")
        .map((o) => o.textContent),
    ).toEqual(["All of acme", "acme / paris", "acme / lyon"]);
    await user.click(
      screen.getByRole("button", { name: "Save and send test" }),
    );
    await waitFor(() =>
      expect(posted(fetchMock)).toMatchObject({
        customer_id: "c1",
        site_id: null,
      }),
    );
  });

  it("gives a site administrator its site without asking", async () => {
    stubFetch({
      ...empty,
      "GET /api/customers": {
        status: 200,
        body: { customers: [{ id: "c1", name: "acme", site_count: 1 }] },
      },
      "GET /api/sites": {
        status: 200,
        body: { sites: [site("site-1", "paris", "c1", "acme")] },
      },
    });
    const user = userEvent.setup();
    page(siteAdmin);
    await user.click(
      await screen.findByRole("button", { name: "Add webhook" }),
    );
    expect(
      await screen.findByText("Alerts from paris only."),
    ).toBeInTheDocument();
    expect(screen.queryByRole("combobox")).not.toBeInTheDocument();
  });

  it("does not let a customer administrator save before knowing where", async () => {
    stubFetch({
      ...empty,
      "GET /api/customers": { status: 500, body: {} },
      "GET /api/sites": { status: 500, body: {} },
    });
    const user = userEvent.setup();
    page(customerAdmin);
    await user.click(
      await screen.findByRole("button", { name: "Add webhook" }),
    );
    expect(
      screen.getByRole("button", { name: "Save and send test" }),
    ).toBeDisabled();
  });

  it("keeps what an edited channel covers", async () => {
    const scoped = hook({ customer_id: "c2", customer_name: "globex" });
    const fetchMock = stubFetch({
      [`GET ${CHANNELS}`]: { status: 200, body: { channels: [scoped] } },
      "GET /api/customers": twoCustomers,
      "GET /api/sites": threeSites,
      [`PUT ${CHANNELS}/n1`]: { status: 200, body: scoped },
      [`POST ${CHANNELS}/n1/test`]: {
        status: 200,
        body: { ok: true, error: null },
      },
    });
    const user = userEvent.setup();
    page();
    await user.click(await screen.findByRole("button", { name: "Edit" }));
    expect(
      await screen.findByRole("combobox", { name: /Notify about/ }),
    ).toHaveDisplayValue("All of globex");
    await user.click(
      screen.getByRole("button", { name: "Save and send test" }),
    );
    await screen.findByText("Test sent");
    const put = fetchMock.mock.calls.find(([, init]) => init?.method === "PUT");
    expect(JSON.parse(String(put?.[1]?.body))).toMatchObject({
      customer_id: "c2",
      site_id: null,
    });
  });

  it("keeps a channel for follow-ups only, and says so in the list", async () => {
    const quiet = hook({ follow_up_only: true });
    const fetchMock = stubFetch({
      [`GET ${CHANNELS}`]: { status: 200, body: { channels: [quiet] } },
      "GET /api/customers": twoCustomers,
      "GET /api/sites": threeSites,
      [`PUT ${CHANNELS}/n1`]: { status: 200, body: { ...quiet, follow_up_only: false } },
      [`POST ${CHANNELS}/n1/test`]: { status: 200, body: { ok: true, error: null } },
    });
    const user = userEvent.setup();
    page();
    expect(await screen.findByText(/only for follow-ups/)).toBeInTheDocument();
    await user.click(screen.getByRole("button", { name: "Edit" }));
    const box = screen.getByRole("checkbox", { name: /Only for follow-ups/ });
    expect(box).toBeChecked();
    await user.click(box);
    await user.click(screen.getByRole("button", { name: "Save and send test" }));
    await screen.findByText("Test sent");
    const put = fetchMock.mock.calls.find(([, init]) => init?.method === "PUT");
    expect(JSON.parse(String(put?.[1]?.body))).toMatchObject({ follow_up_only: false });
  });
});

describe("destination", () => {
  it("names where a channel sends", () => {
    expect(destination(hook())).toBe("Webhook to hooks.slack.com");
    expect(destination(chat())).toBe("Slack: #ops");
    expect(
      destination(
        chat({
          settings: { name: "", host: "hooks.slack.com" },
        } as Partial<NotificationChannel>),
      ),
    ).toBe("Slack: hooks.slack.com");
    expect(newSecret()).not.toBe(newSecret());
  });
});

describe("pluginAllows", () => {
  const plugin = (over: object): Parameters<typeof pluginAllows>[0] => ({
    name: "p",
    kind: "channel",
    version: "1.0.0",
    title: "P",
    description: "",
    capabilities: [],
    settings: [],
    everywhere: false,
    customer_ids: [],
    site_ids: [],
    ...over,
  });
  const sites = [site("site-1", "paris", "c1", "acme")] as Parameters<typeof pluginAllows>[2];
  const everything = { customer_id: null, site_id: null };
  const acme = { customer_id: "c1", site_id: null };
  const paris = { customer_id: null, site_id: "site-1" };

  it("follows the plugin's places", () => {
    expect(pluginAllows(plugin({ everywhere: true }), everything, sites)).toBe(true);
    expect(pluginAllows(plugin({ customer_ids: ["c1"] }), everything, sites)).toBe(false);
    expect(pluginAllows(plugin({ customer_ids: ["c1"] }), acme, sites)).toBe(true);
    expect(pluginAllows(plugin({ customer_ids: ["c1"] }), paris, sites)).toBe(true);
    expect(pluginAllows(plugin({ site_ids: ["site-1"] }), acme, sites)).toBe(false);
    expect(pluginAllows(plugin({ site_ids: ["site-1"] }), paris, sites)).toBe(true);
  });
});
