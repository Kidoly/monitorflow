import { render, screen, within } from "@testing-library/react";
import { describe, expect, it } from "vitest";

import type { EffectiveSensors } from "@/lib/api";
import { stubFetch } from "@/test/fetch";

import { SensorConfigPanel } from "./SensorConfigPanel";

const ID = "6f1c2a52-3b5e-4c43-9a51-0e7f6d1f1a01";
const PATH = `GET /api/servers/${ID}/sensors`;

function config(overrides: Partial<EffectiveSensors>): EffectiveSensors {
  return {
    server_id: ID,
    site_id: "site-1",
    revision: 5,
    applied_revision: 5,
    applied_at: new Date().toISOString(),
    in_sync: true,
    sensors: [
      { origin: "default", entry_id: null, kind: "cpu", name: "cpu", instance: "", interval_ms: 60_000, timeout_ms: null, params: {} },
      {
        origin: "server",
        entry_id: "e1",
        kind: "disk",
        name: "disk_data",
        instance: "data",
        interval_ms: 30_000,
        timeout_ms: 5_000,
        params: { mounts_exclude: "/data/tmp" },
      },
      { origin: "site", entry_id: "e2", kind: "memory", name: "memory", instance: "", interval_ms: 120_000, timeout_ms: null, params: {} },
    ],
    ...overrides,
  };
}

describe("SensorConfigPanel", () => {
  it("lists the effective sensors with their origin", async () => {
    stubFetch({ [PATH]: { status: 200, body: config({}) } });
    render(<SensorConfigPanel serverId={ID} />);
    expect(screen.getByRole("status")).toHaveTextContent("Loading sensor configuration…");

    const rows = within(await screen.findByRole("table", { name: "Sensor configuration" })).getAllByRole("row");
    expect(rows).toHaveLength(4);
    expect(rows[1]).toHaveTextContent(/cpu\s*–\s*cpu\s*1 min\s*agent default\s*–\s*Built-in default/);
    expect(rows[2]).toHaveTextContent(/disk_data\s*data\s*disk\s*30 s\s*5 s\s*mounts_exclude=\/data\/tmp\s*This server/);
    expect(rows[3]).toHaveTextContent(/memory.*2 min.*Site/);
    expect(screen.getByText(/In sync/)).toHaveTextContent("In sync: the agent runs revision 5, applied just now.");
  });

  it("shows drift between the applied and current revisions", async () => {
    stubFetch({ [PATH]: { status: 200, body: config({ revision: 7, applied_revision: 5, in_sync: false }) } });
    render(<SensorConfigPanel serverId={ID} />);
    expect(await screen.findByText(/Out of date/)).toHaveTextContent(
      "Out of date: the agent runs revision 5 (applied just now), the current one is 7.",
    );
  });

  it("says when the agent never acknowledged a configuration", async () => {
    stubFetch({
      [PATH]: { status: 200, body: config({ applied_revision: null, applied_at: null, in_sync: false }) },
    });
    render(<SensorConfigPanel serverId={ID} />);
    expect(await screen.findByText(/Not applied/)).toHaveTextContent("The current revision is 5.");
  });

  it("says when every sensor is turned off", async () => {
    stubFetch({ [PATH]: { status: 200, body: config({ sensors: [] }) } });
    render(<SensorConfigPanel serverId={ID} />);
    expect(await screen.findByText(/No sensors/)).toBeInTheDocument();
    expect(screen.queryByRole("table")).not.toBeInTheDocument();
  });

  it("shows errors", async () => {
    stubFetch({ [PATH]: { status: 500, body: { error: "internal error" } } });
    render(<SensorConfigPanel serverId={ID} />);
    expect(await screen.findByRole("alert")).toHaveTextContent("could not handle the request");
  });
});
