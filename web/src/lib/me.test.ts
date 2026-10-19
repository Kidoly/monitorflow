import { describe, expect, it } from "vitest";

import type { Me, RoleGrant } from "./api";
import { administersCustomer, isAdministrator } from "./me";

const me = (...roles: RoleGrant[]): Me => ({ id: "u1", username: "alice", roles });

describe("administration rights", () => {
  it("shows the section for any administrator grant", () => {
    expect(isAdministrator(null)).toBe(false);
    expect(isAdministrator(me({ role: "operator", site_id: null }))).toBe(false);
    expect(isAdministrator(me({ role: "administrator", site_id: "s1" }))).toBe(true);
  });

  it("needs a global or customer grant to administer a customer", () => {
    const site = me({ role: "administrator", customer_id: null, site_id: "s1" });
    expect(administersCustomer(site, "c1")).toBe(false);
    expect(administersCustomer(site)).toBe(false);
    const customer = me({ role: "administrator", customer_id: "c1", site_id: null });
    expect(administersCustomer(customer, "c1")).toBe(true);
    expect(administersCustomer(customer, "c2")).toBe(false);
    expect(administersCustomer(customer)).toBe(true);
    const global = me({ role: "administrator", customer_id: null, site_id: null });
    expect(administersCustomer(global, "c2")).toBe(true);
    expect(administersCustomer(me({ role: "operator", site_id: null }), "c1")).toBe(false);
  });
});
