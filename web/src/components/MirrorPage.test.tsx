import { render, screen, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { describe, expect, it, vi } from "vitest";

import { stubFetch } from "@/test/fetch";
import { MIRROR_ID, mirrorDetail, run, snapshot } from "@/test/mirrors";

import { MirrorPage } from "./MirrorPage";

vi.mock("next/navigation", () => import("@/test/navigation"));

const PATH = `GET /api/mirrors/${MIRROR_ID}`;

describe("MirrorPage", () => {
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

  it("does not tell a missing mirror from a bad ID", async () => {
    stubFetch({ [PATH]: { status: 404, body: { error: "not found" } } });
    const { unmount } = render(<MirrorPage id={MIRROR_ID} />);
    expect(await screen.findByText("Mirror not found")).toBeInTheDocument();
    unmount();
    render(<MirrorPage id="nope" />);
    expect(screen.getByText("Mirror not found")).toBeInTheDocument();
  });
});
