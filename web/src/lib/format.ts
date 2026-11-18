import { t } from "@/i18n";
import { activeLocale } from "@/i18n/locale";

const OS_NAMES: Record<string, string> = {
  almalinux: "AlmaLinux",
  arch: "Arch Linux",
  centos: "CentOS",
  debian: "Debian",
  linux: "Linux",
  opensuse: "openSUSE",
  rhel: "RHEL",
  rocky: "Rocky Linux",
  windows: "Windows",
};

const osName = (id: string) => OS_NAMES[id] ?? id.charAt(0).toUpperCase() + id.slice(1);

/**
 * `debian` + `12` gives `Debian 12`. Agents send the distribution first, so
 * `debian` + `debian 12` does too, and `rhel` + `rocky 9.4` gives
 * `Rocky Linux 9.4`. A Windows version names the system already.
 */
export function formatOs(family: string, version: string | null): string {
  if (family === "device") return t("devices.word");
  if (!version) return osName(family);
  if (family === "windows") return version;
  const [first = "", ...rest] = version.split(" ");
  return /^\d/.test(first) ? `${osName(family)} ${version}` : [osName(first), ...rest].join(" ");
}


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
      return new Intl.RelativeTimeFormat(activeLocale(), { numeric: "auto" }).format(Math.trunc(seconds / size), unit);
    }
  }
  return t("format.justNow");
}

export function formatDateTime(iso: string): string {
  return new Date(iso).toLocaleString(activeLocale(), { dateStyle: "medium", timeStyle: "medium" });
}

/** How a metric's values read, from its name's last word (ADR-0015). */
export type Unit = "ratio" | "bytes" | "bytes_per_second" | "bits_per_second" | "per_second" | "seconds" | "celsius" | "number";

export function unitOf(metric: string): Unit {
  if (metric.endsWith("_ratio")) return "ratio";
  if (metric.endsWith("_bytes")) return "bytes";
  if (metric.endsWith("_bytes_per_second")) return "bytes_per_second";
  if (metric.endsWith("_bits_per_second")) return "bits_per_second";
  if (metric.endsWith("_celsius")) return "celsius";
  if (metric.endsWith("_per_second")) return "per_second";
  if (metric.endsWith("_seconds")) return "seconds";
  return "number";
}

/** The binary units, from bytes (0) to exbibytes (6), in the active language: "KiB", "Kio". */
const BINARY_UNITS = 7;

const binaryUnit = (exponent: number) => t(`format.bytes.${exponent as 0 | 1 | 2 | 3 | 4 | 5 | 6}`);

/** The power of 1024 to show `bytes` in: 1536 gives 1 (KiB). */
export function binaryExponent(bytes: number): number {
  const exponent = Math.floor(Math.log(Math.abs(bytes)) / Math.log(1024));
  return Number.isFinite(exponent) ? Math.min(Math.max(exponent, 0), BINARY_UNITS - 1) : 0;
}

/** `1536` gives `1.5 KiB`; `exponent` forces the unit, for axis ticks. */
export function formatBytes(bytes: number, exponent = binaryExponent(bytes)): string {
  const value = bytes / 1024 ** exponent;
  return `${formatNumber(value, exponent === 0 ? 0 : 1)} ${binaryUnit(exponent)}`;
}

/** A 0 to 1 ratio as a percentage: `0.853` gives `85.3%`, `85,3 %` in French. */
export function formatPercent(ratio: number): string {
  return ratio.toLocaleString(activeLocale(), { style: "percent", maximumFractionDigits: 1 });
}

export function formatNumber(value: number, maximumFractionDigits = 2): string {
  return value.toLocaleString(activeLocale(), { maximumFractionDigits });
}

/** A duration in seconds, in the largest unit that keeps it at least 1. */
export function formatSeconds(seconds: number): string {
  for (const [unit, size] of [["day", 86_400], ["hour", 3_600], ["minute", 60]] as const) {
    if (Math.abs(seconds) >= size) {
      return `${formatNumber(seconds / size, 1)} ${t(`format.unit.${unit}`)}`;
    }
  }
  return Math.abs(seconds) >= 1 || seconds === 0
    ? `${formatNumber(seconds, 1)} ${t("format.unit.second")}`
    : `${formatNumber(seconds * 1000, 1)} ${t("format.unit.millisecond")}`;
}

/** `value` in `unit`, or a dash when there is none. */
export function formatValue(value: number | null | undefined, unit: Unit): string {
  if (value === null || value === undefined) return "–";
  switch (unit) {
    case "ratio":
      return formatPercent(value);
    case "bytes":
      return formatBytes(value);
    case "bytes_per_second": {
      // Short enough for a chart's axis: 954 MiB/s, not 953.7 MiB/s.
      const exponent = binaryExponent(value);
      const scaled = value / 1024 ** exponent;
      return scaled >= 100 ? `${formatNumber(scaled, 0)} ${binaryUnit(exponent)}/s` : `${formatBytes(value)}/s`;
    }
    case "bits_per_second": {
      // Link speeds: 100 Mbit/s, 1 Gbit/s, 10 Gbit/s.
      const steps: [number, string][] = [[1e9, "Gbit/s"], [1e6, "Mbit/s"], [1e3, "kbit/s"]];
      const [size, word] = steps.find(([s]) => Math.abs(value) >= s) ?? [1, "bit/s"];
      return `${formatNumber(value / size, value % size === 0 ? 0 : 1)} ${word}`;
    }
    case "celsius":
      return `${formatNumber(value, Number.isInteger(value) ? 0 : 1)} °C`;
    case "per_second":
      return `${formatNumber(value, value < 10 ? 1 : 0)}/s`;
    case "seconds":
      return formatSeconds(value);
    case "number":
      return formatNumber(value);
  }
}

/** A duration in plain words, in the largest unit it fits exactly: `12 hours`, `90 minutes`. */
export function formatDuration(seconds: number): string {
  if (seconds >= 86_400 && seconds % 86_400 === 0) return t("format.days", { count: seconds / 86_400 });
  if (seconds >= 3_600 && seconds % 3_600 === 0) return t("format.hours", { count: seconds / 3_600 });
  if (seconds >= 60 && seconds % 60 === 0) return t("format.minutes", { count: seconds / 60 });
  return t("format.seconds", { count: seconds });
}
