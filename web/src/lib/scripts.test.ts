import { describe, expect, it } from "vitest";

import type { ScriptParameter } from "./api";
import { NEW_PARAMETER, envName, parametersOf, runProgress, runStatus, scheduledAt, targetOf, valuesOf } from "./scripts";

const days: ScriptParameter = { name: "days", label: "Days", type: "integer", required: true, default: null, choices: [] };
const mode: ScriptParameter = { name: "mode", label: "", type: "choice", required: false, default: "fast", choices: ["fast", "full"] };

describe("script parameters", () => {
  it("turn the editor's rows into the API's, or say why not", () => {
    expect(parametersOf([{ ...NEW_PARAMETER, name: " days ", label: "Days ", type: "integer", required: true }])).toEqual([days]);
    expect(parametersOf([{ ...NEW_PARAMETER, name: "mode", type: "choice", choices: "fast\n\n full ", defaultValue: "fast" }])).toEqual([mode]);
    expect(typeof parametersOf([{ ...NEW_PARAMETER, name: "Bad" }])).toBe("string");
    expect(typeof parametersOf([{ ...NEW_PARAMETER, name: "a" }, { ...NEW_PARAMETER, name: "a" }])).toBe("string");
    expect(typeof parametersOf([{ ...NEW_PARAMETER, name: "m", type: "choice" }])).toBe("string");
    expect(typeof parametersOf([{ ...NEW_PARAMETER, name: "n", type: "integer", defaultValue: "x" }])).toBe("string");
  });

  it("check a run's values and leave empty ones to the defaults", () => {
    expect(valuesOf([days, mode], { days: " 7 " })).toEqual({ days: "7" });
    expect(typeof valuesOf([days, mode], {})).toBe("string");
    expect(typeof valuesOf([days], { days: "seven" })).toBe("string");
    expect(typeof valuesOf([mode], { mode: "slow" })).toBe("string");
    expect(envName("max_age")).toBe("MF_PARAM_MAX_AGE");
  });
});

describe("script runs", () => {
  it("name their target, or why there is none", () => {
    expect(targetOf("servers", { servers: ["s1"], site: "", service: "" })).toEqual({ kind: "servers", server_ids: ["s1"] });
    expect(targetOf("site", { servers: [], site: "x", service: "" })).toEqual({ kind: "site", site_id: "x" });
    expect(typeof targetOf("business_service", { servers: [], site: "", service: "" })).toBe("string");
    expect(scheduledAt("")).toBeUndefined();
    expect(scheduledAt("2026-12-04T22:00")).toMatch(/^2026-12-0[45]T/);
  });

  it("read their progress", () => {
    const counts = { servers: 2, pending: 0, succeeded: 1, failed: 1 };
    expect(runStatus({ state: "dispatched", counts })).toBe("failed");
    expect(runStatus({ state: "dispatched", counts: { ...counts, pending: 1, failed: 0 } })).toBe("running");
    expect(runStatus({ state: "awaiting_approval", counts })).toBe("awaiting_approval");
    expect(runProgress({ state: "dispatched", counts })).toBe("1 of 2 servers failed");
    expect(runProgress({ state: "scheduled", counts })).toBe("2 servers");
  });
});
