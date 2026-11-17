import { render, screen, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { beforeEach, describe, expect, it, vi } from "vitest";

import { setActiveLocale } from "@/i18n/locale";
import type { ImportItem, ImportRun } from "@/lib/api";
import { As, me, oneSite, viewer } from "@/test/admin";
import { stubFetch } from "@/test/fetch";
import { setUrl } from "@/test/navigation";

import { ImportPage } from "./ImportPage";

vi.mock("next/navigation", () => import("@/test/navigation"));

const admin = oneSite(me({ role: "administrator", customer_id: null, site_id: null }));

const SOURCES = {
  sources: [
    {
      id: "example",
      name: "Example tool",
      extensions: ["yaml"],
      many_files: true,
      connect: [
        { key: "url", secret: false, optional: false, placeholder: "https://monitoring.example.com" },
        { key: "token", secret: true, optional: false, placeholder: "" },
      ],
    },
  ],
};

const SITES = { sites: [{ id: "site-1", name: "paris", customer_id: "c1", customer_name: "acme" }] };

const items: ImportItem[] = [
  {
    id: "server:web-01",
    kind: "server",
    list: "exact",
    origin: { kind: "host", name: "web-01" },
    hosts: ["web-01"],
    present: false,
    needs: [],
    create: { object: "server", peer_id: "web-01", hostname: "web-01", os_family: "debian", addresses: [] },
  },
  {
    id: "check:web-01:service_nginx",
    kind: "check",
    list: "exact",
    origin: { kind: "item", name: "nginx running" },
    hosts: ["web-01"],
    present: false,
    needs: ["server:web-01"],
    create: { object: "check", server: "web-01", kind: "systemd", name: "service_nginx", interval_ms: 60000, params: { units: "nginx" } },
  },
  {
    id: "check:web-01:port_shop_443",
    kind: "check",
    list: "approximated",
    note: { code: "web_as_port", args: { port: "443", host: "shop" }, text: "english" },
    origin: { kind: "web_scenario", name: "Shop" },
    hosts: ["web-01"],
    present: false,
    needs: ["server:web-01"],
    create: { object: "check", server: "web-01", kind: "port", name: "port_shop_443", interval_ms: 60000, params: { port: "443", host: "shop" } },
  },
  {
    id: "other:item:traffic",
    kind: "other",
    list: "not_movable",
    note: { code: "network_traffic", text: "english" },
    origin: { kind: "item", name: "Bits received", detail: "net.if.in[eth0]" },
    hosts: ["web-01"],
    present: false,
    needs: [],
  },
];

function run(overrides: Partial<ImportRun> = {}): ImportRun {
  return {
    id: "r1",
    source: "example",
    source_name: "Example tool",
    site_id: "site-1",
    site_name: "paris",
    input: "export.yaml",
    state: "preview",
    created_at: "2026-11-17T09:00:00Z",
    created_by: "alice",
    expires_at: "2026-11-18T09:00:00Z",
    confirmed_at: null,
    undone_at: null,
    counts: { exact: 2, approximated: 1, not_movable: 1, present: 0, to_create: 3 },
    items,
    result: null,
    ...overrides,
  };
}

function renderPage(user = admin) {
  return render(
    <As user={user}>
      <ImportPage />
    </As>,
  );
}

describe("ImportPage", () => {
  beforeEach(() => {
    setActiveLocale("en");
    setUrl("/settings/import/");
  });

  it("is for administrators only", () => {
    stubFetch({});
    renderPage(viewer);
    expect(screen.getByText("Administrators only")).toBeInTheDocument();
  });

  it("connects with a token it does not keep, and opens the preview", async () => {
    const fetchMock = stubFetch({
      "GET /api/imports/sources": { status: 200, body: SOURCES },
      "GET /api/sites": { status: 200, body: SITES },
      "GET /api/imports": { status: 200, body: { imports: [] } },
      "POST /api/imports": { status: 201, body: run() },
      "GET /api/imports/r1": { status: 200, body: run() },
    });
    renderPage();
    await userEvent.click(await screen.findByLabelText("Connect to its API"));
    const read = screen.getByRole("button", { name: "Read and preview" });
    expect(read).toBeDisabled();
    await userEvent.type(screen.getByLabelText("Address"), "https://monitoring.example.com");
    await userEvent.type(screen.getByLabelText("API token"), "s3cret");
    expect(screen.getByLabelText("API token")).toHaveAttribute("type", "password");
    await userEvent.click(read);
    const post = fetchMock.mock.calls.find(([, init]) => init?.method === "POST");
    // One site: the server picks it.
    expect(JSON.parse(String(post?.[1]?.body))).toEqual({
      source: "example",
      connection: { url: "https://monitoring.example.com", token: "s3cret" },
    });
    expect(await screen.findByText("Example tool: export.yaml")).toBeInTheDocument();
  });

  it("shows the three lists, leaves out what needs an unticked server, and confirms the rest", async () => {
    setUrl("/settings/import/?id=r1");
    const fetchMock = stubFetch({
      "GET /api/imports/r1": { status: 200, body: run() },
      "POST /api/imports/r1/confirm": {
        status: 200,
        body: run({
          state: "confirmed",
          expires_at: null,
          confirmed_at: "2026-11-17T09:05:00Z",
          result: { created: {}, skipped: 3, present: 0, failed: [], removed: {}, kept_servers: [] },
        }),
      },
    });
    renderPage();
    expect(await screen.findByRole("heading", { name: "Same in MonitorFlow" })).toBeInTheDocument();
    expect(screen.getByRole("heading", { name: "Approximated" })).toBeInTheDocument();
    expect(screen.getByRole("heading", { name: "Cannot be moved" })).toBeInTheDocument();
    expect(screen.getByText("MonitorFlow checks that port 443 of shop answers; it does not read the page or its status code.")).toBeInTheDocument();
    expect(screen.getByText("MonitorFlow does not measure network interface traffic yet.")).toBeInTheDocument();
    expect(screen.getByText("net.if.in[eth0]")).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "Create 3 items" })).toBeEnabled();

    await userEvent.click(screen.getByRole("checkbox", { name: "web-01" }));
    const nginx = screen.getByRole("checkbox", { name: "Service nginx" });
    expect(nginx).toBeDisabled();
    expect(nginx).not.toBeChecked();
    expect(screen.getByRole("button", { name: "Create 0 items" })).toBeDisabled();

    await userEvent.click(screen.getByRole("checkbox", { name: "web-01" }));
    await userEvent.click(screen.getByRole("checkbox", { name: "Service nginx" }));
    await userEvent.click(screen.getByRole("button", { name: "Create 2 items" }));
    const post = fetchMock.mock.calls.find(([, init]) => init?.method === "POST");
    expect(JSON.parse(String(post?.[1]?.body))).toEqual({ skip: ["check:web-01:service_nginx"] });
    expect(await screen.findByText("Imported")).toBeInTheDocument();
  });

  it("says what a confirmation created, and undoes it", async () => {
    setUrl("/settings/import/?id=r1");
    const result = { created: { server: 1, check: 2 }, skipped: 0, present: 1, failed: [], removed: {}, kept_servers: [] };
    stubFetch({
      "GET /api/imports/r1": { status: 200, body: run({ state: "confirmed", expires_at: null, result }) },
      "POST /api/imports/r1/undo": {
        status: 200,
        body: run({ state: "undone", expires_at: null, result: { ...result, removed: { check: 2 }, kept_servers: ["web-01"] } }),
      },
    });
    renderPage();
    expect(await screen.findByText(/Created 1 server, 2 checks\./)).toBeInTheDocument();
    expect(screen.getByText(/The server waits for its agent/)).toBeInTheDocument();
    await userEvent.click(screen.getByRole("button", { name: "Undo this import…" }));
    await userEvent.click(screen.getByRole("button", { name: "Undo the import" }));
    expect(await screen.findByText(/Removed 2 checks\./)).toBeInTheDocument();
    expect(screen.getByText(/Kept, as their agent took them over: web-01\./)).toBeInTheDocument();
  });

  it("speaks French", async () => {
    setActiveLocale("fr");
    setUrl("/settings/import/?id=r1");
    stubFetch({ "GET /api/imports/r1": { status: 200, body: run() } });
    renderPage();
    expect(await screen.findByRole("heading", { name: "Pareil dans MonitorFlow" })).toBeInTheDocument();
    const list = screen.getByRole("heading", { name: "Ne peut pas être repris" }).closest("section")!;
    expect(within(list).getByText("MonitorFlow ne mesure pas encore le trafic des interfaces réseau.")).toBeInTheDocument();
  });
});
