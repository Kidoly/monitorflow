import { t } from "@/i18n";

import type { EffectiveThreshold } from "./api";

export type Severity = "warning" | "critical";

/** "Warning", "Critical", in the active language: read when shown. */
export const SEVERITY_LABELS: Record<Severity, string> = {
  get warning() {
    return t("sensors.severity.warning");
  },
  get critical() {
    return t("sensors.severity.critical");
  },
};

/**
 * The threshold the engine applies to one series, as ADR-0017 resolves it:
 * the exact instance's entry wins over the one for every instance, even when
 * it is muted. Muted entries give nothing.
 */
export function thresholdFor(
  thresholds: EffectiveThreshold[],
  metric: string,
  instance: string,
): EffectiveThreshold | undefined {
  // Only plain thresholds are in the metric's unit; a forecast judges seconds (ADR-0153).
  const ofMetric = thresholds.filter((t) => t.metric === metric && (t.kind ?? "value") === "value");
  const rule = ofMetric.find((t) => t.instance === instance) ?? ofMetric.find((t) => t.instance === null);
  return rule?.enabled ? rule : undefined;
}
