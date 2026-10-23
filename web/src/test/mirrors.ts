import type { Mirror, MirrorDetail, MirrorRun, MirrorSnapshot } from "@/lib/api";

export const MIRROR_ID = "7d4e1f20-5b6c-4a7d-9e8f-0a1b2c3d4e5f";

export function mirror(overrides: Partial<Mirror> = {}): Mirror {
  return {
    id: MIRROR_ID,
    name: "debian-bookworm",
    family: "apt",
    upstream_url: "https://deb.debian.org/debian",
    distribution: "bookworm",
    components: ["main", "contrib"],
    architectures: ["amd64", "arm64"],
    sync_interval_s: 43_200,
    auto_snapshot: true,
    created_at: "2026-10-01T10:00:00Z",
    current_snapshot: "20261022T143000Z",
    last_run: {
      id: "r1",
      kind: "sync",
      trigger: "schedule",
      requested_by: null,
      state: "succeeded",
      snapshot_id: "s1",
      error: null,
      started_at: new Date(Date.now() - 3_600_000).toISOString(),
      finished_at: new Date(Date.now() - 3_000_000).toISOString(),
    },
    ...overrides,
  };
}

export function snapshot(overrides: Partial<MirrorSnapshot> = {}): MirrorSnapshot {
  return {
    id: "s1",
    mirror_id: MIRROR_ID,
    name: "20261022T143000Z",
    state: "published",
    size_bytes: 31.2 * 1024 ** 3,
    error: null,
    created_at: "2026-10-22T14:30:00Z",
    published_at: "2026-10-22T14:41:00Z",
    ...overrides,
  };
}

export function run(overrides: Partial<MirrorRun> = {}): MirrorRun {
  return {
    id: "r1",
    mirror_id: MIRROR_ID,
    kind: "sync",
    trigger: "schedule",
    requested_by: null,
    state: "succeeded",
    snapshot_id: "s1",
    output: "aptly mirror update mf:debian-bookworm:main\nDownload queue: 38 items",
    error: null,
    started_at: "2026-10-22T14:30:00Z",
    finished_at: "2026-10-22T14:41:40Z",
    ...overrides,
  };
}

export function mirrorDetail(overrides: Partial<MirrorDetail> = {}): MirrorDetail {
  return { ...mirror(), snapshots: [snapshot()], runs: [run()], ...overrides };
}
