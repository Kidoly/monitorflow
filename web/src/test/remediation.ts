import type { PendingApproval, RemediationEvent } from "@/lib/api";

import { SERVER_ID, SITE_ID } from "./incidents";

export const CUSTOMER_ID = "0b8f3c4e-1a2b-4c3d-8e9f-000000000001";
export const INCIDENT_ID = "3c1d2e4f-5a6b-4c7d-8e9f-0a1b2c3d4e5f";
export const PENDING_ID = "9a8b7c6d-5e4f-4a3b-8c2d-1e0f9a8b7c6d";

/** A low-impact command waiting on web-1, with `overrides`. */
export function pending(overrides: Partial<PendingApproval> = {}): PendingApproval {
  return {
    id: PENDING_ID,
    incident_id: INCIDENT_ID,
    customer_id: CUSTOMER_ID,
    customer_name: "Acme Hosting",
    site_id: SITE_ID,
    site_name: "paris",
    server_id: SERVER_ID,
    hostname: "web-1",
    alert_incident_id: null,
    metric: "monitorflow_disk_used_ratio",
    instance: "/var",
    command: "journalctl --vacuum-size=500M",
    diagnosis: "journald holds 29 GiB of /var",
    reasoning: "Vacuuming archived journals frees about 28 GiB.",
    tier: "low_impact",
    rule: { source: "builtin", id: "low.journal-vacuum" },
    reason: "vacuums the systemd journal",
    commands_run: 2,
    max_commands: 10,
    opened_at: "2026-10-19T11:30:00Z",
    proposed_at: "2026-10-19T11:40:00Z",
    deadline: "2026-10-19T12:00:00Z",
    can_approve: true,
    can_reject: true,
    snapshots: { available: false, reason: "not_mapped" },
    ...overrides,
  };
}

/** An event of the incident on web-1, with `overrides`. */
export function event(overrides: Partial<RemediationEvent> = {}): RemediationEvent {
  return {
    incident_id: INCIDENT_ID,
    seq: 1,
    kind: "note",
    actor_kind: "system",
    actor_id: "server",
    actor_name: null,
    payload: { text: "a note" },
    created_at: "2026-10-19T11:45:00Z",
    hash: "ab".repeat(32),
    customer_id: CUSTOMER_ID,
    customer_name: "Acme Hosting",
    site_id: SITE_ID,
    site_name: "paris",
    server_id: SERVER_ID,
    hostname: "web-1",
    metric: "monitorflow_disk_used_ratio",
    instance: "/var",
    ...overrides,
  };
}
