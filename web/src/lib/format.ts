const OS_NAMES: Record<string, string> = { debian: "Debian", rhel: "RHEL", windows: "Windows" };

/** `debian` + `12` gives `Debian 12`. A Windows version names the product already. */
export function formatOs(family: string, version: string | null): string {
  const name = OS_NAMES[family] ?? family;
  if (family === "windows" && version) return version;
  return version ? `${name} ${version}` : name;
}

const relative = new Intl.RelativeTimeFormat("en", { numeric: "auto" });

const UNITS: [Intl.RelativeTimeFormatUnit, number][] = [
  ["day", 86_400],
  ["hour", 3_600],
  ["minute", 60],
];

/** `iso` relative to `now`, like "5 minutes ago". Under a minute is "just now". */
export function formatRelative(iso: string, now: Date): string {
  const seconds = Math.round((new Date(iso).getTime() - now.getTime()) / 1000);
  for (const [unit, size] of UNITS) {
    if (Math.abs(seconds) >= size) {
      return relative.format(Math.trunc(seconds / size), unit);
    }
  }
  return "just now";
}

export function formatDateTime(iso: string): string {
  return new Date(iso).toLocaleString("en", { dateStyle: "medium", timeStyle: "medium" });
}

/** How a metric's values read, from its name's last word (ADR-0015). */
export type Unit = "ratio" | "bytes" | "seconds" | "number";

export function unitOf(metric: string): Unit {
  if (metric.endsWith("_ratio")) return "ratio";
  if (metric.endsWith("_bytes")) return "bytes";
  if (metric.endsWith("_seconds")) return "seconds";
  return "number";
}

const BINARY_UNITS = ["B", "KiB", "MiB", "GiB", "TiB", "PiB", "EiB"];

/** The power of 1024 to show `bytes` in: 1536 gives 1 (KiB). */
export function binaryExponent(bytes: number): number {
  const exponent = Math.floor(Math.log(Math.abs(bytes)) / Math.log(1024));
  return Number.isFinite(exponent) ? Math.min(Math.max(exponent, 0), BINARY_UNITS.length - 1) : 0;
}

/** `1536` gives `1.5 KiB`; `exponent` forces the unit, for axis ticks. */
export function formatBytes(bytes: number, exponent = binaryExponent(bytes)): string {
  const value = bytes / 1024 ** exponent;
  return `${formatNumber(value, exponent === 0 ? 0 : 1)} ${BINARY_UNITS[exponent]}`;
}

/** A 0 to 1 ratio as a percentage: `0.853` gives `85.3%`. */
export function formatPercent(ratio: number): string {
  return `${formatNumber(ratio * 100, 1)}%`;
}

export function formatNumber(value: number, maximumFractionDigits = 2): string {
  return value.toLocaleString("en", { maximumFractionDigits });
}

/** A duration in seconds, in the largest unit that keeps it at least 1. */
export function formatSeconds(seconds: number): string {
  for (const [unit, size] of [["d", 86_400], ["h", 3_600], ["min", 60]] as const) {
    if (Math.abs(seconds) >= size) {
      return `${formatNumber(seconds / size, 1)} ${unit}`;
    }
  }
  return Math.abs(seconds) >= 1 || seconds === 0
    ? `${formatNumber(seconds, 1)} s`
    : `${formatNumber(seconds * 1000, 1)} ms`;
}

/** `value` in `unit`, or a dash when there is none. */
export function formatValue(value: number | null | undefined, unit: Unit): string {
  if (value === null || value === undefined) return "–";
  switch (unit) {
    case "ratio":
      return formatPercent(value);
    case "bytes":
      return formatBytes(value);
    case "seconds":
      return formatSeconds(value);
    case "number":
      return formatNumber(value);
  }
}
