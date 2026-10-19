import type { Incident } from "@/lib/api";

export const SERVER_ID = "6f1c2a52-3b5e-4c43-9a51-0e7f6d1f1a01";
export const SITE_ID = "0b8d2f6e-1c4a-4f7e-9d2b-3a5c6e7f8a90";

/** An open critical disk incident on web-1, with `overrides`. */
export function incident(overrides: Partial<Incident> = {}): Incident {
  return {
    id: "1d6a9a3e-5b7c-4d8e-9f01-23456789abcd",
    server_id: SERVER_ID,
    hostname: "web-1",
    site_id: SITE_ID,
    site_name: "paris",
    metric: "monitorflow_disk_used_ratio",
    instance: "/var",
    severity: "critical",
    state: "open",
    opened_at: "2026-10-19T11:00:00Z",
    resolved_at: null,
    last_value: 0.962,
    last_value_at: "2026-10-19T11:59:00Z",
    peak_value: 0.971,
    threshold: {
      metric: "monitorflow_disk_used_ratio",
      instance: null,
      comparison: ">",
      warning: 0.85,
      critical: 0.95,
      duration_s: 300,
      hysteresis: 0.02,
    },
    ...overrides,
  };
}
