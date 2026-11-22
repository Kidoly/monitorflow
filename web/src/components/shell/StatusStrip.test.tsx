import { render, screen, within } from "@testing-library/react";
import { beforeEach, describe, expect, it, vi } from "vitest";

import { stubFetch } from "@/test/fetch";
import { setUrl } from "@/test/navigation";
import { ACME, LYON, summary } from "@/test/shell";

import { StatusStrip } from "./StatusStrip";

vi.mock("next/navigation", () => import("@/test/navigation"));

const strip = () => screen.findByRole("navigation", { name: "Servers by status" });

describe("StatusStrip", () => {
  beforeEach(() => setUrl("/incidents/"));

  it("counts every state, each opening the fleet filtered to it, and Silenced the silences", async () => {
    stubFetch({ "GET /api/servers/summary": { status: 200, body: summary } });
    render(<StatusStrip />);
    const links = within(await strip()).getAllByRole("link");
    expect(links.map((l) => [l.textContent, l.getAttribute("href")])).toEqual([
      ["2 Critical", "/?state=critical"],
      ["0 Warning", "/?state=warning"],
      ["1 Acknowledged", "/?state=acknowledged"],
      ["1 Silenced", "/silences"],
      ["3 Offline", "/?state=offline"],
      ["1,980 OK", "/?state=ok"],
    ]);
    // Problems are tinted; zero and OK stay plain.
    expect(links[0]!.className).toMatch(/bad/);
    expect(links[1]!.className).not.toMatch(/warn/);
    expect(links[5]!.className).not.toMatch(/ok/);
  });

  it("keeps the scope, and marks the state the fleet shows", async () => {
    setUrl(`/?customer=${ACME}&site=${LYON}&state=offline`);
    const fetchMock = stubFetch({ [`GET /api/servers/summary?site_id=${LYON}`]: { status: 200, body: summary } });
    render(<StatusStrip />);
    const offline = within(await strip()).getByRole("link", { name: "3 Offline" });
    expect(offline).toHaveAttribute("aria-current", "page");
    // Clicked again, it clears the filter.
    expect(offline).toHaveAttribute("href", `/?customer=${ACME}&site=${LYON}`);
    expect(screen.getByRole("link", { name: "2 Critical" })).toHaveAttribute(
      "href",
      `/?state=critical&customer=${ACME}&site=${LYON}`,
    );
    expect(fetchMock).toHaveBeenCalledTimes(1);
  });

  it("marks Silenced on the silences page", async () => {
    setUrl("/silences/");
    stubFetch({ "GET /api/servers/summary": { status: 200, body: summary } });
    render(<StatusStrip />);
    const silenced = within(await strip()).getByRole("link", { name: "1 Silenced" });
    expect(silenced).toHaveAttribute("aria-current", "page");
    expect(silenced).toHaveAttribute("href", "/silences");
  });

  it("stays away without servers or when the counts fail", async () => {
    for (const reply of [{ status: 200, body: { ...summary, ok: 0, critical: 0, acknowledged: 0, silenced: 0, offline: 0, total: 0 } }, { status: 403, body: { error: "no", code: "forbidden" } }]) {
      const fetchMock = stubFetch({ "GET /api/servers/summary": reply });
      const { unmount } = render(<StatusStrip />);
      await vi.waitFor(() => expect(fetchMock).toHaveBeenCalled());
      await new Promise((r) => setTimeout(r, 0));
      expect(screen.queryByRole("navigation", { name: "Servers by status" })).not.toBeInTheDocument();
      unmount();
    }
  });
});
