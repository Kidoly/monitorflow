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
    provider: "anthropic",
    has_key: false,
    base_url: null,
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
    expect(await screen.findByText("The model answered.")).toBeInTheDocument();
    expect(sent(fetchMock, "POST")).toEqual({ provider: "anthropic", api_key: "sk-ant-1", base_url: null, model: "claude-sonnet-5-5" });

    await user.click(screen.getByRole("button", { name: "Enable AI" }));
    expect(await screen.findByText(/with Claude Sonnet 5.5/)).toBeInTheDocument();
    expect(sent(fetchMock, "PUT")).toEqual({
      provider: "anthropic",
      api_key: "sk-ant-1",
      base_url: null,
      model: "claude-sonnet-5-5",
      enabled: true,
    });
    expect(screen.getByLabelText(/Anthropic API key/)).toHaveValue("");
    expect(screen.getByLabelText(/Anthropic API key/)).toHaveAttribute("placeholder", "Saved; leave empty to keep it");
    // AI commands are turned on from each server's page now (ADR-0098).
    expect(await screen.findByText(/Turn AI commands on or off from each server/)).toBeInTheDocument();
    expect(screen.queryByLabelText("Command that allows AI commands")).not.toBeInTheDocument();
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
    expect(screen.getByText(/^Off, turn them on from their page/)).toHaveTextContent(/web-2$/);
    expect(screen.getByRole("link", { name: "web-2" })).toHaveAttribute("href", expect.stringContaining("id=2&tab=commands"));
    // Not reporting is its own group, even from an agent that reports other kinds.
    expect(screen.getByText(/^Not known yet/)).toHaveTextContent(/web-3, web-5$/);
    expect(screen.getByText(/^Their agent is too old/)).toHaveTextContent(/web-4$/);
  });

  it("still says where to turn them on when the servers cannot be read", async () => {
    stubFetch({
      [`GET ${AI}`]: ok(settings({ enabled: true, has_key: true })),
      "GET /api/settings/agent-install": install,
      "GET /api/settings/public-address": address,
      "GET /api/servers": { status: 500, body: { error: "database unavailable" } },
    });
    page();
    expect(await screen.findByText(/Turn AI commands on or off from each server/)).toBeInTheDocument();
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
    expect(sent(fetchMock, "PUT")).toEqual({
      provider: "anthropic",
      api_key: null,
      base_url: null,
      model: "claude-opus-5-5",
      enabled: false,
    });

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
    expect(await screen.findByText("The model answered.")).toBeInTheDocument();
  });

  it("sets up a local model with an address, its listed models and no key", async () => {
    const user = userEvent.setup();
    const LOCAL = "http://ollama.lan:11434/v1";
    const fetchMock = stubFetch({
      [`GET ${AI}`]: ok(settings()),
      [`POST ${AI}/models`]: { status: 200, body: { models: ["llama3.1:8b", "qwen3:8b"], error: null } },
      [`POST ${AI}/test`]: { status: 200, body: { ok: true, error: null } },
      [`PUT ${AI}`]: ok(settings({ enabled: true, provider: "openai_compatible", base_url: LOCAL, model: "llama3.1:8b" })),
      "GET /api/settings/agent-install": install,
      "GET /api/settings/public-address": address,
    });
    page();
    await user.click(await screen.findByLabelText("Another or a local model"));
    expect(screen.getByText(/nothing leaves it/)).toBeInTheDocument();
    expect(screen.getByText(/Small local models diagnose less well/)).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "Enable AI" })).toBeDisabled();

    await user.type(screen.getByLabelText(/^Address/), LOCAL);
    expect(screen.getByText(/Sent unencrypted over your network/)).toBeInTheDocument();
    await user.tab();
    expect(await screen.findByText("2 models on this server: pick one or type its name.")).toBeInTheDocument();
    // The first listed model fills an empty field.
    expect(screen.getByLabelText(/^Model/)).toHaveValue("llama3.1:8b");
    expect(screen.getByLabelText(/only if the server asks/)).toHaveAttribute("placeholder", "Usually none for a local model");

    await user.click(screen.getByRole("button", { name: "Test" }));
    expect(await screen.findByText("The model answered.")).toBeInTheDocument();
    await user.click(screen.getByRole("button", { name: "Enable AI" }));
    expect(await screen.findByText(/with llama3.1:8b on ollama.lan:11434/)).toBeInTheDocument();
    expect(sent(fetchMock, "PUT")).toEqual({
      provider: "openai_compatible",
      api_key: null,
      base_url: LOCAL,
      model: "llama3.1:8b",
      enabled: true,
    });
  });

  it("asks for the key again when a local model's address changes", async () => {
    const user = userEvent.setup();
    const LOCAL = "http://ollama.lan:11434/v1";
    stubFetch({
      [`GET ${AI}`]: ok(settings({ provider: "openai_compatible", has_key: true, base_url: LOCAL, model: "qwen3:8b" })),
      [`POST ${AI}/models`]: { status: 200, body: { models: ["qwen3:8b"], error: null } },
    });
    page();
    const key = await screen.findByLabelText(/only if the server asks/);
    expect(key).toHaveAttribute("placeholder", "Saved; leave empty to keep it");
    const addressField = screen.getByLabelText(/^Address/);
    // A trailing slash is the same address.
    await user.type(addressField, "/");
    expect(key).toHaveAttribute("placeholder", "Saved; leave empty to keep it");
    await user.clear(addressField);
    await user.type(addressField, "https://elsewhere.example/v1");
    expect(key).toHaveAttribute("placeholder", "Enter it again for the new address");
  });

  it("asks OpenAI for a key, and does not reuse another provider's", async () => {
    const user = userEvent.setup();
    const fetchMock = stubFetch({
      [`GET ${AI}`]: ok(settings({ has_key: true })),
      [`POST ${AI}/models`]: { status: 200, body: { models: [], error: "OpenAI refused this API key." } },
    });
    page();
    await user.click(await screen.findByLabelText("OpenAI"));
    const key = screen.getByLabelText(/OpenAI API key/);
    expect(key).toHaveAttribute("placeholder", "sk-…");
    expect(screen.getByLabelText(/^Model/)).toHaveValue("gpt-5.1");
    expect(screen.getByRole("button", { name: "Enable AI" })).toBeDisabled();
    await user.type(key, "sk-bad");
    await user.tab();
    expect(await screen.findByText(/Could not list the models, type the name instead: OpenAI refused/)).toBeInTheDocument();
    expect(sent(fetchMock, "POST")).toEqual({ provider: "openai", api_key: "sk-bad", base_url: null });
  });
});
