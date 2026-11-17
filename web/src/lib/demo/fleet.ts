// The made-up fleet of the demo: one company, two sites, one relay and
// about thirty servers. Addresses are in documentation ranges (RFC 5737)
// and names under example.com.

import type {
  Customer,
  Me,
  Relay,
  Server,
  ServerState,
  ServerSummary,
  Site,
  UserAccount,
} from "../api";

import { DAY, HOUR, ago, later } from "./time";

/** A demo identifier that reads like a UUID: `prefix` tells kinds apart. */
export function demoId(prefix: number, n: number): string {
  return `00000000-0000-4000-8${prefix.toString(16).padStart(3, "0")}-${n.toString(16).padStart(12, "0")}`;
}

export const AGENT_VERSION = "0.6.0";
export const SERVER_VERSION = "0.6.0";

export const CUSTOMER: Customer = { id: demoId(1, 1), name: "Demo Company", site_count: 2 };

const site = (n: number, name: string, servers: number, relays: number): Site => ({
  id: demoId(2, n),
  name,
  server_count: servers,
  relay_count: relays,
  customer_id: CUSTOMER.id,
  customer_name: CUSTOMER.name,
  agent_auto_update: null,
  agent_auto_update_effective: true,
  offline_alerts: true,
});

export const DATACENTER = site(1, "Datacenter", 0, 0);
export const BRANCH = site(2, "Branch office", 0, 1);
export const SITES: Site[] = [BRANCH, DATACENTER];

export const RELAY: Relay = {
  id: demoId(3, 1),
  peer_id: demoId(4, 1),
  name: "relay-01",
  site_id: BRANCH.id,
  site_name: BRANCH.name,
  customer_id: CUSTOMER.id,
  customer_name: CUSTOMER.name,
  created_at: ago(200 * DAY),
  revoked_at: null,
  status: "online",
  connected_at: ago(6 * DAY),
  last_heartbeat: ago(0),
  cert_not_after: later(300 * DAY),
  server_names: ["relay-01.branch.example.com", "198.51.100.10"],
  server_count: 0,
  connected_agents: 0,
  relay_version: AGENT_VERSION,
  last_update: null,
  offline_alerts: true,
  site_offline_alerts: true,
  silenced_until: null,
};

/** What the demo knows of a server beyond the API's row. */
export interface DemoServer extends Server {
  /** Display name of the distribution, for packages and sources. */
  distribution: string;
  /** Its address, in a documentation range. */
  address: string;
  /** Linux disks, or Windows drives. */
  disks: string[];
  /** Services its checks watch. */
  services: string[];
  /** What it does, for its logs and load. */
  role: "web" | "proxy" | "db" | "app" | "mail" | "files" | "dc" | "backup" | "build" | "cache" | "misc";
  /** 0 to 1: how busy it is on average. */
  load: number;
}

type Os = "debian12" | "ubuntu2404" | "ubuntu2204" | "rocky9" | "alma9" | "win2022" | "win2019";

const OS: Record<Os, { family: string; version: string; distribution: string }> = {
  debian12: { family: "debian", version: "Debian GNU/Linux 12 (bookworm)", distribution: "Debian 12" },
  ubuntu2404: { family: "debian", version: "Ubuntu 24.04.1 LTS", distribution: "Ubuntu 24.04" },
  ubuntu2204: { family: "debian", version: "Ubuntu 22.04.5 LTS", distribution: "Ubuntu 22.04" },
  rocky9: { family: "rhel", version: "Rocky Linux 9.4 (Blue Onyx)", distribution: "Rocky Linux 9" },
  alma9: { family: "rhel", version: "AlmaLinux 9.4 (Seafoam Ocelot)", distribution: "AlmaLinux 9" },
  win2022: { family: "windows", version: "Windows Server 2022 Standard 21H2", distribution: "Windows Server 2022" },
  win2019: { family: "windows", version: "Windows Server 2019 Standard 1809", distribution: "Windows Server 2019" },
};

interface Spec {
  hostname: string;
  os: Os;
  role: DemoServer["role"];
  services: string[];
  load: number;
  branch?: boolean;
  state?: ServerState;
  disks?: string[];
}

const LINUX_DISKS = ["/", "/var"];
const WINDOWS_DISKS = ["C:", "D:"];

const SPECS: Spec[] = [
  { hostname: "web-01", os: "debian12", role: "web", services: ["nginx.service", "php8.2-fpm.service"], load: 0.35 },
  { hostname: "web-02", os: "debian12", role: "web", services: ["nginx.service", "php8.2-fpm.service"], load: 0.33 },
  { hostname: "web-03", os: "debian12", role: "web", services: ["nginx.service", "php8.2-fpm.service"], load: 0.62, state: "warning" },
  { hostname: "web-04", os: "debian12", role: "web", services: ["nginx.service", "php8.2-fpm.service"], load: 0.3 },
  { hostname: "proxy-01", os: "debian12", role: "proxy", services: ["haproxy.service"], load: 0.25 },
  { hostname: "proxy-02", os: "debian12", role: "proxy", services: ["haproxy.service"], load: 0.22 },
  { hostname: "db-01", os: "debian12", role: "db", services: ["postgresql@16-main.service"], load: 0.45, disks: ["/", "/var/lib/postgresql"] },
  { hostname: "db-02", os: "debian12", role: "db", services: ["postgresql@16-main.service"], load: 0.5, state: "critical", disks: ["/", "/var/lib/postgresql"] },
  { hostname: "app-01", os: "ubuntu2404", role: "app", services: ["app-worker.service", "redis-server.service"], load: 0.4 },
  { hostname: "app-02", os: "ubuntu2404", role: "app", services: ["app-worker.service", "redis-server.service"], load: 0.42 },
  { hostname: "app-03", os: "ubuntu2404", role: "app", services: ["app-worker.service"], load: 0.38 },
  { hostname: "app-04", os: "ubuntu2404", role: "app", services: ["app-worker.service"], load: 0.36, state: "offline" },
  { hostname: "api-01", os: "ubuntu2204", role: "app", services: ["api.service"], load: 0.3 },
  { hostname: "api-02", os: "ubuntu2204", role: "app", services: ["api.service"], load: 0.28 },
  { hostname: "cache-01", os: "rocky9", role: "cache", services: ["redis.service"], load: 0.2 },
  { hostname: "git-01", os: "rocky9", role: "misc", services: ["sshd.service", "nginx.service"], load: 0.15 },
  { hostname: "build-01", os: "ubuntu2404", role: "build", services: ["build-runner.service"], load: 0.55 },
  { hostname: "mail-01", os: "alma9", role: "mail", services: ["postfix.service", "dovecot.service"], load: 0.3, state: "acknowledged" },
  { hostname: "vpn-01", os: "alma9", role: "misc", services: ["wg-quick@wg0.service"], load: 0.12 },
  { hostname: "log-01", os: "debian12", role: "misc", services: ["rsyslog.service"], load: 0.35 },
  { hostname: "backup-01", os: "debian12", role: "backup", services: ["backup-agent.service"], load: 0.2, state: "silenced", disks: ["/", "/srv/backup"] },
  { hostname: "dc-01", os: "win2022", role: "dc", services: ["NTDS", "DNS", "Netlogon"], load: 0.18 },
  { hostname: "dc-02", os: "win2022", role: "dc", services: ["NTDS", "DNS", "Netlogon"], load: 0.16 },
  { hostname: "files-01", os: "win2022", role: "files", services: ["LanmanServer", "DFSR"], load: 0.28, state: "critical" },
  { hostname: "sql-01", os: "win2019", role: "db", services: ["MSSQLSERVER", "SQLSERVERAGENT"], load: 0.48 },
  { hostname: "print-01", os: "win2019", role: "misc", services: ["Spooler"], load: 0.1 },
  { hostname: "br-dc-01", os: "win2022", role: "dc", services: ["NTDS", "DNS", "Netlogon"], load: 0.14, branch: true },
  { hostname: "br-files-01", os: "win2019", role: "files", services: ["LanmanServer"], load: 0.22, branch: true },
  { hostname: "br-web-01", os: "debian12", role: "web", services: ["nginx.service"], load: 0.18, branch: true },
  { hostname: "br-backup-01", os: "rocky9", role: "backup", services: ["backup-agent.service"], load: 0.12, branch: true },
];

const COMMAND_KINDS_LINUX = { apply_updates: "accepted", configure_sources: "accepted", shell: "accepted", self_update: "accepted", snapshot: "accepted" };
const COMMAND_KINDS_WINDOWS = { apply_updates: "accepted", shell: "accepted", self_update: "accepted" };

export const SERVERS: DemoServer[] = SPECS.map((spec, i) => {
  const os = OS[spec.os];
  const windows = os.family === "windows";
  const where = spec.branch ? BRANCH : DATACENTER;
  const state = spec.state ?? "ok";
  const offline = state === "offline";
  return {
    id: demoId(5, i + 1),
    peer_id: demoId(6, i + 1),
    hostname: spec.hostname,
    site_id: where.id,
    site_name: where.name,
    customer_id: CUSTOMER.id,
    customer_name: CUSTOMER.name,
    relay_id: spec.branch ? RELAY.id : null,
    os_family: os.family,
    os_version: os.version,
    package_updates: true,
    command_kinds: windows ? COMMAND_KINDS_WINDOWS : COMMAND_KINDS_LINUX,
    agent_version: AGENT_VERSION,
    created_at: ago((180 - i * 3) * DAY),
    revoked_at: null,
    status: offline ? "offline" : "online",
    connected_at: offline ? null : ago(6 * DAY + i * 17),
    last_heartbeat: offline ? ago(47) : ago(0),
    via_relay: spec.branch && !offline ? RELAY.peer_id : null,
    open_incidents: state === "ok" || state === "silenced" ? 0 : 1,
    state,
    cert_not_after: later((200 + i * 5) * DAY),
    offline_alerts: true,
    site_offline_alerts: true,
    silenced_until: state === "silenced" ? later(3 * HOUR) : null,
    silence: state === "silenced" ? { by: "alex", until: later(3 * HOUR), reason: "Disk replacement in the backup array" } : null,
    acknowledgement:
      state === "acknowledged" ? { by: "sam", at: ago(25), note: "Looking into the mail queue, a large newsletter went out." } : null,
    distribution: os.distribution,
    address: spec.branch ? `198.51.100.${20 + i}` : `192.0.2.${10 + i}`,
    disks: spec.disks ?? (windows ? WINDOWS_DISKS : LINUX_DISKS),
    services: spec.services,
    role: spec.role,
    load: spec.load,
  };
});

DATACENTER.server_count = SERVERS.filter((s) => s.site_id === DATACENTER.id).length;
BRANCH.server_count = SERVERS.filter((s) => s.site_id === BRANCH.id).length;
RELAY.server_count = BRANCH.server_count;
RELAY.connected_agents = SERVERS.filter((s) => s.via_relay).length;

export function serverByName(hostname: string): DemoServer {
  const server = SERVERS.find((s) => s.hostname === hostname);
  if (!server) throw new Error(`no demo server ${hostname}`);
  return server;
}

export const isWindows = (server: Server) => server.os_family === "windows";

/** The API's row: without the demo's own fields. */
export function serverRow(server: DemoServer): Server {
  // eslint-disable-next-line @typescript-eslint/no-unused-vars
  const { distribution, address, disks, services, role, load, ...row } = server;
  return row;
}

export function summary(servers: Server[]): ServerSummary {
  const counts: ServerSummary = { critical: 0, warning: 0, acknowledged: 0, silenced: 0, offline: 0, ok: 0, total: servers.length };
  for (const server of servers) counts[server.state ?? "ok"] += 1;
  return counts;
}

/** The signed-in demo user: an administrator of everything, whose every write is refused. */
export const DEMO_USER: Me = {
  id: demoId(7, 1),
  username: "demo",
  roles: [{ role: "administrator", customer_id: null, site_id: null }],
  multi_customer: false,
};

const user = (n: number, username: string, roles: UserAccount["roles"], days: number): UserAccount => ({
  id: demoId(7, n),
  username,
  state: "active",
  created_at: ago(days * DAY),
  roles,
  link_expires_at: null,
});

export const USERS: UserAccount[] = [
  user(2, "alex", [{ role: "administrator", customer_id: null, site_id: null }], 210),
  user(5, "camille", [{ role: "viewer", customer_id: null, site_id: BRANCH.id }], 40),
  { ...user(1, "demo", DEMO_USER.roles, 220) },
  user(4, "robin", [{ role: "approver", customer_id: CUSTOMER.id, site_id: null }], 90),
  user(3, "sam", [{ role: "operator", customer_id: CUSTOMER.id, site_id: null }], 150),
  { ...user(6, "taylor", [{ role: "operator", customer_id: null, site_id: DATACENTER.id }], 2), state: "invited", link_expires_at: later(5 * DAY) },
];
