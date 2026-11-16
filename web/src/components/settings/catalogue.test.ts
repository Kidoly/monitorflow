import { describe, expect, it } from "vitest";

import { t } from "@/i18n";
import { customerAdmin, me, single, viewer } from "@/test/admin";

import { currentSettingsEntry, filterSettings, insideSettings, visibleSettings } from "./catalogue";

const global = me({ role: "administrator", customer_id: null, site_id: null });
const ids = (groups: ReturnType<typeof visibleSettings>) => groups.flatMap((g) => g.entries.map((e) => e.id));

describe("settings catalogue", () => {
  it("shows global administrators every page, in the five groups", () => {
    const groups = visibleSettings(global);
    expect(groups.map((g) => t(g.heading))).toEqual(["Fleet", "Alerting", "Access", "AI & extensions", "System"]);
    expect(ids(groups)).toHaveLength(14);
  });

  it("keeps global pages from customer administrators and everything from other roles", () => {
    const visible = ids(visibleSettings(customerAdmin));
    expect(visible).toEqual(["sites", "relays", "add-servers", "notifications", "integrations", "users", "api-keys"]);
    expect(visibleSettings(viewer)).toEqual([]);
  });

  it("filters by name, description and keywords, every word, ignoring case", () => {
    const groups = visibleSettings(global);
    expect(ids(filterSettings(groups, "back", global))).toEqual(["backups"]);
    expect(ids(filterSettings(groups, "SLACK", global))).toEqual(["notifications"]);
    expect(ids(filterSettings(groups, "install command", global))).toEqual(["add-servers"]);
    expect(ids(filterSettings(groups, "  ", global))).toHaveLength(14);
    expect(filterSettings(groups, "nothing like this", global)).toEqual([]);
  });

  it("keeps a whole group when its heading matches", () => {
    expect(ids(filterSettings(visibleSettings(global), "access", global))).toEqual(["users", "api-keys"]);
  });

  it("words the sites line for one customer or several", () => {
    const sites = (user: typeof global) => visibleSettings(user)[0]!.entries[0]!.description!(user);
    expect(sites(global)).toMatch(/Customers/);
    expect(sites(single(global))).not.toMatch(/ustomer/);
  });

  it("finds the page a path is on, and tells settings pages from the list", () => {
    expect(currentSettingsEntry("/settings/relays/view/")?.id).toBe("relays");
    expect(currentSettingsEntry("/settings/api-keys/docs/")?.id).toBe("api-keys");
    expect(currentSettingsEntry("/incidents/")).toBeUndefined();
    expect(insideSettings("/settings/backups/")).toBe(true);
    expect(insideSettings("/settings/")).toBe(false);
    expect(insideSettings("/settings")).toBe(false);
    expect(insideSettings("/")).toBe(false);
  });
});
