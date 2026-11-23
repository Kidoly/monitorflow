import { t } from "@/i18n";
import type { FlowDimension, FlowView } from "@/lib/api";
import { formatValue } from "@/lib/format";
import type { TimeSeries } from "@/components/ui/TimeChart";

/** Traffic flows of a device (ADR-0184): what the Flows tab reads and shows. */

/** Periods offered, in hours. */
export const FLOW_PERIODS = ["1h", "6h", "24h", "7d", "30d"] as const;
export type FlowPeriod = (typeof FLOW_PERIODS)[number];

const PERIOD_HOURS: Record<FlowPeriod, number> = { "1h": 1, "6h": 6, "24h": 24, "7d": 168, "30d": 720 };

export const FLOW_DIMENSIONS: readonly FlowDimension[] = ["src", "dst", "app", "protocol", "in", "out"];

/** Dimensions whose keys are addresses, which a click filters on. */
export const isAddressDimension = (d: FlowDimension): boolean => d === "src" || d === "dst";

export function isFlowPeriod(value: string | null): value is FlowPeriod {
  return value !== null && (FLOW_PERIODS as readonly string[]).includes(value);
}

export function isFlowDimension(value: string | null): value is FlowDimension {
  return value !== null && (FLOW_DIMENSIONS as readonly string[]).includes(value);
}

/** The period ending at `now`, as the API takes it. */
export function periodRange(period: FlowPeriod, now: Date): { from: string; to: string } {
  const to = new Date(Math.floor(now.getTime() / 60_000) * 60_000);
  const from = new Date(to.getTime() - PERIOD_HOURS[period] * 3_600_000);
  return { from: from.toISOString(), to: to.toISOString() };
}

export const formatBits = (bitsPerSecond: number): string => formatValue(bitsPerSecond, "bits_per_second");

/** A key as people read it: an application's or interface's name with its key, or the key. */
export function keyText(dimension: FlowDimension, key: string, label: string | null): string {
  if ((dimension === "in" || dimension === "out") && key === "0") return t("flows.key.noInterface");
  if (dimension === "in" || dimension === "out") {
    return label ? t("flows.key.interface", { name: label, index: key }) : t("flows.key.ifIndex", { index: key });
  }
  if (label) return t("flows.key.named", { name: label, key });
  return key;
}

/** The chart's keys and the rest as stacked series, in bits per second. */
export function chartSeries(view: FlowView): TimeSeries[] {
  const time = (at: string) => Math.floor(new Date(at).getTime() / 1000);
  const series: TimeSeries[] = view.chart.keys.map((k, i) => ({
    label: keyText(view.chart.by, k.key, k.label),
    points: view.chart.points.map((p): [number, number] => [time(p.at), p.values[i] ?? 0]),
  }));
  if (view.chart.points.some((p) => p.other > 0)) {
    series.push({ label: t("flows.other"), points: view.chart.points.map((p): [number, number] => [time(p.at), p.other]) });
  }
  return series;
}

/** The share of the period's bytes, 0 to 1. */
export function share(bytes: number, total: number): number {
  return total > 0 ? Math.min(bytes / total, 1) : 0;
}

/** A rate typed in Mbit/s, as bits per second; null when not a positive number. */
export function parseMbits(text: string): number | null {
  const value = Number(text.trim().replace(",", "."));
  return text.trim() !== "" && Number.isFinite(value) && value > 0 ? value * 1e6 : null;
}

/** The metric of each address's traffic (ADR-0184). */
export const ADDRESS_METRIC = "monitorflow_flow_address_bits_per_second";
