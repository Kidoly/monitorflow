import type { Rollback } from "./api";

export type SnapshotSet = Rollback["local_snapshots"][number];

/** "/", "/ and /var", "/, /usr and /var". */
export function listWords(items: string[]): string {
  if (items.length <= 1) return items.join("");
  return `${items.slice(0, -1).join(", ")} and ${items[items.length - 1]}`;
}

/** The mount points a snapshot covers, or its origins when they are not mounted. */
export function covers(set: SnapshotSet): string {
  const points = [...new Set(set.volumes.map((v) => v.mount_point || v.origin))];
  return listWords(points);
}

/** "8.0 GiB": binary units with one decimal, as the server writes them. */
export function formatBytes(bytes: number): string {
  const units = ["B", "KiB", "MiB", "GiB", "TiB"];
  let value = bytes;
  let unit = 0;
  while (value >= 1024 && unit < units.length - 1) {
    value /= 1024;
    unit += 1;
  }
  return unit === 0 ? `${bytes} B` : `${value.toFixed(1)} ${units[unit]}`;
}

/** The space a snapshot holds: classic LVM reserves it and can fill up. */
export function snapshotSpace(set: SnapshotSet): string {
  if (set.volumes.some((v) => v.invalid)) return "Full: can't be rolled back to";
  const classic = set.volumes.filter((v) => v.kind === "lvm");
  if (classic.length > 0) {
    const size = classic.reduce((sum, v) => sum + v.size_bytes, 0);
    const used = Math.max(...classic.map((v) => v.used_percent));
    return `${formatBytes(size)}, ${Math.round(used)} % used`;
  }
  if (set.kind === "zfs") return formatBytes(set.volumes.reduce((sum, v) => sum + v.size_bytes, 0));
  return "Shares the thin pool";
}

/** How to roll a server back to `set`: what to know, then the commands, run as root (ADR-0094). */
export function rollbackSteps(set: SnapshotSet): { intro: string; commands: string[] } {
  if (set.kind === "zfs") {
    return {
      intro:
        "A mounted / can't be rolled back, so boot a rescue system first and import the pool, then run these as root. Everything written to these datasets since the snapshot is lost. zfs refuses while a dataset has newer snapshots; delete them first if you are sure.",
      commands: set.volumes.map((v) => `zfs rollback ${v.source}`),
    };
  }
  return {
    intro:
      "Run these as root on the server. LVM merges the snapshot into each volume at the next boot, as they are in use. Everything written to them since the snapshot is lost, logs included.",
    commands: [...set.volumes.map((v) => `lvconvert --merge ${v.source}`), "reboot"],
  };
}
