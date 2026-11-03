import type { CaPeerState, CaRotation, CertificateAuthorityView } from "@/lib/api";

export function peer(overrides: Partial<CaPeerState> = {}): CaPeerState {
  return {
    role: "agent",
    peer_id: "web-01",
    name: "web-01.example",
    last_seen_at: "2026-11-03T11:55:00Z",
    not_after: "2026-11-09T12:00:00Z",
    moved: false,
    trusts_all: false,
    too_old: false,
    cut_off: false,
    ...overrides,
  };
}

export function rotation(overrides: Partial<CaRotation> = {}): CaRotation {
  return {
    stage: "introduced",
    emergency: false,
    started_at: "2026-11-03T10:00:00Z",
    switched_at: null,
    distrust_at: null,
    other_fingerprint: "b".repeat(64),
    total: 315,
    done: 312,
    waiting: [
      peer(),
      peer({ peer_id: "relay-1", role: "relay", name: "relay-1", last_seen_at: "2026-11-02T08:00:00Z", too_old: true }),
      peer({ peer_id: "db-02", name: "db-02.example", last_seen_at: null }),
    ],
    ...overrides,
  };
}

export function authority(overrides: Partial<CertificateAuthorityView> = {}): CertificateAuthorityView {
  return {
    fingerprint: "a".repeat(64),
    created_at: "2026-10-01T12:00:00Z",
    expires_at: "2036-09-29T12:00:00Z",
    expires_soon: false,
    auto_change_at: "2036-04-02T12:00:00Z",
    restored_retired_at: null,
    cut_off: [],
    rotation: null,
    ...overrides,
  };
}
