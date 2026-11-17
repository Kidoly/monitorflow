import { render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { beforeEach, describe, expect, it, vi } from "vitest";

import { setActiveLocale } from "@/i18n/locale";
import type { CreatedToken, ServerDetail } from "@/lib/api";
import { As, customerAdmin, viewer } from "@/test/admin";
import { stubFetch } from "@/test/fetch";

import { WaitingForAgent } from "./WaitingForAgent";

vi.mock("next/navigation", () => import("@/test/navigation"));

const SECRET = "mfe1.c2VjcmV0.Y2E";

const server = {
  id: "s1",
  peer_id: "web-01",
  hostname: "web-01",
  site_id: "site-1",
  site_name: "paris",
  customer_id: "c1",
  customer_name: "acme",
  awaiting_agent: true,
} as ServerDetail;

const created: CreatedToken = {
  id: "t1",
  role: "agent",
  peer_id: "web-01",
  site_id: "site-1",
  site_name: "paris",
  customer_id: "c1",
  customer_name: "acme",
  server_names: [],
  created_by: "u1",
  created_by_username: "alice",
  created_at: "2026-11-17T10:00:00Z",
  expires_at: "2026-11-24T10:00:00Z",
  used_at: null,
  state: "pending",
  token: SECRET,
  upstream_url: "https://mf.example:7443",
  enroll_url: "https://mf.example:7445",
  install: {
    source: "github",
    custom_url: null,
    source_from_config: false,
    custom_url_from_config: false,
    github_url: "https://github.com/Kidoly/monitorflow/releases/latest/download",
    central: null,
  },
  address: {
    address: "mf.example",
    default: "mf.example",
    names: ["mf.example"],
    certified: true,
    upstream_url: "https://mf.example:7443",
    enroll_url: "https://mf.example:7445",
  },
};

describe("WaitingForAgent", () => {
  beforeEach(() => setActiveLocale("en"));

  it("makes a token for the server's name and shows its install command", async () => {
    const fetchMock = stubFetch({ "POST /api/tokens": { status: 201, body: created } });
    render(
      <As user={customerAdmin}>
        <WaitingForAgent server={server} />
      </As>,
    );
    expect(screen.getByText(/once it enrolls as web-01/)).toBeInTheDocument();
    await userEvent.click(screen.getByRole("button", { name: "Make an install command" }));
    const body = JSON.parse(String(fetchMock.mock.calls[0]?.[1]?.body));
    expect(body).toMatchObject({ role: "agent", peer_id: "web-01", site_id: "site-1" });
    const install = await screen.findByRole("group", { name: "Install the agent" });
    expect(install).toHaveTextContent(`MF_TOKEN=${SECRET}`);
  });

  it("only explains to people who cannot make tokens", () => {
    stubFetch({});
    render(
      <As user={viewer}>
        <WaitingForAgent server={server} />
      </As>,
    );
    expect(screen.getByText("Waiting for its agent")).toBeInTheDocument();
    expect(screen.queryByRole("button")).toBeNull();
  });
});
