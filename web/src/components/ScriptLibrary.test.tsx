import { render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, describe, expect, it, vi } from "vitest";

import type { LibraryScript } from "@/lib/api";
import { As, me, single } from "@/test/admin";
import { stubFetch } from "@/test/fetch";

import { describeEvent } from "./AutomationRules";
import { ScriptLibrary } from "./ScriptLibrary";

vi.mock("next/navigation", () => import("@/test/navigation"));

const script: LibraryScript = {
  id: "s1", customer_id: null, name: "clear-cache", description: "Frees /var/cache", interpreter: "sh",
  run_role: "operator", ai_allowed: true, timeout_s: 600, version: 3, parameters: [],
  created_at: "2026-12-01T09:00:00Z", updated_at: "2026-12-03T09:00:00Z", deleted_at: null, can_manage: true,
};

afterEach(() => vi.unstubAllGlobals());

describe("script library", () => {
  it("lists the scripts and adds one", async () => {
    const assign = vi.fn();
    vi.stubGlobal("location", { ...window.location, assign });
    const fetchMock = stubFetch({
      "GET /api/remediation/scripts": { status: 200, body: { scripts: [script] } },
      "GET /api/remediation/script-runs?limit=20": { status: 200, body: { runs: [] } },
      "GET /api/customers": { status: 200, body: { customers: [{ id: "c1", name: "acme", site_count: 1 }] } },
      "POST /api/remediation/scripts": { status: 201, body: { script: { ...script, id: "s2" }, body: "df -h", versions: [] } },
    });
    render(
      <As user={single(me({ role: "administrator", customer_id: null, site_id: null }))}>
        <ScriptLibrary />
      </As>,
    );
    expect(await screen.findByRole("link", { name: "clear-cache" })).toBeInTheDocument();
    expect(screen.getByText("v3")).toBeInTheDocument();
    await userEvent.click(screen.getAllByRole("button", { name: "New script" })[0]!);
    await userEvent.type(await screen.findByRole("textbox", { name: "Name" }), "disk-report");
    await userEvent.type(screen.getByRole("textbox", { name: /^Script/ }), "df -h");
    await userEvent.click(screen.getByRole("button", { name: "Add the script" }));
    const post = fetchMock.mock.calls.find((c) => c[1]?.method === "POST");
    expect(JSON.parse(String(post?.[1]?.body))).toMatchObject({ name: "disk-report", body: "df -h", run_role: "administrator", customer_id: null });
    expect(assign).toHaveBeenCalledWith("/remediation/scripts/view/?id=s2");
  });
});

describe("automation rules", () => {
  it("say when they run", () => {
    expect(describeEvent({ event: "incident_opened", metric: null })).toBe("When any incident opens");
    expect(describeEvent({ event: "check_failed", metric: "port_open" })).toBe("When the check port_open fails");
    expect(describeEvent({ event: "server_joined", metric: null })).toBe("When a server joins");
  });
});
