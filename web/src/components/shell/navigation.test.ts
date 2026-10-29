import { describe, expect, it } from "vitest";

import type { Me } from "@/lib/api";

import { visibleNavigation } from "./navigation";

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
    expect(labels(as(null))).toContain("Notifications");
    expect(customerAdmin).not.toContain("Notifications");
  });
});
