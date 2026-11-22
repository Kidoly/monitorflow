import { describe, expect, it } from "vitest";

import { t } from "@/i18n";
import type { Me } from "@/lib/api";

import { NAVIGATION, SETTINGS_ITEM, arrangedNavigation, ordered, visibleNavigation } from "./navigation";

const as = (customer_id: string | null): Me => ({
  id: "u1",
  username: "alice",
  roles: [{ role: "administrator", customer_id, site_id: null }],
});

const labels = (me: Me) => visibleNavigation(me).flatMap((section) => section.items.map((item) => t(item.label)));

describe("visibleNavigation", () => {
  it("shows Mirrors to global administrators only", () => {
    expect(labels(as(null))).toContain("Linux mirrors");
    const customerAdmin = labels(as("c1"));
    expect(customerAdmin).not.toContain("Linux mirrors");
    expect(labels(as(null))).toContain("Automatic updates");
    expect(customerAdmin).not.toContain("Automatic updates");
    expect(customerAdmin).toContain("Campaigns");
  });

  it("holds daily work only: administration is under Settings", () => {
    expect(labels(as(null))).not.toContain("Relays");
    expect(labels(as(null))).not.toContain("Backups");
    // Everyone has Settings, for their own preferences (ADR-0137).
    expect(SETTINGS_ITEM.visible).toBeUndefined();
  });
});

const ids = (me: Me, sidebar = {}) => arrangedNavigation(me, sidebar).map((section) => section.items.map((item) => item.id));
const viewer: Me = { id: "u2", username: "bob", roles: [{ role: "viewer", customer_id: null, site_id: "s1" }] };

describe("arrangedNavigation", () => {
  it("gives every item a unique identifier the server accepts", () => {
    const all = [...NAVIGATION.flatMap((section) => section.items), SETTINGS_ITEM].map((item) => item.id);
    expect(new Set(all).size).toBe(all.length);
    for (const id of all) expect(id).toMatch(/^[A-Za-z][A-Za-z0-9-]{0,39}$/);
  });

  it("is the default navigation without preferences", () => {
    expect(arrangedNavigation(as(null))).toEqual(visibleNavigation(as(null)));
  });

  it("hides the items the person does not use, and sections left empty", () => {
    const sections = ids(as(null), { hidden: ["silences", "approvals", "audit"] });
    expect(sections).toEqual([
      ["fleet", "dashboards", "incidents", "onCall", "reports"],
      ["mirrors", "automaticUpdates", "campaigns"],
    ]);
  });

  it("orders items within their section", () => {
    const sections = ids(as(null), { order: ["incidents", "fleet", "audit", "approvals"] });
    expect(sections[0]).toEqual(["incidents", "dashboards", "fleet", "silences", "onCall", "reports"]);
    expect(sections[2]).toEqual(["audit", "approvals"]);
  });

  it("never shows an item the roles hide, whatever the preferences say", () => {
    const sections = ids(viewer, { order: ["mirrors", "campaigns", "fleet"], hidden: [] });
    expect(sections.flat()).not.toContain("mirrors");
    expect(sections.flat()).not.toContain("campaigns");
    expect(sections.flat()).toContain("fleet");
  });

  it("ignores identifiers it does not know", () => {
    expect(ids(as(null), { order: ["gone", "audit"], hidden: ["gone"] })).toEqual(ids(as(null), { order: ["audit"] }));
  });
});

describe("ordered", () => {
  const items = ["a", "b", "c", "d"].map((id) => ({ id }));
  const order = (o: string[]) => ordered(items, o).map((item) => item.id);

  it("keeps unlisted items in their default place", () => {
    expect(order([])).toEqual(["a", "b", "c", "d"]);
    expect(order(["d", "b"])).toEqual(["a", "d", "c", "b"]);
    expect(order(["d", "c", "b", "a"])).toEqual(["d", "c", "b", "a"]);
  });
});
