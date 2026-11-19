import { afterEach, describe, expect, it } from "vitest";

import { setActiveLocale } from "@/i18n/locale";

import type { EffectiveThreshold } from "./api";
import { type Shape, formOf } from "./alerts";
import { describeRule, formOfRule, incidentValue, judgedValue, kindsFor, linesInMetricUnit, requestOf, ruleRequest } from "./rules";

const DISK = "monitorflow_disk_used_ratio";
const diskShape: Shape = { type: "percent", noun: "a disk", suffix: " full" };

const rule = (overrides: Partial<EffectiveThreshold> = {}): EffectiveThreshold => ({
  origin: "default",
  threshold_id: null,
  metric: DISK,
  instance: null,
  enabled: true,
  comparison: ">",
  warning: 0.85,
  critical: 0.95,
  duration_s: 300,
  hysteresis: 0.02,
  ...overrides,
});

const forecast = rule({ kind: "forecast", comparison: ">=", warning: 259_200, critical: 86_400, duration_s: 0, window_s: 86_400, target: 1 });

afterEach(() => setActiveLocale("en"));

describe("describeRule", () => {
  it("says what each kind judges", () => {
    expect(describeRule(diskShape, rule())).toBe("Warn when a disk is over 85% full, critical over 95%, for 5 min.");
    expect(describeRule(diskShape, forecast)).toBe("Warn 3 d before a disk reaches 100% full, critical 1 d before.");
    expect(describeRule(diskShape, rule({ kind: "window", function: "avg", window_s: 900 }))).toBe(
      "On average over 15 min, warn when a disk is over 85% full, critical over 95%, for 5 min.",
    );
    expect(describeRule(diskShape, rule({ kind: "nodata", warning: 300, critical: null, duration_s: 0 }))).toBe(
      "Warn when a disk sends nothing for 5 min.",
    );
    expect(
      describeRule(diskShape, rule({ kind: "compare", function: "ratio", offset_s: 604_800, window_s: 3600, warning: 2, critical: null, duration_s: 0 })),
    ).toBe("Compared with last week, averaged over 1 h, warn at 2 times.");
    expect(
      describeRule(
        diskShape,
        rule({ conditions: [{ metric: "monitorflow_cpu_load1", instance: null, function: "value", comparison: ">", value: 8 }] }),
      ),
    ).toMatch(/for 5 min\. Only while .* is over 8\.$/);
  });

  it("speaks French", () => {
    setActiveLocale("fr");
    expect(describeRule({ ...diskShape, noun: "un disque", suffix: " plein" }, forecast)).toMatch(
      /^Avertir quand un disque sera à 100\s%\splein dans moins de 3 j, critique dans moins de 1 j\.$/,
    );
  });
});

describe("values", () => {
  it("read in the rule's words", () => {
    expect(judgedValue(DISK, { kind: "forecast" }, 172_800)).toBe("2 d left");
    expect(judgedValue(DISK, { kind: "forecast" }, 0)).toBe("reached");
    expect(judgedValue(DISK, { kind: "nodata" }, 720)).toBe("silent for 12 min");
    expect(judgedValue(DISK, { kind: "compare", function: "ratio" }, 2.5)).toBe("2.5 times");
    expect(judgedValue(DISK, { kind: "value" }, 0.9)).toBe("90%");
    expect(incidentValue({ metric: DISK, kind: "forecast", threshold: {} }, 3600)).toBe("1 h left");
    expect(linesInMetricUnit({ kind: "forecast" })).toBe(false);
    expect(linesInMetricUnit({ kind: "window", function: "max" })).toBe(true);
    expect(linesInMetricUnit({})).toBe(true);
  });
});

describe("ruleRequest", () => {
  const form = (kind: Parameters<typeof formOfRule>[3], base = rule()) => formOfRule(diskShape, base, formOf(diskShape, base), kind);

  it("builds each kind with its defaults", () => {
    expect(ruleRequest(diskShape, rule(), form("forecast"))).toEqual({
      request: {
        metric: DISK,
        instance: null,
        enabled: true,
        kind: "forecast",
        comparison: ">=",
        warning: 259_200,
        critical: 86_400,
        duration_s: 0,
        hysteresis: 3600,
        window_s: 86_400,
        target: 1,
      },
    });
    expect(ruleRequest(diskShape, rule(), form("nodata"))).toMatchObject({
      request: { kind: "nodata", comparison: ">", warning: 600, critical: null, duration_s: 0 },
    });
    expect(ruleRequest(diskShape, rule(), form("window"))).toMatchObject({
      request: { kind: "window", function: "avg", window_s: 900, warning: 0.85, critical: 0.95 },
    });
    expect(ruleRequest(diskShape, rule(), form("compare"))).toMatchObject({
      request: { kind: "compare", function: "ratio", window_s: 3600, offset_s: 604_800, warning: 2, critical: null },
    });
    // A plain threshold is sent as before, without a kind.
    expect(ruleRequest(diskShape, rule(), form("value"))).toEqual({
      request: expect.not.objectContaining({ kind: expect.anything() }),
    });
  });

  it("reads back a stored forecast and refuses wrong fields", () => {
    const own = form("forecast", forecast);
    expect([own.warning, own.critical, own.learn, own.target]).toEqual(["72", "24", "24", "100"]);
    expect(ruleRequest(diskShape, forecast, { ...own, critical: "96" })).toEqual({
      errors: { critical: "Critical must come sooner than warning." },
    });
    expect(ruleRequest(diskShape, forecast, { ...own, learn: "0" })).toMatchObject({ errors: { learn: expect.any(String) } });
    const silent = form("nodata");
    expect(ruleRequest(diskShape, rule(), { ...silent, warning: "20", critical: "10" })).toMatchObject({
      errors: { critical: "Critical must be a longer silence than warning." },
    });
    const window = form("window");
    expect(ruleRequest(diskShape, rule(), { ...window, window: "0" })).toMatchObject({ errors: { window: expect.any(String) } });
  });

  it("adds conditions, in percent for ratios", () => {
    const withConditions = { ...form("value"), conditions: [{ metric: "monitorflow_memory_available_ratio", comparison: "<" as const, value: "10" }] };
    expect(ruleRequest(diskShape, rule(), withConditions)).toMatchObject({
      request: { conditions: [{ metric: "monitorflow_memory_available_ratio", instance: null, function: "value", comparison: "<", value: 0.1 }] },
    });
    expect(ruleRequest(diskShape, rule(), { ...withConditions, conditions: [{ ...withConditions.conditions[0]!, value: "x" }] })).toMatchObject({
      errors: { conditions: "Enter a number." },
    });
  });

  it("offers events their trigger and silence only, and writes a rule back as it is", () => {
    expect(kindsFor({ type: "event", noun: "port 443", text: "closes", brief: "", comparison: "<", value: 1 })).toEqual(["value", "nodata"]);
    expect(kindsFor(diskShape)).toHaveLength(5);
    expect(requestOf(forecast, false)).toEqual({
      metric: DISK,
      instance: null,
      kind: "forecast",
      comparison: ">=",
      warning: 259_200,
      critical: 86_400,
      duration_s: 0,
      hysteresis: 0.02,
      enabled: false,
      window_s: 86_400,
      target: 1,
    });
    expect(requestOf(rule(), true)).not.toHaveProperty("kind");
  });
});
