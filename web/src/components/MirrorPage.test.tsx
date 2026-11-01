import { render, screen, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { describe, expect, it, vi } from "vitest";

import { stubFetch } from "@/test/fetch";
import { MIRROR_ID, mirror, mirrorDetail, run, snapshot } from "@/test/mirrors";

import { MirrorPage } from "./MirrorPage";

vi.mock("next/navigation", () => import("@/test/navigation"));

const PATH = `GET /api/mirrors/${MIRROR_ID}`;

describe("MirrorPage", () => {
  it("offers one next step on a mirror that never ran", async () => {
    stubFetch({ [PATH]: { status: 200, body: mirrorDetail({ runs: [], snapshots: [], last_run: null }) } });
    render(<MirrorPage id={MIRROR_ID} />);
    expect(await screen.findByText("No snapshots yet")).toBeInTheDocument();
    expect(screen.getAllByRole("button", { name: "Sync now" })).toHaveLength(2);
    expect(screen.queryByText("No runs yet.")).not.toBeInTheDocument();
  });

  it("shows the mirror, its snapshots and its runs", async () => {
    stubFetch({
      [PATH]: {
        status: 200,
        body: mirrorDetail({
          snapshots: [snapshot({ id: "s2", name: "20261023T090000Z", state: "creating", size_bytes: null, published_at: null }), snapshot()],
          runs: [
            run({ id: "r2", state: "failed", error: "Curl error (28) on repomd.xml", output: "reposync\nfailed", snapshot_id: null }),
            run(),
          ],
        }),
      },
    });
    render(<MirrorPage id={MIRROR_ID} />);

    expect(await screen.findByRole("heading", { level: 1, name: "debian-bookworm" })).toBeInTheDocument();
    expect(screen.getByRole("navigation", { name: "Breadcrumb" })).toHaveTextContent("Mirrors/debian-bookworm");
    expect(screen.getByText("apt · deb.debian.org/debian · bookworm · main contrib · amd64 arm64 · every 12 h")).toBeInTheDocument();
    expect(screen.getByText("Last run succeeded")).toBeInTheDocument();

    const snapshots = screen.getByRole("table", { name: "Snapshots" });
    expect(within(snapshots).getByText("20261022T143000Z").closest("tr")).toHaveTextContent("Published current");
    expect(within(snapshots).getByText("20261022T143000Z").closest("tr")).toHaveTextContent("31.2 GiB");
    expect(within(snapshots).getByText("20261023T090000Z").closest("tr")).toHaveTextContent("Creating");

    const runs = screen.getByRole("table", { name: "Runs" });
    const failed = within(runs).getByText("Curl error (28) on repomd.xml").closest("tr")!;
    expect(failed).toHaveTextContent("Failed");
    expect(failed).toHaveTextContent("11 min 40 s");
    expect(within(runs).getAllByRole("row")[2]).toHaveTextContent("Synced, snapshot 20261022T143000Z published");
  });

  it("expands a run's log as plain text under its row", async () => {
    const user = userEvent.setup();
    stubFetch({ [PATH]: { status: 200, body: mirrorDetail({ runs: [run({ output: "<b>not html</b>" })] }) } });
    render(<MirrorPage id={MIRROR_ID} />);

    const log = await screen.findByRole("button", { name: /^Log/ });
    expect(log).toHaveAttribute("aria-expanded", "false");
    await user.click(log);
    expect(screen.getByRole("button", { name: /^Hide log/ })).toHaveAttribute("aria-expanded", "true");
    const pre = screen.getByText("<b>not html</b>");
    expect(pre.tagName).toBe("PRE");
    const wrap = screen.getByRole("button", { name: "Wrap lines" });
    await user.click(wrap);
    expect(wrap).toHaveAttribute("aria-pressed", "true");
  });

  it("starts a snapshot from the header and disables both while running", async () => {
    const user = userEvent.setup();
    const fetchMock = stubFetch({
      [PATH]: [
        { status: 200, body: mirrorDetail() },
        {
          status: 200,
          body: mirrorDetail({ last_run: { ...mirrorDetail().last_run!, kind: "snapshot", state: "running", finished_at: null } }),
        },
      ],
      [`POST /api/mirrors/${MIRROR_ID}/snapshots`]: { status: 202, body: run({ kind: "snapshot", state: "running" }) },
    });
    render(<MirrorPage id={MIRROR_ID} />);
    await user.click(await screen.findByRole("button", { name: "Create snapshot" }));
    expect(fetchMock).toHaveBeenCalledWith(
      `/api/mirrors/${MIRROR_ID}/snapshots`,
      expect.objectContaining({ method: "POST", body: "{}" }),
    );
    expect(await screen.findByText("Running", { selector: "span" })).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "Sync now" })).toBeDisabled();
    expect(screen.getByRole("button", { name: "Create snapshot" })).toBeDisabled();
  });

  it("says why each snapshot stays and what retention keeps", async () => {
    stubFetch({
      [PATH]: {
        status: 200,
        body: mirrorDetail({
          snapshots: [
            snapshot({ id: "a", name: "new" }),
            snapshot({ id: "b", name: "pinned", kept: "campaign" }),
            snapshot({ id: "c", name: "old", kept: null }),
            snapshot({ id: "d", name: "going", state: "pruning", kept: null, published_at: null, error: "aptly failed" }),
          ],
        }),
      },
    });
    render(<MirrorPage id={MIRROR_ID} />);
    const table = await screen.findByRole("table", { name: "Snapshots" });
    expect(within(table).getByText("new").closest("tr")).toHaveTextContent("The current snapshot");
    expect(within(table).getByText("pinned").closest("tr")).toHaveTextContent("An update campaign is pinned to it");
    expect(within(table).getByText("old").closest("tr")).toHaveTextContent("Removed at the next cleanup");
    expect(within(table).getByText("going").closest("tr")).toHaveTextContent("Being removed aptly failed");
    expect(
      screen.getByText(
        "Each mirror keeps its 7 newest snapshots. Older ones are removed by themselves and their disk space is freed, unless something still needs them: the current snapshot, a snapshot an update campaign is pinned to, or one a relay or a server is still using.",
      ),
    ).toBeInTheDocument();
  });

  it("removes a mirror after a confirmation that says what happens", async () => {
    const user = userEvent.setup();
    const fetchMock = stubFetch({
      [PATH]: [
        { status: 200, body: mirrorDetail() },
        { status: 200, body: mirrorDetail({ deleting_at: "2026-10-22T15:00:00Z", snapshots: [], current_snapshot: null }) },
        { status: 404, body: { error: "not found" } },
      ],
      [`DELETE /api/mirrors/${MIRROR_ID}`]: { status: 202, body: mirror({ deleting_at: "2026-10-22T15:00:00Z" }) },
    });
    render(<MirrorPage id={MIRROR_ID} />);

    await user.click(await screen.findByRole("button", { name: "Remove debian-bookworm…" }));
    expect(screen.getByText(/Its snapshots and packages are deleted from this server/)).toHaveTextContent(
      "Relays delete their copy at their next sync. This can't be undone",
    );
    expect(fetchMock).not.toHaveBeenCalledWith(`/api/mirrors/${MIRROR_ID}`, expect.objectContaining({ method: "DELETE" }));
    await user.click(screen.getByRole("button", { name: "Remove debian-bookworm" }));

    expect(fetchMock).toHaveBeenCalledWith(`/api/mirrors/${MIRROR_ID}`, expect.objectContaining({ method: "DELETE" }));
    expect(await screen.findByRole("status")).toHaveTextContent("Removing this mirror.");
    expect(screen.getByText("Being removed")).toBeInTheDocument();
    for (const button of screen.getAllByRole("button", { name: "Sync now" })) expect(button).toBeDisabled();
    expect(screen.queryByRole("button", { name: /^Remove/ })).not.toBeInTheDocument();
  });

  it("warns that removing stops a running sync", async () => {
    const user = userEvent.setup();
    stubFetch({
      [PATH]: { status: 200, body: mirrorDetail({ last_run: { ...mirrorDetail().last_run!, state: "running", finished_at: null } }) },
    });
    render(<MirrorPage id={MIRROR_ID} />);
    await user.click(await screen.findByRole("button", { name: "Remove debian-bookworm…" }));
    expect(screen.getByText(/The run going now is stopped\./)).toBeInTheDocument();
  });

  it("says what uses a mirror and offers no way to remove it", async () => {
    stubFetch({
      [PATH]: {
        status: 200,
        body: mirrorDetail({
          used_by: {
            campaigns: [{ id: "c1", name: "March updates" }],
            servers: [{ id: "s1", hostname: "web-01" }],
          },
        }),
      },
    });
    render(<MirrorPage id={MIRROR_ID} />);
    const section = await screen.findByRole("region", { name: "Remove this mirror" });
    expect(section).toHaveTextContent("debian-bookworm can't be removed yet, because it is in use.");
    expect(within(section).getByRole("link", { name: "March updates" })).toHaveAttribute("href", expect.stringContaining("c1"));
    expect(within(section).getByRole("link", { name: "web-01" })).toHaveAttribute("href", expect.stringContaining("s1"));
    expect(screen.queryByRole("button", { name: /^Remove/ })).not.toBeInTheDocument();
  });

  it("keeps the confirmation open with the server's reason", async () => {
    const user = userEvent.setup();
    stubFetch({
      [PATH]: { status: 200, body: mirrorDetail() },
      [`DELETE /api/mirrors/${MIRROR_ID}`]: { status: 409, body: { error: "debian-bookworm is still used by 1 server (web-01)" } },
    });
    render(<MirrorPage id={MIRROR_ID} />);
    await user.click(await screen.findByRole("button", { name: "Remove debian-bookworm…" }));
    await user.click(screen.getByRole("button", { name: "Remove debian-bookworm" }));
    expect(await screen.findByText("Not removed: debian-bookworm is still used by 1 server (web-01).")).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "Remove debian-bookworm" })).toBeInTheDocument();
  });

  it("shows why a removal failed and tries again on request", async () => {
    const user = userEvent.setup();
    const fetchMock = stubFetch({
      [PATH]: [
        {
          status: 200,
          body: mirrorDetail({ deleting_at: "2026-10-22T15:00:00Z", delete_error: "the mirror tools are not running" }),
        },
        { status: 404, body: { error: "not found" } },
      ],
      [`DELETE /api/mirrors/${MIRROR_ID}`]: { status: 202, body: mirror({ deleting_at: "2026-10-22T15:00:00Z" }) },
    });
    render(<MirrorPage id={MIRROR_ID} />);
    expect(await screen.findByText(/could not be fully removed: the mirror tools are not running/)).toBeInTheDocument();
    expect(screen.getByText("Removal failed")).toBeInTheDocument();
    await user.click(screen.getByRole("button", { name: "Try again now" }));
    expect(fetchMock).toHaveBeenCalledWith(`/api/mirrors/${MIRROR_ID}`, expect.objectContaining({ method: "DELETE" }));
    // Once it ends, the page says so rather than "not found".
    expect(await screen.findByText("Mirror removed")).toBeInTheDocument();
  });

  it("does not tell a missing mirror from a bad ID", async () => {
    stubFetch({ [PATH]: { status: 404, body: { error: "not found" } } });
    const { unmount } = render(<MirrorPage id={MIRROR_ID} />);
    expect(await screen.findByText("Mirror not found")).toBeInTheDocument();
    unmount();
    render(<MirrorPage id="nope" />);
    expect(screen.getByText("Mirror not found")).toBeInTheDocument();
  });
});
