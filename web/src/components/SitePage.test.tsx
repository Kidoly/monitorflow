import { render, screen } from "@testing-library/react";
import { describe, expect, it, vi } from "vitest";

import type { Site } from "@/lib/api";
import { As, viewer } from "@/test/admin";
import { stubFetch } from "@/test/fetch";

import { SitePage } from "./SitePage";

vi.mock("next/navigation", () => import("@/test/navigation"));

const ID = "6f1c2a52-3b5e-4c43-9a51-0e7f6d1f1a09";

const paris: Site = {
  id: ID,
  name: "paris",
  server_count: 3,
  relay_count: 1,
  customer_id: "c1",
  customer_name: "acme",
  agent_auto_update: null,
  agent_auto_update_effective: true,
};

describe("SitePage", () => {
  it("shows the site's alerts and checks, read-only for viewers", async () => {
    stubFetch({
      "GET /api/sites": { status: 200, body: { sites: [paris] } },
      [`GET /api/sites/${ID}/thresholds/effective`]: { status: 200, body: { site_id: ID, thresholds: [] } },
      [`GET /api/sites/${ID}/sensors`]: { status: 200, body: { site_id: ID, sensors: [] } },
      [`GET /api/sites/${ID}/sensor_configs`]: { status: 200, body: { entries: [] } },
    });
    render(
      <As user={viewer}>
        <SitePage id={ID} />
      </As>,
    );
    expect(await screen.findByRole("heading", { level: 1, name: "paris" })).toBeInTheDocument();
    expect(screen.getByText("acme · 3 servers")).toBeInTheDocument();
    expect(screen.getByRole("link", { name: "Show its 3 servers" })).toHaveAttribute("href", `/?customer=c1&site=${ID}`);
    expect(await screen.findByText(/Every server of paris uses these/)).toBeInTheDocument();
    expect(screen.getByText(/Nothing is checked/)).toBeInTheDocument();
    expect(screen.queryByRole("button", { name: "Add a check" })).not.toBeInTheDocument();
  });

  it("says when the site does not exist or is out of scope", async () => {
    stubFetch({ "GET /api/sites": { status: 200, body: { sites: [] } } });
    render(<SitePage id={ID} />);
    expect(await screen.findByText("Site not found")).toBeInTheDocument();
  });
});
