import type { PsaConnection } from "@/lib/api";

/** A saved HaloPSA connection that worked. */
export function connection(overrides: Partial<PsaConnection> = {}): PsaConnection {
  return {
    kind: "halopsa",
    base_url: "https://halo.example.com",
    tenant: null,
    client_id: "id-1",
    secret_error: null,
    enabled: true,
    agent_name: "MonitorFlow",
    last_success_at: "2026-11-22T09:00:00Z",
    last_error: null,
    last_error_at: null,
    created_at: "2026-11-22T08:00:00Z",
    updated_at: "2026-11-22T08:00:00Z",
    ...overrides,
  };
}
