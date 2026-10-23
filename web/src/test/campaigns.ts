import type { Campaign, CampaignDetail, CampaignWave, Server } from "@/lib/api";

export const CAMPAIGN_ID = "8a1c2a52-3b5e-4c43-9a51-0e7f6d1f1a10";
export const WEB1 = "6f1c2a52-3b5e-4c43-9a51-0e7f6d1f1a01";
export const WEB2 = "6f1c2a52-3b5e-4c43-9a51-0e7f6d1f1a02";

const progress = { total: 2, pending: 1, in_progress: 1, succeeded: 0, failed: 0, cancelled: 0 };

/** A running campaign of two waves of acme, with `overrides`. */
export function campaign(overrides: Partial<Campaign> = {}): Campaign {
  return {
    id: CAMPAIGN_ID,
    customer_id: "c1",
    customer_name: "acme",
    name: "October updates",
    site_id: null,
    state: "running",
    error: null,
    spec: { packages: [], security_only: true, allow_reboot: false },
    command_ttl_s: 3600,
    failure_threshold: { value: 0, unit: "count" },
    windows: [{ kind: "weekly", weekdays: [6, 7], start: "22:00", end: "04:00", timezone: "Europe/Paris" }],
    sources: null,
    wave_count: 2,
    current_wave: 0,
    progress,
    created_by: "u1",
    created_at: "2026-10-22T10:00:00Z",
    started_at: "2026-10-22T10:05:00Z",
    finished_at: null,
    ...overrides,
  };
}

export function detail(overrides: Partial<CampaignDetail> = {}): CampaignDetail {
  const waves: CampaignWave[] = [
    {
      index: 0,
      state: "running",
      started_at: "2026-10-22T10:05:00Z",
      finished_at: null,
      progress: { ...progress, total: 1, pending: 0 },
      servers: [
        {
          server_id: WEB1,
          peer_id: "web-1",
          hostname: "web-1",
          site_id: "site-1",
          status: "running",
          command_id: "9a1c2a52-3b5e-4c43-9a51-0e7f6d1f1a11",
          exit_code: null,
          error: null,
          finished_at: null,
          sources_command_id: null,
          restore_command_id: null,
        },
      ],
    },
    {
      index: 1,
      state: "pending",
      started_at: null,
      finished_at: null,
      progress: { ...progress, total: 1, in_progress: 0 },
      servers: [
        {
          server_id: WEB2,
          peer_id: "web-2",
          hostname: "web-2",
          site_id: "site-1",
          status: "pending",
          command_id: null,
          exit_code: null,
          error: null,
          finished_at: null,
          sources_command_id: null,
          restore_command_id: null,
        },
      ],
    },
  ];
  return { ...campaign(), waves, ...overrides };
}

/** A server of acme's paris site, with `overrides`. */
export function server(overrides: Partial<Server> = {}): Server {
  return {
    id: WEB1,
    peer_id: "web-1",
    hostname: "web-1",
    site_id: "site-1",
    site_name: "paris",
    customer_id: "c1",
    customer_name: "acme",
    relay_id: null,
    os_family: "debian",
    os_version: "12",
    agent_version: "0.1.0",
    created_at: "2026-10-18T10:00:00Z",
    revoked_at: null,
    status: "online",
    connected_at: null,
    last_heartbeat: null,
    via_relay: null,
    open_incidents: 0,
    cert_not_after: null,
    ...overrides,
  };
}
