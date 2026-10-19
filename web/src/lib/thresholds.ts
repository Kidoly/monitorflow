import type { EffectiveThreshold } from "./api";

export type Severity = "warning" | "critical";

export const SEVERITY_LABELS: Record<Severity, string> = { warning: "Warning", critical: "Critical" };

/** A threshold value to draw on a chart, with its label. */
export interface ThresholdLine {
  severity: Severity;
  value: number;
  label: string;
}

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
  const ofMetric = thresholds.filter((t) => t.metric === metric);
  const rule = ofMetric.find((t) => t.instance === instance) ?? ofMetric.find((t) => t.instance === null);
  return rule?.enabled ? rule : undefined;
}

/** Instance names shown in a line's label before "and N more". */
const MAX_NAMES = 2;

/**
 * Warning and critical lines for a chart of `metric` showing `instances`.
 * A value shared by every instance is drawn once with a plain label; a value
 * that only some instances have names them, so a chart with several
 * instances gets one line per distinct value, not one per instance.
 */
export function thresholdLines(
  thresholds: EffectiveThreshold[],
  metric: string,
  instances: string[],
): ThresholdLine[] {
  const groups = new Map<string, { severity: Severity; value: number; instances: string[] }>();
  for (const instance of instances) {
    const rule = thresholdFor(thresholds, metric, instance);
    for (const severity of ["warning", "critical"] as const) {
      const value = rule?.[severity];
      if (value === null || value === undefined) continue;
      const key = `${severity}:${value}`;
      const group = groups.get(key) ?? { severity, value, instances: [] };
      group.instances.push(instance);
      groups.set(key, group);
    }
  }
  return [...groups.values()].map(({ severity, value, instances: covered }) => {
    let label = SEVERITY_LABELS[severity];
    if (covered.length < instances.length) {
      const names = covered.slice(0, MAX_NAMES).map((i) => i || "no instance");
      const more = covered.length - names.length;
      label += ` (${names.join(", ")}${more > 0 ? ` and ${more} more` : ""})`;
    }
    return { severity, value, label };
  });
}
