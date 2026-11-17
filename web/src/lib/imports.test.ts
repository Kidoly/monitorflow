import { beforeEach, describe, expect, it } from "vitest";

import { setActiveLocale } from "@/i18n/locale";

import type { ImportItem } from "./api";
import { countsText, grouped, itemTitle, leftOut, noteText, originKind, toCreate } from "./imports";

const item = (id: string, needs: string[] = [], extra: Partial<ImportItem> = {}): ImportItem => ({
  id,
  kind: "check",
  list: "exact",
  origin: { kind: "item", name: id },
  hosts: [],
  present: false,
  needs,
  create: { object: "check", server: "a", kind: "port", name: "port_22", interval_ms: 60000, params: { port: "22" } },
  ...extra,
});

describe("imports", () => {
  beforeEach(() => setActiveLocale("en"));

  it("leaves out what needs an item left out, transitively", () => {
    const items = [item("server"), item("check", ["server"]), item("alert", ["check", "server"]), item("other")];
    expect([...leftOut(items, new Set(["server"]))]).toEqual(["server", "check", "alert"]);
    expect(toCreate(items, leftOut(items, new Set(["check"])))).toBe(2);
    const present = [item("p", [], { present: true }), item("n", [], { list: "not_movable", create: undefined })];
    expect(toCreate(present, new Set())).toBe(0);
  });

  it("translates notes and kinds, and falls back to the server's words", () => {
    expect(noteText({ code: "udp", text: "english" })).toBe("MonitorFlow checks TCP ports only.");
    expect(noteText({ code: "from_a_newer_server", text: "As the server says." })).toBe("As the server says.");
    expect(originKind("trigger")).toBe("Trigger");
    expect(originKind("sensor_group")).toBe("sensor_group");
    setActiveLocale("fr");
    expect(noteText({ code: "web_as_port", args: { port: "443", host: "shop" }, text: "" })).toContain("le port 443 de shop");
  });

  it("titles what items create", () => {
    const threshold: ImportItem = item("t", [], {
      kind: "threshold",
      create: {
        object: "threshold",
        server: "a",
        metric: "monitorflow_cpu_usage_ratio",
        instance: "total",
        comparison: ">",
        warning: 0.8,
        critical: 0.95,
        duration_s: 300,
        hysteresis: 0,
      },
    });
    const checks = [item("c")];
    expect(itemTitle(checks[0]!, checks)).toBe("Port 22");
    expect(itemTitle(threshold, [threshold])).toMatch(/80%.*95%/);
    const channel = item("ch", [], {
      kind: "channel",
      create: { object: "channel", kind: "email", settings: { to: ["ops@example.com"] }, has_secret: true, site_id: null },
    });
    expect(itemTitle(channel, [])).toBe("Email to ops@example.com");
  });

  it("groups by list and kind, in order", () => {
    const items = [item("c"), item("s", [], { kind: "server" }), item("x", [], { list: "approximated" })];
    expect(grouped(items, "exact").map((g) => g.kind)).toEqual(["server", "check"]);
    expect(grouped(items, "not_movable")).toEqual([]);
    expect(countsText({ server: 1, check: 2 })).toBe("1 server, 2 checks");
    expect(countsText({})).toBe("nothing");
  });
});
