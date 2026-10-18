const OS_NAMES: Record<string, string> = { debian: "Debian", rhel: "RHEL" };

/** `debian` + `12` gives `Debian 12`. */
export function formatOs(family: string, version: string | null): string {
  const name = OS_NAMES[family] ?? family;
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
