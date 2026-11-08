import { describe, expect, it } from "vitest";

import type { Customer, NotificationChannel, Site } from "@/lib/api";
import { customerAdmin, me, single, siteAdmin } from "@/test/admin";

import { channelScopes, scopeName, scopeValue } from "./notifications";

const global = me({ role: "administrator", customer_id: null, site_id: null });

const customer = (id: string, name: string) => ({ id, name, site_count: 1 }) as Customer;
const site = (id: string, name: string, customer_id: string) => ({ id, name, customer_id }) as Site;
const acme = customer("c1", "acme");
const globex = customer("c2", "globex");
const paris = site("site-1", "paris", "c1");
const lyon = site("site-2", "lyon", "c1");
const berlin = site("site-3", "berlin", "c2");
const channel = (fields: Partial<NotificationChannel>) =>
  ({ customer_id: null, customer_name: null, site_id: null, site_name: null, ...fields }) as NotificationChannel;
const labels = (...args: Parameters<typeof channelScopes>) => channelScopes(...args).map((s) => s.label);

describe("channelScopes", () => {
  it("keeps a small install to Everything", () => {
    expect(labels(single(global), [acme], [paris])).toEqual(["Everything"]);
    expect(labels(single(global), [acme], [paris, lyon])).toEqual(["Everything", "paris", "lyon"]);
  });

  it("names customers when there are several", () => {
    expect(labels(global, [acme, globex], [paris, berlin])).toEqual([
      "Everything",
      "All of acme",
      "acme / paris",
      "All of globex",
      "globex / berlin",
    ]);
  });

  it("offers only what the user administers, never Everything below global", () => {
    expect(labels(customerAdmin, [acme, globex], [paris, lyon, berlin])).toEqual([
      "All of acme",
      "acme / paris",
      "acme / lyon",
    ]);
    // An MSP's customer reads no customer wording.
    expect(labels(single(customerAdmin), [acme], [paris, lyon])).toEqual(["All sites", "paris", "lyon"]);
    expect(labels(single(siteAdmin), [acme], [paris, lyon])).toEqual(["paris"]);
    const viewerOfGlobex = me(
      { role: "administrator", customer_id: "c1", site_id: null },
      { role: "viewer", customer_id: "c2", site_id: null },
    );
    expect(labels(viewerOfGlobex, [acme, globex], [paris, berlin])).toEqual(["All of acme", "acme / paris"]);
  });

  it("maps each choice to one column", () => {
    const columns = channelScopes(global, [acme, globex], [paris]).map((s) => [s.customer_id, s.site_id]);
    expect(columns.slice(0, 3)).toEqual([
      [null, null],
      ["c1", null],
      [null, "site-1"],
    ]);
  });

  it("keeps the current scope of an edited channel as a choice", () => {
    const hidden = channel({ customer_id: "c1", customer_name: "default" });
    expect(labels(single(global), [acme], [paris], hidden)).toEqual(["Everything", "All sites"]);
    const gone = channel({ customer_id: "c1", customer_name: "acme", site_id: "site-9", site_name: "old" });
    expect(labels(single(global), [acme], [], gone)).toEqual(["Everything", "old"]);
  });
});

describe("a saved channel", () => {
  it("names its scope", () => {
    const onSite = channel({ customer_id: "c1", customer_name: "acme", site_id: "s", site_name: "paris" });
    expect(scopeValue(onSite)).toBe("site:s");
    expect(scopeName(onSite, true)).toBe("acme / paris");
    expect(scopeName(onSite, false)).toBe("paris");
    const onCustomer = channel({ customer_id: "c1", customer_name: "acme" });
    expect(scopeValue(onCustomer)).toBe("customer:c1");
    expect(scopeName(onCustomer, true)).toBe("acme");
    expect(scopeName(onCustomer, false)).toBe("All sites");
    expect(scopeValue(channel({}))).toBe("everything");
    expect(scopeName(channel({}), true)).toBe("Everything");
  });
});
