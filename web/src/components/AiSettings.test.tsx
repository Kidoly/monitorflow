import { render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { beforeEach, describe, expect, it, vi } from "vitest";

import type { AiSettings } from "@/lib/api";
import { As, customerAdmin, me } from "@/test/admin";
import { stubFetch } from "@/test/fetch";

import { AiSettingsPage } from "./AiSettings";

vi.mock("next/navigation", () => import("@/test/navigation"));

const globalAdmin = me({ role: "administrator", customer_id: null, site_id: null });
const AI = "/api/settings/ai";
const GITHUB = "https://github.com/Kidoly/monitorflow/releases/latest/download";
const install = {
  status: 200,
  body: { source: "github", custom_url: null, source_from_config: false, custom_url_from_config: false, github_url: GITHUB, central: null },
};

const address = {
  status: 200,
  body: { address: "mf.example", default: "mf.example", names: ["mf.example"], certified: true, upstream_url: "https://mf.example:7443", enroll_url: "https://mf.example:7445" },
};

function settings(overrides: Partial<AiSettings> = {}): AiSettings {
  return {
    enabled: false,
    from_config: false,
    has_key: false,
    model: "claude-opus-5-5",
    models: [
      { id: "claude-opus-5-5", name: "Claude Opus 5.5", note: "Recommended: the best diagnoses" },
      { id: "claude-sonnet-5-5", name: "Claude Sonnet 5.5", note: "Half the price, for most incidents" },
    ],
    ...overrides,
  };
}

const ok = (body: AiSettings) => ({ status: 200, body });
const page = () =>
  render(
    <As user={globalAdmin}>
      <AiSettingsPage />
    </As>,
  );
const sent = (fetchMock: ReturnType<typeof stubFetch>, method: string) =>
  JSON.parse(String(fetchMock.mock.calls.find(([, init]) => init?.method === method)?.[1]?.body));

describe("AiSettingsPage", () => {
  beforeEach(() => vi.unstubAllGlobals());

  it("is for global administrators only", () => {
    render(
      <As user={customerAdmin}>
        <AiSettingsPage />
      </As>,
    );
    expect(screen.getByText("AI setup needs a global administrator")).toBeInTheDocument();
  });

  it("walks through key, test and enable, then shows the command for servers", async () => {
    const user = userEvent.setup();
    const fetchMock = stubFetch({
      [`GET ${AI}`]: ok(settings()),
      [`POST ${AI}/test`]: [
        { status: 200, body: { ok: false, error: "Anthropic refused this API key." } },
        { status: 200, body: { ok: true, error: null } },
      ],
      [`PUT ${AI}`]: ok(settings({ enabled: true, has_key: true, model: "claude-sonnet-5-5" })),
      "GET /api/settings/agent-install": install,
      "GET /api/settings/public-address": address,
    });
    page();
    expect(await screen.findByText("AI is off.")).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "Enable AI" })).toBeDisabled();
    expect(screen.getByRole("button", { name: "Test" })).toBeDisabled();
    expect(screen.getByText("Recommended: the best diagnoses")).toBeInTheDocument();

    await user.type(screen.getByLabelText(/Anthropic API key/), "sk-ant-1");
    await user.selectOptions(screen.getByLabelText(/Model/), "claude-sonnet-5-5");
    await user.click(screen.getByRole("button", { name: "Test" }));
    expect(await screen.findByRole("alert")).toHaveTextContent("Anthropic refused this API key.");
    await user.click(screen.getByRole("button", { name: "Test" }));
    expect(await screen.findByText("The key and model answered.")).toBeInTheDocument();
    expect(sent(fetchMock, "POST")).toEqual({ api_key: "sk-ant-1", model: "claude-sonnet-5-5" });

    await user.click(screen.getByRole("button", { name: "Enable AI" }));
    expect(await screen.findByText(/with Claude Sonnet 5.5/)).toBeInTheDocument();
    expect(sent(fetchMock, "PUT")).toEqual({ api_key: "sk-ant-1", model: "claude-sonnet-5-5", enabled: true });
    expect(screen.getByLabelText(/Anthropic API key/)).toHaveValue("");
    expect(screen.getByLabelText(/Anthropic API key/)).toHaveAttribute("placeholder", "Saved; leave empty to keep it");
    expect(await screen.findByLabelText("Command that allows AI commands")).toHaveTextContent("sudo MF_ALLOW_COMMANDS=1 sh");
  });

  it("shows where each server stands, with a link to act on it", async () => {
    const server = (id: string, command_kinds: Record<string, string> | null) => ({ id, hostname: `web-${id}`, command_kinds });
    stubFetch({
      [`GET ${AI}`]: ok(settings({ enabled: true, has_key: true })),
      "GET /api/settings/agent-install": install,
      "GET /api/settings/public-address": address,
      "GET /api/servers": {
        status: 200,
        body: {
          servers: [
            server("1", { shell: "accepted" }),
            server("2", { shell: "off" }),
            server("3", null),
            server("4", { shell: "unsupported" }),
            server("5", { apply_updates: "accepted" }),
          ],
          next_cursor: null,
        },
      },
    });
    page();
    expect(await screen.findByText("1 of 5 servers accept AI commands.")).toBeInTheDocument();
    expect(screen.getByText(/^Off, run the line below/)).toHaveTextContent(/web-2$/);
    expect(screen.getByRole("link", { name: "web-2" })).toHaveAttribute("href", expect.stringContaining("id=2&tab=commands"));
    // Not reporting is its own group, even from an agent that reports other kinds.
    expect(screen.getByText(/^Not known yet/)).toHaveTextContent(/web-3, web-5$/);
    expect(screen.getByText(/^Their system does not run/)).toHaveTextContent(/web-4$/);
  });

  it("still gives the line when the servers cannot be read", async () => {
    stubFetch({
      [`GET ${AI}`]: ok(settings({ enabled: true, has_key: true })),
      "GET /api/settings/agent-install": install,
      "GET /api/settings/public-address": address,
      "GET /api/servers": { status: 500, body: { error: "database unavailable" } },
    });
    page();
    expect(await screen.findByLabelText("Command that allows AI commands")).toBeInTheDocument();
    expect(screen.queryByText(/accept AI commands\./)).not.toBeInTheDocument();
  });

  it("shows why enabling failed", async () => {
    const user = userEvent.setup();
    stubFetch({
      [`GET ${AI}`]: ok(settings({ has_key: true })),
      [`PUT ${AI}`]: { status: 400, body: { error: "This Anthropic account has no credit left." } },
    });
    page();
    await user.click(await screen.findByRole("button", { name: "Enable AI" }));
    expect(await screen.findByRole("alert")).toHaveTextContent("no credit left");
  });

  it("turns off and removes the key", async () => {
    const user = userEvent.setup();
    const fetchMock = stubFetch({
      [`GET ${AI}`]: ok(settings({ enabled: true, has_key: true })),
      [`PUT ${AI}`]: ok(settings({ has_key: true })),
      [`DELETE ${AI}`]: ok(settings()),
      "GET /api/settings/agent-install": install,
      "GET /api/settings/public-address": address,
    });
    page();
    await user.click(await screen.findByRole("button", { name: "Turn off AI" }));
    expect(await screen.findByText("AI is off.")).toBeInTheDocument();
    expect(sent(fetchMock, "PUT")).toEqual({ api_key: null, model: "claude-opus-5-5", enabled: false });

    await user.click(screen.getByRole("button", { name: "Remove key" }));
    await user.click(await screen.findByRole("button", { name: "Remove" }));
    expect(await screen.findByPlaceholderText("sk-ant-…")).toBeInTheDocument();
  });

  it("says the configuration takes precedence and offers only the test", async () => {
    const user = userEvent.setup();
    stubFetch({
      [`GET ${AI}`]: ok(settings({ enabled: true, from_config: true, has_key: true, model: "claude-custom" })),
      [`POST ${AI}/test`]: { status: 200, body: { ok: true, error: null } },
      "GET /api/settings/agent-install": install,
      "GET /api/settings/public-address": address,
    });
    page();
    expect(await screen.findByText(/takes precedence over this page/)).toBeInTheDocument();
    expect(screen.getByText(/with claude-custom/)).toBeInTheDocument();
    expect(screen.queryByLabelText(/Anthropic API key/)).not.toBeInTheDocument();
    expect(screen.queryByRole("button", { name: "Turn off AI" })).not.toBeInTheDocument();
    await user.click(screen.getByRole("button", { name: "Test" }));
    expect(await screen.findByText("The key and model answered.")).toBeInTheDocument();
  });
});
