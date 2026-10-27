import type { Customer, Incident, Site } from "@/lib/api";

export const ACME = "0b8f3c4e-1a2b-4c3d-8e9f-000000000001";
export const NORTHWIND = "0b8f3c4e-1a2b-4c3d-8e9f-000000000002";
export const PARIS = "5a1e0c2d-0000-4000-8000-000000000001";
export const LYON = "5a1e0c2d-0000-4000-8000-000000000002";
export const BERLIN = "5a1e0c2d-0000-4000-8000-000000000003";

export const customers: Customer[] = [
  { id: ACME, name: "Acme Hosting", site_count: 2 },
  { id: NORTHWIND, name: "Northwind Logistics", site_count: 1 },
];

const site = (id: string, name: string, customer: Customer): Site => ({
  id,
  name,
  server_count: 1,
  relay_count: 0,
  customer_id: customer.id,
  customer_name: customer.name,
  agent_auto_update: false,
});

export const sites: Site[] = [
  site(PARIS, "paris-dc1", customers[0]!),
  site(LYON, "lyon-office", customers[0]!),
  site(BERLIN, "berlin-hq", customers[1]!),
];

/** An open incident on `siteId`; only the fields the shell reads matter. */
export const openIncident = (id: string, siteId: string) => ({ id, site_id: siteId, state: "open" }) as Incident;

/** What the shell fetches: scope lists and the open incidents count. */
export const SHELL_ROUTES = {
  "GET /api/customers": { status: 200, body: { customers } },
  "GET /api/sites": { status: 200, body: { sites } },
  "GET /api/incidents?state=open&limit=500": {
    status: 200,
    body: { incidents: [openIncident("i1", PARIS), openIncident("i2", PARIS), openIncident("i3", BERLIN)], next_cursor: null },
  },
};
