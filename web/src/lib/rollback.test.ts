import { describe, expect, it } from "vitest";

import type { LocalSnapshotVolume } from "./api";
import { type SnapshotSet, covers, formatBytes, listWords, rollbackSteps, snapshotSpace } from "./rollback";

const GIB = 1 << 30;

function set(kind: SnapshotSet["kind"], volumes: Partial<LocalSnapshotVolume>[]): SnapshotSet {
  return {
    name: "monitorflow-x",
    kind,
    created_at: "2026-11-03T09:00:00Z",
    delete_after: "2026-11-04T09:00:00Z",
    volumes: volumes.map((v) => ({
      name: "monitorflow-x",
      mount_point: "/",
      kind,
      source: "vg0/monitorflow-x_root",
      origin: "vg0/root",
      created_at: "2026-11-03T09:00:00Z",
      size_bytes: 0,
      used_percent: 0,
      invalid: false,
      ...v,
    })),
  };
}

describe("rollback", () => {
  it("lists words and bytes", () => {
    expect(listWords([])).toBe("");
    expect(listWords(["/"])).toBe("/");
    expect(listWords(["/", "/usr", "/var"])).toBe("/, /usr and /var");
    expect(formatBytes(512)).toBe("512 B");
    expect(formatBytes(8 * GIB)).toBe("8.0 GiB");
  });

  it("says what a snapshot covers and holds", () => {
    const zfs = set("zfs", [
      { source: "rpool/ROOT/debian@monitorflow-x", size_bytes: GIB },
      { mount_point: "", origin: "rpool/ROOT/debian/var/lib", source: "rpool/ROOT/debian/var/lib@monitorflow-x", size_bytes: GIB },
    ]);
    expect(covers(zfs)).toBe("/ and rpool/ROOT/debian/var/lib");
    expect(snapshotSpace(zfs)).toBe("2.0 GiB");
    expect(snapshotSpace(set("lvm", [{ size_bytes: 8 * GIB, used_percent: 12.34 }]))).toBe("8.0 GiB, 12 % used");
    expect(snapshotSpace(set("lvm", [{ invalid: true }]))).toBe("Full: can't be rolled back to");
    expect(snapshotSpace(set("lvm_thin", [{}]))).toBe("Shares the thin pool");
  });

  it("gives the commands to roll back", () => {
    expect(rollbackSteps(set("lvm", [{}, { mount_point: "/var", source: "vg0/monitorflow-x_var" }])).commands).toEqual([
      "lvconvert --merge vg0/monitorflow-x_root",
      "lvconvert --merge vg0/monitorflow-x_var",
      "reboot",
    ]);
    const zfs = rollbackSteps(set("zfs", [{ source: "rpool/ROOT/debian@monitorflow-x" }]));
    expect(zfs.commands).toEqual(["zfs rollback rpool/ROOT/debian@monitorflow-x"]);
    expect(zfs.intro).toMatch(/rescue system/);
  });
});
