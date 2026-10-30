import { render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { describe, expect, it, vi } from "vitest";

import { As, me, viewer } from "@/test/admin";
import { stubFetch } from "@/test/fetch";

import { OfflineAlerts } from "./OfflineAlerts";

const operator = me({ role: "operator", customer_id: null, site_id: "site-1" });
const target = { id: "s1", customer_id: "c1", site_id: "site-1" };

describe("OfflineAlerts", () => {
  it("is on by default and lets operators of the site mute it", async () => {
    const fetchMock = stubFetch({ "PUT /api/servers/s1/offline-alerts": { status: 200, body: { enabled: false } } });
    const onChanged = vi.fn();
    render(
      <As user={operator}>
        <OfflineAlerts kind="servers" target={target} onChanged={onChanged} />
      </As>,
    );
    expect(screen.getByText(/critical incident when it sends nothing for 5 min/)).toBeInTheDocument();
    await userEvent.click(screen.getByRole("button", { name: "Mute" }));
    expect(fetchMock.mock.calls[0]?.[1]?.body).toBe(JSON.stringify({ enabled: false }));
    expect(onChanged).toHaveBeenCalled();
  });

  it("shows a muted one, with no button for viewers or another site's operator", () => {
    const { rerender } = render(
      <As user={viewer}>
        <OfflineAlerts kind="relays" target={{ ...target, offline_alerts: false }} onChanged={() => {}} />
      </As>,
    );
    expect(screen.getByText("Muted")).toBeInTheDocument();
    expect(screen.queryByRole("button")).not.toBeInTheDocument();
    rerender(
      <As user={operator}>
        <OfflineAlerts kind="relays" target={{ ...target, site_id: "site-2", offline_alerts: false }} onChanged={() => {}} />
      </As>,
    );
    expect(screen.queryByRole("button")).not.toBeInTheDocument();
    rerender(
      <As user={operator}>
        <OfflineAlerts kind="relays" target={{ ...target, offline_alerts: false }} onChanged={() => {}} />
      </As>,
    );
    expect(screen.getByRole("button", { name: "Turn on" })).toBeInTheDocument();
  });

  it("points to the site when the whole site is muted", () => {
    render(
      <As user={operator}>
        <OfflineAlerts kind="servers" target={{ ...target, site_offline_alerts: false }} onChanged={() => {}} />
      </As>,
    );
    expect(screen.getByText(/for the whole site/)).toBeInTheDocument();
    expect(screen.getByRole("link", { name: "Sites" })).toHaveAttribute("href", expect.stringMatching(/^\/admin\/sites\/?$/));
    expect(screen.queryByRole("button")).not.toBeInTheDocument();
  });
});
