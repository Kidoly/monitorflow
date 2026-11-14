import { describe, expect, it } from "vitest";

import type { Me } from "@/lib/api";

import { SETTINGS_ITEM, visibleNavigation } from "./navigation";

const as = (customer_id: string | null): Me => ({
  id: "u1",
  username: "alice",
  roles: [{ role: "administrator", customer_id, site_id: null }],
});

const labels = (me: Me) => visibleNavigation(me).flatMap((section) => section.items.map((item) => item.label));

describe("visibleNavigation", () => {
  it("shows Mirrors to global administrators only", () => {
    expect(labels(as(null))).toContain("Mirrors");
    const customerAdmin = labels(as("c1"));
    expect(customerAdmin).not.toContain("Mirrors");
    expect(labels(as(null))).toContain("Automatic updates");
    expect(customerAdmin).not.toContain("Automatic updates");
    expect(customerAdmin).toContain("Campaigns");
  });

  it("holds daily work only: administration is under Settings", () => {
    expect(labels(as(null))).not.toContain("Relays");
    expect(labels(as(null))).not.toContain("Backups");
    expect(SETTINGS_ITEM.visible?.(as("c1"))).toBe(true);
    expect(SETTINGS_ITEM.visible?.({ ...as(null), roles: [{ role: "viewer", customer_id: null, site_id: null }] })).toBe(false);
  });
});
