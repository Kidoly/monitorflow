import { t } from "@/i18n";
import { activeLocale } from "@/i18n/locale";

import type { Rollback } from "./api";

export type SnapshotSet = Rollback["local_snapshots"][number];

/** "/", "/ and /var", "/, /usr and /var". */
export function listWords(items: string[]): string {
  if (items.length <= 1) return items.join("");
  return t("rollback.and", { items: items.slice(0, -1).join(", "), last: items[items.length - 1]! });
}

/** The mount points a snapshot covers, or its origins when they are not mounted. */
export function covers(set: SnapshotSet): string {
  const points = [...new Set(set.volumes.map((v) => v.mount_point || v.origin))];
  return listWords(points);
}

/** "8.0 GiB" ("8,0 Gio" in French): binary units with one decimal, as the server writes them. */
export function formatBytes(bytes: number): string {
  let value = bytes;
  let unit: 0 | 1 | 2 | 3 | 4 = 0;
  while (value >= 1024 && unit < 4) {
    value /= 1024;
    unit = (unit + 1) as 1 | 2 | 3 | 4;
  }
  if (unit === 0) return `${bytes} ${t("format.bytes.0")}`;
  const number = value.toLocaleString(activeLocale(), { minimumFractionDigits: 1, maximumFractionDigits: 1 });
  return `${number} ${t(`format.bytes.${unit}`)}`;
}

/** The space a snapshot holds: classic LVM reserves it and can fill up. */
export function snapshotSpace(set: SnapshotSet): string {
  if (set.volumes.some((v) => v.invalid)) return t("rollback.full");
  const classic = set.volumes.filter((v) => v.kind === "lvm");
  if (classic.length > 0) {
    const size = classic.reduce((sum, v) => sum + v.size_bytes, 0);
    const used = Math.max(...classic.map((v) => v.used_percent));
    return t("rollback.used", { size: formatBytes(size), percent: Math.round(used) });
  }
  if (set.kind === "zfs") return formatBytes(set.volumes.reduce((sum, v) => sum + v.size_bytes, 0));
  return t("rollback.thinPool");
}

/** How to roll a server back to `set`: what to know, then the commands, run as root (ADR-0094). */
export function rollbackSteps(set: SnapshotSet): { intro: string; commands: string[] } {
  if (set.kind === "zfs") {
    return {
      intro: t("rollback.steps.zfs"),
      commands: set.volumes.map((v) => `zfs rollback ${v.source}`),
    };
  }
  return {
    intro: t("rollback.steps.lvm"),
    commands: [...set.volumes.map((v) => `lvconvert --merge ${v.source}`), "reboot"],
  };
}
