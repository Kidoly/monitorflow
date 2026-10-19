import type { ReactNode } from "react";

import type { Me, Relay, RoleGrant } from "@/lib/api";
import { MeContext } from "@/lib/me";

export const me = (...roles: RoleGrant[]): Me => ({ id: "u1", username: "alice", roles });
export const customerAdmin = me({ role: "administrator", customer_id: "c1", site_id: null });
export const siteAdmin = me({ role: "administrator", customer_id: null, site_id: "site-1" });
export const viewer = me({ role: "viewer", customer_id: null, site_id: null });

/** Renders `children` as signed in as `user`. */
export function As({ user, children }: { user: Me; children: ReactNode }) {
  return <MeContext.Provider value={user}>{children}</MeContext.Provider>;
}

export function relay(overrides: Partial<Relay> = {}): Relay {
  return {
    id: "6f1c2a52-3b5e-4c43-9a51-0e7f6d1f1a02",
    peer_id: "relay-1",
    name: "relay-1",
    site_id: "site-1",
    site_name: "paris",
    customer_id: "c1",
    customer_name: "acme",
    created_at: "2026-10-18T10:00:00Z",
    revoked_at: null,
    status: "online",
    connected_at: new Date().toISOString(),
    last_heartbeat: new Date().toISOString(),
    cert_not_after: new Date(Date.now() + 5 * 86_400_000).toISOString(),
    server_names: ["relay1.paris.example"],
    server_count: 2,
    connected_agents: 1,
    ...overrides,
  };
}
