import { render, screen, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { describe, expect, it, vi } from "vitest";

import { setActiveLocale } from "@/i18n/locale";
import type { CheckRecommendation } from "@/lib/api";
import { stubFetch } from "@/test/fetch";

import { CheckRecommendations } from "./CheckRecommendations";

vi.mock("next/navigation", () => import("@/test/navigation"));

const ROUTE = "GET /api/servers/srv-1/recommendations";
const postgres: CheckRecommendation = {
  key: "postgresql",
  title: "PostgreSQL is running",
  evidence: ["postgresql@16-main.service is running", "port 5432 is listening"],
  checks: [
    { kind: "systemd", params: { units: "postgresql@16-main.service" } },
    { kind: "port", params: { port: "5432", host: "127.0.0.1" } },
  ],
  dismissed_by: null,
  dismissed_at: null,
};
const share: CheckRecommendation = {
  key: "share:/mnt/backup",
  title: "/mnt/backup is a share on nas.lan",
  evidence: ["/mnt/backup is mounted from nas.lan (nfs4)"],
  checks: [{ kind: "port", params: { port: "2049", host: "nas.lan" } }],
  dismissed_by: "alice",
  dismissed_at: new Date().toISOString(),
};

const bodies = (fetchMock: ReturnType<typeof stubFetch>, key: string) =>
  fetchMock.mock.calls
    .filter(([input, init]) => `${init?.method ?? "GET"} ${String(input)}` === key)
    .map(([, init]) => JSON.parse(String(init?.body)));

const props = { serverId: "srv-1", osFamily: "debian", intervalMs: 60_000, onAdded: vi.fn() };

describe("CheckRecommendations", () => {
  it("adds every proposed check with its alert in one click", async () => {
    const user = userEvent.setup();
    const onAdded = vi.fn();
    const fetchMock = stubFetch({
      [ROUTE]: [
        { status: 200, body: { reported_at: new Date().toISOString(), recommendations: [postgres, share] } },
        { status: 200, body: { reported_at: new Date().toISOString(), recommendations: [share] } },
      ],
      "POST /api/servers/srv-1/sensor_configs": [{ status: 201, body: {} }, { status: 409, body: { error: "exists" } }],
      "GET /api/servers/srv-1/thresholds": { status: 200, body: { entries: [] } },
      "POST /api/servers/srv-1/thresholds": { status: 201, body: {} },
    });
    render(<CheckRecommendations {...props} canEdit onAdded={onAdded} />);

    const box = await screen.findByRole("region", { name: "Recommended checks" });
    expect(box).toHaveTextContent("From what the agent saw");
    expect(box).toHaveTextContent("postgresql@16-main.service is running, port 5432 is listening.");
    expect(box).toHaveTextContent("Adds: Service postgresql@16-main.service, Port 5432, each with its alert.");
    expect(box).not.toHaveTextContent("Port 2049 on nas.lan");
    // The dismissed one waits behind its button.
    expect(box).not.toHaveTextContent("/mnt/backup");

    await user.click(within(box).getByRole("button", { name: "Add 2 checks for PostgreSQL is running" }));
    await vi.waitFor(() => expect(onAdded).toHaveBeenCalled());
    expect(bodies(fetchMock, "POST /api/servers/srv-1/sensor_configs")).toEqual([
      { kind: "systemd", name: "service_postgresql_16_main", interval_ms: 60_000, params: { units: "postgresql@16-main.service" } },
      { kind: "port", name: "port_5432", interval_ms: 60_000, params: { port: "5432", host: "127.0.0.1" } },
    ]);
    // The port check existed already: only the service's alert is added.
    expect(bodies(fetchMock, "POST /api/servers/srv-1/thresholds")).toEqual([
      expect.objectContaining({ metric: "monitorflow_service_postgresql_16_main_active", critical: 1 }),
    ]);
    expect(await within(box).findByText("Nothing to add: what runs here is checked.")).toBeInTheDocument();
  });

  it("speaks French, leaving the agent's findings as they came", async () => {
    setActiveLocale("fr");
    stubFetch({ [ROUTE]: { status: 200, body: { reported_at: new Date().toISOString(), recommendations: [postgres, share] } } });
    render(<CheckRecommendations {...props} canEdit />);
    expect(await screen.findByRole("heading", { name: "Contrôles recommandés" })).toBeInTheDocument();
    expect(screen.getByText("PostgreSQL is running")).toBeInTheDocument();
    expect(screen.getByText("Ajoute : Service postgresql@16-main.service, Port 5432, chacun avec son alerte.")).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "Ajouter 2 contrôles pour PostgreSQL is running" })).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "Afficher les écartés (1)" })).toBeInTheDocument();
  });

  it("dismisses, shows dismissed ones and brings them back", async () => {
    const user = userEvent.setup();
    const fetchMock = stubFetch({
      [ROUTE]: { status: 200, body: { reported_at: new Date().toISOString(), recommendations: [postgres, share] } },
      "POST /api/servers/srv-1/recommendations/dismiss": { status: 204 },
      "POST /api/servers/srv-1/recommendations/restore": { status: 204 },
    });
    render(<CheckRecommendations {...props} canEdit />);

    await user.click(await screen.findByRole("button", { name: "Dismiss PostgreSQL is running" }));
    await user.click(screen.getByRole("button", { name: "Show dismissed (1)" }));
    expect(screen.getByText("Dismissed by alice.")).toBeInTheDocument();
    expect(screen.queryByText(/each with its alert/)).toHaveTextContent("Adds: Service postgresql@16-main.service, Port 5432, each with its alert.");
    await user.click(screen.getByRole("button", { name: "Bring back" }));
    await vi.waitFor(() => expect(bodies(fetchMock, "POST /api/servers/srv-1/recommendations/restore")).toEqual([{ key: "share:/mnt/backup" }]));
    expect(bodies(fetchMock, "POST /api/servers/srv-1/recommendations/dismiss")).toEqual([{ key: "postgresql" }]);
  });

  it("says why there is nothing, and shows no buttons to viewers", async () => {
    stubFetch({ [ROUTE]: { status: 200, body: { reported_at: null, recommendations: [] } } });
    const { unmount } = render(<CheckRecommendations {...props} osFamily="windows" canEdit />);
    expect(await screen.findByText("Windows agents do not report what runs on their server yet.")).toBeInTheDocument();
    unmount();

    stubFetch({ [ROUTE]: { status: 200, body: { reported_at: new Date().toISOString(), recommendations: [postgres] } } });
    render(<CheckRecommendations {...props} canEdit={false} />);
    expect(await screen.findByText("PostgreSQL is running")).toBeInTheDocument();
    expect(screen.queryByRole("button")).not.toBeInTheDocument();
  });
});
