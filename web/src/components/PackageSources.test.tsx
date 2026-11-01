import { render, screen, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { describe, expect, it, vi } from "vitest";

import type { ServerDetail, Site, SourcesStatus } from "@/lib/api";
import { As, me, viewer } from "@/test/admin";
import { stubFetch } from "@/test/fetch";

import { PackageSources, SiteSources } from "./PackageSources";

vi.mock("next/navigation", () => import("@/test/navigation"));

const globalAdmin = me({ role: "administrator", customer_id: null, site_id: null });
const ID = "0b7c5d6e-1f2a-4b3c-8d4e-5f6a7b8c9d0e";
const SITE = "6f1c2a52-3b5e-4c43-9a51-0e7f6d1f1a09";
const server = { id: ID, hostname: "web-1", customer_id: "c1" } as ServerDetail;
const site = { id: SITE, name: "paris", customer_id: "c1" } as Site;

const own: SourcesStatus = {
  using: "own",
  mirrors: [],
  changing_to: null,
  failed: null,
  campaign: false,
  distribution: "Debian 12 (bookworm)",
  problem: null,
  add_mirror: false,
};
const onMirror: SourcesStatus = {
  ...own,
  using: "mirror",
  mirrors: [
    { name: "debian-12", snapshot: "current" },
    { name: "debian-12-security", snapshot: "s1" },
  ],
};
const PATH = `/api/servers/${ID}/sources`;
const ok = (body: unknown) => ({ status: 200, body });

function show(user = globalAdmin) {
  return render(
    <As user={user}>
      <PackageSources server={server} />
    </As>,
  );
}

describe("PackageSources", () => {
  it("offers the mirror of the server's distribution, says what it does and shows the change", async () => {
    const fetchMock = stubFetch({
      [`GET ${PATH}`]: [ok(own), ok({ ...own, changing_to: "mirror" })],
      [`POST ${PATH}/use-mirror`]: { status: 201, body: {} },
    });
    show();
    expect(await screen.findByText("web-1 gets its packages from the sources it came with.")).toBeInTheDocument();
    expect(screen.getByText("Uses its own sources")).toBeInTheDocument();

    await userEvent.click(screen.getByRole("button", { name: "Use the Debian 12 (bookworm) mirror…" }));
    expect(screen.getByText(/set aside/)).toBeInTheDocument();
    expect(screen.getByText(/You can go back at any time/)).toBeInTheDocument();
    await userEvent.click(screen.getByRole("button", { name: "Use the mirror on web-1" }));

    expect(await screen.findByText("Moving to a mirror")).toBeInTheDocument();
    expect(screen.getByText(/The change reaches it at its next contact/)).toBeInTheDocument();
    expect(screen.queryByRole("button", { name: /mirror…/ })).not.toBeInTheDocument();
    expect(fetchMock.mock.calls.some(([url, init]) => String(url).endsWith("/use-mirror") && init?.method === "POST")).toBe(true);
  });

  it("names the mirrors in use and offers the way back", async () => {
    const fetchMock = stubFetch({
      [`GET ${PATH}`]: [ok(onMirror), ok({ ...onMirror, changing_to: "own" })],
      [`DELETE ${PATH}`]: { status: 202, body: {} },
    });
    show();
    expect(await screen.findByText(/from debian-12, debian-12-security at snapshot s1\./)).toBeInTheDocument();
    await userEvent.click(screen.getByRole("button", { name: "Go back to its own sources…" }));
    expect(screen.getByText("Its original package sources are put back exactly as they were.")).toBeInTheDocument();
    await userEvent.click(screen.getByRole("button", { name: "Go back on web-1" }));
    expect(await screen.findByText("Going back to its own sources")).toBeInTheDocument();
    expect(fetchMock.mock.calls.some(([, init]) => init?.method === "DELETE")).toBe(true);
  });

  it("says plainly when no mirror carries the distribution, with the next step", async () => {
    const problem = "No mirror carries Debian 12 (bookworm) yet, so this server keeps its own package sources.";
    stubFetch({ [`GET ${PATH}`]: ok({ ...own, problem, add_mirror: true }) });
    show();
    expect(await screen.findByText(problem)).toBeInTheDocument();
    expect(screen.getByRole("link", { name: "Add Debian 12 (bookworm) on the Mirrors page" })).toHaveAttribute(
      "href",
      "/admin/mirrors",
    );
    expect(screen.queryByRole("button", { name: /mirror/ })).not.toBeInTheDocument();
  });

  it("points people who cannot add mirrors to someone who can", async () => {
    stubFetch({ [`GET ${PATH}`]: ok({ ...own, problem: "No mirror.", add_mirror: true }) });
    show(viewer);
    expect(await screen.findByText("A global administrator can add it on the Mirrors page.")).toBeInTheDocument();
    expect(screen.queryByRole("link", { name: /Mirrors page/ })).not.toBeInTheDocument();
  });

  it("offers nothing without the role, or while a campaign has the sources", async () => {
    stubFetch({ [`GET ${PATH}`]: ok(own) });
    const { unmount } = show(viewer);
    await screen.findByText("Uses its own sources");
    expect(screen.queryByRole("button")).not.toBeInTheDocument();

    unmount();
    stubFetch({ [`GET ${PATH}`]: ok({ ...onMirror, campaign: true }) });
    show();
    expect(await screen.findByText(/An update campaign has its sources for now/)).toBeInTheDocument();
    expect(screen.queryByRole("button", { name: /Go back/ })).not.toBeInTheDocument();
  });

  it("tells why the last change failed and keeps the offer", async () => {
    stubFetch({ [`GET ${PATH}`]: ok({ ...own, failed: "apt-get update failed: 404" }) });
    show();
    expect(await screen.findByRole("alert")).toHaveTextContent(
      "The last change didn't work: apt-get update failed: 404 A change that fails puts the server's own sources back.",
    );
    expect(screen.getByRole("button", { name: /mirror…/ })).toBeInTheDocument();
  });

  it("keeps the confirmation open with the reason when the API refuses", async () => {
    stubFetch({
      [`GET ${PATH}`]: ok(own),
      [`POST ${PATH}/use-mirror`]: { status: 409, body: { error: "The address of the mirrors isn't set." } },
    });
    show();
    await userEvent.click(await screen.findByRole("button", { name: /mirror…/ }));
    await userEvent.click(screen.getByRole("button", { name: "Use the mirror on web-1" }));
    expect(await screen.findByText(/Nothing changed\. .*address of the mirrors/)).toBeInTheDocument();
  });
});

describe("SiteSources", () => {
  const path = `/api/sites/${SITE}/sources`;
  const row = (id: string, hostname: string, status: SourcesStatus) => ({ id, hostname, ...status });
  const servers = [
    row("a", "db-1", own),
    row("b", "web-1", onMirror),
    row("c", "old-1", { ...own, distribution: null, problem: "MonitorFlow has no mirror for debian 11." }),
  ];

  function showSite(user = globalAdmin) {
    return render(
      <As user={user}>
        <SiteSources site={site} />
      </As>,
    );
  }

  it("shows each server and moves those that can, saying what happened to the others", async () => {
    const skipped = { id: "c", hostname: "old-1", reason: "MonitorFlow has no mirror for debian 11." };
    stubFetch({
      [`GET ${path}`]: ok({ servers }),
      [`POST ${path}/use-mirror`]: ok({ queued: ["a"], skipped: [skipped] }),
    });
    showSite();
    const table = await screen.findByRole("table", { name: "Package sources of the site's servers" });
    expect(within(table).getByRole("link", { name: "db-1" })).toHaveAttribute(
      "href",
      expect.stringContaining("tab=updates"),
    );
    expect(within(table).getByText("MonitorFlow has no mirror for debian 11.")).toBeInTheDocument();
    expect(within(table).getByText("Can use Debian 12 (bookworm).")).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "Go back on 1 server…" })).toBeInTheDocument();

    await userEvent.click(screen.getByRole("button", { name: "Use mirrors on 1 server…" }));
    await userEvent.click(screen.getByRole("button", { name: "Use mirrors on 1 server" }));
    expect(await screen.findByText(/Sent to 1 server\. Each one changes at its next contact\./)).toBeInTheDocument();
    expect(screen.getByText("old-1: MonitorFlow has no mirror for debian 11.")).toBeInTheDocument();
  });

  it("goes back on the servers that use a mirror", async () => {
    stubFetch({
      [`GET ${path}`]: ok({ servers }),
      [`DELETE ${path}`]: ok({ queued: ["b"], skipped: [] }),
    });
    showSite();
    await userEvent.click(await screen.findByRole("button", { name: "Go back on 1 server…" }));
    await userEvent.click(screen.getByRole("button", { name: "Go back on 1 server" }));
    expect(await screen.findByText(/Sent to 1 server/)).toBeInTheDocument();
  });

  it("offers no change to viewers, and stays away from an empty site", async () => {
    stubFetch({ [`GET ${path}`]: ok({ servers }) });
    const { unmount } = showSite(viewer);
    await screen.findByRole("table");
    expect(screen.queryByRole("button")).not.toBeInTheDocument();

    unmount();
    stubFetch({ [`GET ${path}`]: ok({ servers: [] }) });
    showSite();
    await new Promise((resolve) => setTimeout(resolve, 10));
    expect(screen.queryByRole("heading", { name: "Package sources" })).not.toBeInTheDocument();
  });
});
