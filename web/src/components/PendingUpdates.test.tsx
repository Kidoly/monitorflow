import { render, screen, within } from "@testing-library/react";
import { describe, expect, it, vi } from "vitest";

import type { PackageList, ServerDetail, ThirdPartyList } from "@/lib/api";
import { stubFetch } from "@/test/fetch";

import { PendingUpdates } from "./PendingUpdates";

vi.mock("next/navigation", () => import("@/test/navigation"));

const ID = "0b7c5d6e-1f2a-4b3c-8d4e-5f6a7b8c9d0e";
const OS = `GET /api/servers/${ID}/packages?limit=5000`;
const APPS = `GET /api/servers/${ID}/third-party?limit=5000`;

const linux = { id: ID, hostname: "web-1", os_family: "debian" } as ServerDetail;

const packages: PackageList = {
  reboot_required: false,
  reported_at: "2026-11-16T06:00:00Z",
  packages: [
    { name: "openssl", arch: "amd64", version: "3.0.14-1", upgradable_version: "3.0.15-1", title: null, classification: null, severity: null, size_bytes: null, reboot_required: false },
    { name: "bash", arch: "amd64", version: "5.2", upgradable_version: null, title: null, classification: null, severity: null, size_bytes: null, reboot_required: false },
  ],
  next_cursor: null,
};

const apps: ThirdPartyList = {
  reported: true,
  managers: [
    { manager: "flatpak", state: "available", version: "1.14.10", error: null, installed: 2, pending: 1 },
    { manager: "snap", state: "unusable", version: null, error: "snapd is not running", installed: 0, pending: 0 },
  ],
  packages: [
    { manager: "flatpak", id: "app/org.mozilla.firefox/x86_64/stable", name: "Firefox", version: "131.0", available_version: "132.0", scope: "machine", origin: "flathub" },
    { manager: "flatpak", id: "app/org.gnome.Calculator/x86_64/stable", name: "Calculator", version: "46", available_version: null, scope: "machine", origin: "flathub" },
  ],
  next_cursor: null,
};

describe("PendingUpdates", () => {
  it("groups pending updates by source, the OS first", async () => {
    stubFetch({ [OS]: { status: 200, body: packages }, [APPS]: { status: 200, body: apps } });
    render(<PendingUpdates server={linux} />);

    const os = await screen.findByRole("region", { name: "Operating system" });
    expect(os).toHaveTextContent("1 update");
    expect(within(os).getByRole("table")).toHaveTextContent("3.0.14-1 → 3.0.15-1");
    expect(within(os).queryByText("bash")).toBeNull();

    const flatpak = screen.getByRole("region", { name: "Flatpak" });
    expect(flatpak).toHaveTextContent("1 update");
    expect(within(flatpak).getByRole("table")).toHaveTextContent("Firefox");
    expect(within(flatpak).queryByText("Calculator")).toBeNull();

    const snap = screen.getByRole("region", { name: "Snap" });
    expect(snap).toHaveTextContent("Cannot list its apps snapd is not running");
    expect(screen.queryByRole("region", { name: "winget" })).toBeNull();
    const order = screen.getAllByRole("heading", { level: 3 }).map((h) => h.textContent?.split(" ")[0]);
    expect(order).toEqual(["Operating", "Flatpak", "Snap"]);
  });

  it("says when the agent does not list apps yet", async () => {
    const windows = { ...linux, os_family: "windows" } as ServerDetail;
    stubFetch({
      [OS]: { status: 200, body: { ...packages, packages: [] } },
      [APPS]: { status: 200, body: { reported: false, managers: [], packages: [], next_cursor: null } },
    });
    render(<PendingUpdates server={windows} />);

    expect(await screen.findByRole("region", { name: "Operating system" })).toHaveTextContent("Up to date");
    expect(screen.getByRole("region", { name: "winget" })).toHaveTextContent("Not known yet");
    expect(screen.getByText("Third-party apps show once the agent is updated to list them.")).toBeInTheDocument();
    expect(screen.queryByRole("region", { name: "Flatpak" })).toBeNull();
  });
});
