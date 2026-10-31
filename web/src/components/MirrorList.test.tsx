import { render, screen, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { beforeEach, describe, expect, it, vi } from "vitest";

import { As, customerAdmin, me } from "@/test/admin";
import { stubFetch } from "@/test/fetch";
import { MIRROR_ID, mirror } from "@/test/mirrors";
import { setUrl } from "@/test/navigation";

import { MirrorListPage } from "./MirrorList";

vi.mock("next/navigation", () => import("@/test/navigation"));

const globalAdmin = me({ role: "administrator", customer_id: null, site_id: null });
const ROCKY = "9a8b7c6d-5e4f-4a3b-8c2d-1e0f9a8b7c6d";

function mirrors() {
  return [
    mirror(),
    mirror({
      id: ROCKY,
      name: "rocky-9",
      family: "dnf",
      upstream_url: "https://dl.rockylinux.org/pub/rocky/9/BaseOS/x86_64/os/",
      distribution: "",
      components: [],
      architectures: ["x86_64"],
      last_run: { ...mirror().last_run!, id: "r2", state: "failed", error: "reposync exited with 1" },
    }),
  ];
}

describe("MirrorListPage", () => {
  beforeEach(() => setUrl("/admin/mirrors/"));

  it("lists mirrors problems first with their last run and counters", async () => {
    stubFetch({ "GET /api/mirrors": { status: 200, body: { mirrors: mirrors() } } });
    render(
      <As user={globalAdmin}>
        <MirrorListPage />
      </As>,
    );
    const table = await screen.findByRole("table", { name: "Mirrors" });
    const rows = within(table).getAllByRole("row").slice(1);
    expect(rows[0]).toHaveTextContent("rocky-9");
    expect(rows[0]).toHaveTextContent("dnf · dl.rockylinux.org/pub/rocky/9/BaseOS/x86_64/os · x86_64");
    expect(within(rows[0]!).getByText("Failed")).toBeInTheDocument();
    expect(rows[1]).toHaveTextContent("apt · deb.debian.org/debian · main contrib · amd64 arm64");
    expect(rows[1]).toHaveTextContent("bookworm");
    expect(rows[1]).toHaveTextContent("20261022T143000Z");
    expect(within(rows[1]!).getByRole("link", { name: "debian-bookworm" })).toHaveAttribute(
      "href",
      expect.stringContaining(`id=${MIRROR_ID}`),
    );

    const tiles = screen.getByRole("group", { name: "Summary, click to filter" });
    expect(within(tiles).getByRole("link", { name: /Failed/ })).toHaveTextContent("Failed1");
    expect(within(tiles).getByRole("link", { name: /Never run/ })).toHaveTextContent("Never run0");
  });

  it("filters by family in the URL", async () => {
    const user = userEvent.setup();
    stubFetch({ "GET /api/mirrors": { status: 200, body: { mirrors: mirrors() } } });
    render(
      <As user={globalAdmin}>
        <MirrorListPage />
      </As>,
    );
    await user.selectOptions(await screen.findByRole("combobox", { name: "Family" }), "dnf");
    expect(window.location.search).toBe("?family=dnf");
    expect(screen.queryByRole("link", { name: "debian-bookworm" })).not.toBeInTheDocument();
    expect(screen.getByRole("link", { name: "rocky-9" })).toBeInTheDocument();
  });

  it("starts a sync and says why one is refused", async () => {
    const user = userEvent.setup();
    const fetchMock = stubFetch({
      "GET /api/mirrors": { status: 200, body: { mirrors: mirrors() } },
      [`POST /api/mirrors/${MIRROR_ID}/sync`]: [
        { status: 202, body: mirror().last_run },
        { status: 409, body: { error: "a job of this mirror is already running" } },
      ],
    });
    render(
      <As user={globalAdmin}>
        <MirrorListPage />
      </As>,
    );
    await screen.findByRole("table");
    const sync = await screen.findByRole("button", { name: "Sync now debian-bookworm" });
    await user.click(sync);
    expect(fetchMock).toHaveBeenCalledWith(`/api/mirrors/${MIRROR_ID}/sync`, expect.objectContaining({ method: "POST" }));
    await user.click(await screen.findByRole("button", { name: "Sync now debian-bookworm" }));
    expect(await screen.findByRole("alert")).toHaveTextContent(
      "Cannot start a sync of debian-bookworm: a job of this mirror is already running.",
    );
  });

  it("disables Sync now while a run is going", async () => {
    const running = mirror({ last_run: { ...mirror().last_run!, state: "running", finished_at: null } });
    stubFetch({ "GET /api/mirrors": { status: 200, body: { mirrors: [running] } } });
    render(
      <As user={globalAdmin}>
        <MirrorListPage />
      </As>,
    );
    expect(await screen.findByRole("button", { name: "Sync now debian-bookworm" })).toBeDisabled();
    expect(screen.getByRole("table")).toHaveTextContent(/Running since (\d+ \w{3} )?\d\d:\d\d/);
  });

  describe("adding a mirror", () => {
    const GIB = 1024 ** 3;
    const catalog = (overrides = {}) => ({
      tools: { available: true, problem: null },
      disk: { free_bytes: 200 * GIB, total_bytes: 1000 * GIB },
      distributions: [
        { id: "debian-13", name: "Debian 13 (trixie)", family: "apt", size_bytes: 150 * GIB, added: false, problem: null },
        { id: "rocky-9", name: "Rocky Linux 9", family: "dnf", size_bytes: 60 * GIB, added: true, problem: null },
        {
          id: "ubuntu-24.04",
          name: "Ubuntu 24.04 LTS (noble)",
          family: "apt",
          size_bytes: 280 * GIB,
          added: false,
          problem: "Ubuntu 24.04 LTS (noble) needs about 280 GiB and this server has 200 GiB free.",
        },
      ],
      ...overrides,
    });

    it("guides from the empty state to a synced distribution in one click", async () => {
      const user = userEvent.setup();
      const fetchMock = stubFetch({
        "GET /api/mirrors": { status: 200, body: { mirrors: [] } },
        "GET /api/mirrors/catalog": { status: 200, body: catalog() },
        "POST /api/mirrors": { status: 201, body: { mirrors: [mirror()] } },
      });
      render(
        <As user={globalAdmin}>
          <MirrorListPage />
        </As>,
      );
      expect(await screen.findByText("No mirrors yet")).toBeInTheDocument();
      expect(screen.queryByText(/monitorflow-server/)).not.toBeInTheDocument();
      await user.click(within(screen.getByText("No mirrors yet").parentElement!).getByRole("button", { name: "Add a mirror" }));

      const table = await screen.findByRole("table", { name: "Distributions" });
      expect(screen.getByText(/200 GiB free of 1,000 GiB on this server/)).toBeInTheDocument();
      expect(within(table).getByText("about 150 GiB")).toBeInTheDocument();
      expect(within(table).getByText("Added")).toBeInTheDocument();
      expect(within(table).getByText(/needs about 280 GiB and this server has 200 GiB free/)).toBeInTheDocument();
      expect(within(table).queryByRole("button", { name: /Ubuntu/ })).not.toBeInTheDocument();

      await user.click(within(table).getByRole("button", { name: "Add Debian 13 (trixie)" }));
      expect(fetchMock).toHaveBeenCalledWith(
        "/api/mirrors",
        expect.objectContaining({ method: "POST", body: JSON.stringify({ distribution: "debian-13" }) }),
      );
      expect(await screen.findByRole("status")).toHaveTextContent(
        "Debian 13 (trixie) added. The first sync starts within a minute",
      );
      expect(screen.queryByRole("table", { name: "Distributions" })).not.toBeInTheDocument();
    });

    it("says what to do when the tools are not running", async () => {
      const user = userEvent.setup();
      stubFetch({
        "GET /api/mirrors": { status: 200, body: { mirrors: mirrors() } },
        "GET /api/mirrors/catalog": {
          status: 200,
          body: catalog({ tools: { available: false, problem: "the mirror tools are not running: refused" } }),
        },
      });
      render(
        <As user={globalAdmin}>
          <MirrorListPage />
        </As>,
      );
      await user.click(await screen.findByRole("button", { name: "Add a mirror" }));
      expect(await screen.findByRole("alert")).toHaveTextContent("up -d mirror");
      expect(await screen.findByRole("button", { name: "Add Debian 13 (trixie)" })).toBeDisabled();
    });

    it("shows a refusal of the server in its own words", async () => {
      const user = userEvent.setup();
      stubFetch({
        "GET /api/mirrors": { status: 200, body: { mirrors: mirrors() } },
        "GET /api/mirrors/catalog": { status: 200, body: catalog() },
        "POST /api/mirrors": {
          status: 409,
          body: { error: "Debian 13 (trixie) needs about 150 GiB and this server has 90 GiB free." },
        },
      });
      render(
        <As user={globalAdmin}>
          <MirrorListPage />
        </As>,
      );
      await user.click(await screen.findByRole("button", { name: "Add a mirror" }));
      await user.click(await screen.findByRole("button", { name: "Add Debian 13 (trixie)" }));
      expect(await screen.findByRole("alert")).toHaveTextContent(
        "Debian 13 (trixie) needs about 150 GiB and this server has 90 GiB free.",
      );
    });
  });

  it("is for global administrators only", () => {
    render(
      <As user={customerAdmin}>
        <MirrorListPage />
      </As>,
    );
    expect(screen.getByText("Mirrors need a global administrator")).toBeInTheDocument();
  });
});
