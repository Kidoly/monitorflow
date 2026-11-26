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
    tickets: {
      ticket_type_id: null,
      ticket_type_name: null,
      category: null,
      min_severity: "critical",
      resolved_status_id: 9,
      resolved_status_name: "Closed",
      warning_priority_id: null,
      warning_priority_name: null,
      critical_priority_id: null,
      critical_priority_name: null,
      closed_status_ids: [9],
      acknowledge_on_assign: true,
    },
    default_client: {
      default_client_id: null,
      default_client_name: null,
      default_site_id: null,
      default_site_name: null,
    },
    assets: { asset_sync: false, asset_inactive_on_removal: false },
    ...overrides,
  };
}

/** HaloPSA's lists, as the ticket step reads them. */
export const psaListStubs = {
  "GET /api/psa/ticket-types": { status: 200, body: { records: [{ id: 3, name: "Change Request" }, { id: 1, name: "Incident" }] } },
  "GET /api/psa/categories": { status: 200, body: { categories: ["Hardware>Server", "Network"] } },
  "GET /api/psa/statuses": { status: 200, body: { records: [{ id: 1, name: "New" }, { id: 9, name: "Closed" }] } },
  "GET /api/psa/priorities": { status: 200, body: { records: [{ id: 1, name: "Critical" }, { id: 3, name: "Medium" }] } },
  "GET /api/psa/priorities?ticket_type_id=1": {
    status: 200,
    body: { records: [{ id: 1, name: "Critical" }, { id: 3, name: "Medium" }] },
  },
  "GET /api/psa/priorities?ticket_type_id=3": { status: 200, body: { records: [{ id: 1, name: "P1" }] } },
};
