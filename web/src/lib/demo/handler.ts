// The static demo's API (docs/demo.md): `request` in api.ts hands every call
// here when the build sets NEXT_PUBLIC_MONITORFLOW_DEMO=1. Reads answer from
// the made-up data; writes are refused with DEMO_REFUSAL, which pages show
// like any other API error. A read the demo does not know answers 404, which
// pages show as an error rather than crashing on a wrong shape.

import {
  ApiError,
  DEMO_REFUSAL,
  type AgentInstall,
  type AutoUpdateStatus,
  type AutoUpdateVersionPeerList,
  type BackupsView,
  type CertificateAuthorityView,
  type DashboardData,
  type Dependency,
  type DependencyEnd,
  type DependencyPage,
  type EffectiveThreshold,
  type IncidentList,
  type Panel,
  type PanelData,
  type Preferences,
  type PublicAddress,
  type RequestOptions,
  type Server,
  type ServerDetail,
  type ServerState,
  type ServerSettings,
  type ServerUpdateView,
  type Site,
  type SourcesStatus,
  type Within,
} from "../api";

import {
  API_KEYS,
  APPROVAL,
  AUDIT,
  CAMPAIGNS,
  CHANNELS,
  DASHBOARD,
  INCIDENTS,
  INTEGRATIONS,
  MAINTENANCE_WINDOWS,
  MIRRORS,
  REMEDIATIONS,
  SILENCES,
  campaignRow,
  commandsOf,
  fakeHash,
  followUp,
  incidentRow,
  logsOf,
  mirrorDetail,
  onCallSchedule,
  inventoryOf,
  packagesOf,
} from "./data";
import {
  AGENT_VERSION,
  CUSTOMER,
  DEMO_USER,
  type DemoServer,
  RELAY,
  SERVERS,
  SERVER_VERSION,
  SITES,
  USERS,
  isWindows,
  serverRow,
  summary,
} from "./fleet";
import { baseKind } from "../dashboards";
import { ALL_METRICS, instancesOf, metricsOf, rangeSeries, sensorValues, sensorsOf, valueAt } from "./series";
import { DAY, HOUR, ago, later } from "./time";

/** Writes that only read, answered like a GET. */
const READ_POSTS = new Set(["/api/dashboard-data", "/api/display/data", "/api/settings/ai/models"]);

/** The person's preferences, kept for the tab's life so the theme and language switch. */
let preferences: Preferences = {};

class NotFound extends Error {}

const found = <T>(value: T | undefined): T => {
  if (value === undefined) throw new NotFound();
  return value;
};

const server = (id: string): DemoServer => found(SERVERS.find((s) => s.id === id));

/** The API row with a fresh heartbeat while online, so "last seen" stays a few seconds ago. */
const live = (s: DemoServer) => {
  const row = serverRow(s);
  return s.status === "online" ? { ...row, last_heartbeat: new Date().toISOString() } : row;
};
const site = (id: string): Site => found(SITES.find((s) => s.id === id));

/** Answers `path` as the API would, or throws an `ApiError`. */
export async function demoRequest<T>(path: string, options: RequestOptions = {}): Promise<T> {
  const method = options.method ?? "GET";
  // A short wait, so pages show their loading state as they would.
  await new Promise((resolve) => setTimeout(resolve, 60));
  if (options.signal?.aborted) throw new DOMException("The request was aborted.", "AbortError");

  const url = new URL(path, "http://demo.invalid");
  if (method === "PUT" && url.pathname === "/api/me/preferences") {
    preferences = (options.body as Preferences | undefined) ?? {};
    return structuredClone(preferences) as T;
  }
  if (method !== "GET" && !READ_POSTS.has(url.pathname)) {
    throw new ApiError(405, DEMO_REFUSAL);
  }
  try {
    const answer = route(url, options.body);
    return (answer === undefined ? undefined : structuredClone(answer)) as T;
  } catch (e) {
    if (e instanceof NotFound) throw new ApiError(404, "not found");
    throw e;
  }
}

type Params = URLSearchParams;
type Handler = (match: string[], query: Params, body: unknown) => unknown;

const ROUTES: [RegExp, Handler][] = [
  [/^\/api\/health$/, () => ({ status: "ok" })],
  [/^\/api\/setup$/, () => ({ pending: false })],
  [/^\/api\/auth\/me$/, () => DEMO_USER],
  [/^\/api\/auth\/providers$/, () => ({ providers: [] })],
  [/^\/api\/identity-providers$/, () => ({
    providers: [],
    redirect_uri: "https://monitor.example.com/api/auth/oidc/callback",
    local_administrators: 1,
  })],
  [/^\/api\/me\/preferences$/, () => preferences],
  // The phone pages (ADR-0186): no phone yet; adding one is a write the demo refuses.
  [/^\/api\/me\/phones$/, () => ({ phones: [], push_key: "BA", session_days: 90, idle_days: 30 })],

  // Fleet
  [/^\/api\/servers$/, (_, q) => {
    const siteId = q.get("site_id");
    const servers = SERVERS.filter((s) => !siteId || s.site_id === siteId).map(live);
    return { servers, next_cursor: null };
  }],
  [/^\/api\/servers\/summary$/, (_, q) => {
    const siteId = q.get("site_id");
    return summary(SERVERS.filter((s) => !siteId || s.site_id === siteId));
  }],
  [/^\/api\/servers\/([^/]+)$/, ([id]) => serverDetail(server(id!))],
  [/^\/api\/servers\/([^/]+)\/sensor-values$/, ([id]) => {
    const now = Date.now() / 1000;
    const end = Math.floor(now / 60) * 60;
    return { start: end - 3600, end, step: 60, sensors: sensorValues(server(id!), now) };
  }],
  [/^\/api\/servers\/([^/]+)\/sensors$/, ([id]) => {
    const s = server(id!);
    return {
      server_id: s.id, site_id: s.site_id, revision: 12, applied_revision: 12, applied_at: ago(6 * DAY), in_sync: true,
      sensors: sensorsOf(s), check_interval_ms: 60_000, check_interval_origin: "default",
    };
  }],
  [/^\/api\/servers\/([^/]+)\/packages$/, ([id]) => {
    const s = server(id!);
    return { reboot_required: false, reported_at: ago(2 * HOUR), packages: packagesOf(s), next_cursor: null };
  }],
  [/^\/api\/servers\/([^/]+)\/inventory$/, ([id]) => inventoryOf(server(id!))],
  [/^\/api\/servers\/([^/]+)\/inventory\/changes$/, () => ({ changes: [], next_cursor: null })],
  [/^\/api\/inventory$/, () => ({ hits: [], truncated: false })],
  [/^\/api\/servers\/([^/]+)\/third-party$/, () => ({ reported: false, managers: [], packages: [], next_cursor: null })],
  [/^\/api\/servers\/([^/]+)\/proxmox$/, ([id]) => ({
    server_id: server(id!).id, mapping: null, snapshots: { available: false, reason: "disabled" }, clusters: [], candidates: [],
  })],
  [/^\/api\/servers\/([^/]+)\/proxmox\/guests$/, () => ({ guests: [] })],
  [/^\/api\/servers\/([^/]+)\/thresholds\/effective$/, ([id]) => {
    const s = server(id!);
    return { server_id: s.id, site_id: s.site_id, revision: 4, thresholds: THRESHOLDS };
  }],
  [/^\/api\/sites\/([^/]+)\/thresholds\/effective$/, ([id]) => ({ site_id: site(id!).id, thresholds: THRESHOLDS })],
  [/^\/api\/sites\/([^/]+)\/sensors$/, ([id]) => ({
    site_id: site(id!).id, sensors: sensorsOf(SERVERS.find((s) => s.site_id === id)!).slice(0, 4),
    check_interval_ms: 60_000, check_interval_origin: "default",
  })],
  [/^\/api\/(?:sites|servers)\/([^/]+)\/(?:thresholds|sensor_configs)$/, () => ({ entries: [] })],
  [/^\/api\/servers\/([^/]+)\/recommendations$/, ([id]) => ({
    reported_at: ago(HOUR),
    recommendations: server(id!).role === "db" && !isWindows(server(id!))
      ? [{
          key: "port:5432", title: "PostgreSQL listens on port 5432", evidence: ["postgres listens on 0.0.0.0:5432"],
          checks: [{ kind: "port", params: { port: "5432" } }], dismissed_by: null, dismissed_at: null,
        }]
      : [],
  })],
  [/^\/api\/servers\/([^/]+)\/metrics$/, ([id]) => ({ metrics: metricsOf(server(id!)), truncated: false })],
  [/^\/api\/servers\/([^/]+)\/rollback$/, ([id]) => ({
    server_id: server(id!).id,
    snapshots: isWindows(server(id!))
      ? { available: false, reason: "not_mapped", detail: "Windows servers take no local snapshot" }
      : { available: true, reason: null, method: "lvm", detail: "an LVM snapshot of every system volume, kept 3 days" },
    not_covered: [], local_snapshots: [], reported_at: ago(HOUR),
  })],
  [/^\/api\/servers\/([^/]+)\/sources$/, ([id]) => sourcesOf(server(id!))],
  [/^\/api\/servers\/([^/]+)\/commands$/, ([id]) => ({ commands: commandsOf(server(id!)), next_cursor: null })],
  [/^\/api\/servers\/([^/]+)\/logs$/, ([id], q) => {
    const level = q.get("level");
    const text = q.get("q")?.toLowerCase();
    const unit = q.get("unit");
    const all = logsOf(server(id!));
    const lines = all.filter(
      (l) => (level !== "error" || (l.priority ?? 7) <= 3) && (!unit || l.unit === unit) && (!text || l.message.toLowerCase().includes(text)),
    );
    return { lines, more: false, units: [...new Set(all.map((l) => l.unit!))].sort() };
  }],
  [/^\/api\/sites$/, () => ({ sites: SITES })],
  [/^\/api\/sites\/([^/]+)\/sources$/, ([id]) => ({
    servers: SERVERS.filter((s) => s.site_id === site(id!).id).map((s) => ({ ...sourcesOf(s), id: s.id, hostname: s.hostname, os_family: s.os_family })),
  })],
  [/^\/api\/customers$/, () => ({ customers: [CUSTOMER] })],
  [/^\/api\/relays$/, () => ({ relays: [{ ...RELAY, last_heartbeat: new Date().toISOString() }], next_cursor: null })],
  [/^\/api\/relays\/([^/]+)$/, ([id]) => {
    if (id !== RELAY.id) throw new NotFound();
    return {
      ...RELAY,
      last_heartbeat: new Date().toISOString(),
      certificate: certificate(RELAY.id),
      servers: SERVERS.filter((s) => s.relay_id === RELAY.id).map((s) => ({ id: s.id, peer_id: s.peer_id, hostname: s.hostname, status: s.status })),
      updates: [],
    };
  }],
  // Dependencies (ADR-0152): the demo's are its relay's, automatic.
  [/^\/api\/dependencies$/, () => ({ dependencies: [] })],
  [/^\/api\/servers\/([^/]+)\/dependencies$/, ([id]) => {
    const me = server(id!);
    return { ...noDependencies(), parents: me.relay_id === RELAY.id ? [behindRelay(me)] : [] };
  }],
  [/^\/api\/relays\/([^/]+)\/dependencies$/, ([id]) => {
    if (id !== RELAY.id) throw new NotFound();
    const behind = SERVERS.filter((s) => s.relay_id === RELAY.id);
    return { ...noDependencies(), children: behind.map(behindRelay), behind: behind.map(endOf) };
  }],
  [/^\/api\/maintenance-windows$/, (_, q) => {
    const site = q.get("site_id") ?? SERVERS.find((s) => s.id === q.get("server_id"))?.site_id;
    return { windows: MAINTENANCE_WINDOWS.filter((w) => !site || w.site_id === site) };
  }],
  // Availability (ADR-0155): every demo server up all along.
  [/^\/api\/availability$/, (_, q) => {
    const from = q.get("from") ?? new Date().toISOString();
    const to = q.get("to") ?? new Date().toISOString();
    const counted = Math.max(0, (Date.parse(to) - Date.parse(from)) / 1000);
    const numbers = { availability: 100, counted_seconds: counted, down_seconds: 0, maintenance_seconds: 0, outages: 0, longest_seconds: 0 };
    const machines = SERVERS.map((s) => ({
      id: s.id, name: s.hostname, device: false, site_id: s.site_id, site_name: s.site_name,
      customer_id: s.customer_id, numbers, outage_list: [],
    }));
    return {
      customer_id: null, customer_name: null, from, to, generated_at: new Date().toISOString(),
      summary: { services: 0, services_met: 0, services_missed: 0, machines: machines.length, outages: 0, down_seconds: 0, maintenance_seconds: 0 },
      services: [], machines, checks: [], maintenance: [],
    };
  }],
  // Uptime (ADR-0180): the same servers, up most days, with a few outages.
  [/^\/api\/availability\/uptime$/, (_, q) => {
    const today = new Date();
    return {
      at: today.toISOString(), time_zone: q.get("time_zone") ?? "UTC",
      rows: SERVERS.map((s) => ({ kind: "server", id: s.id, name: s.hostname, check: null, site_name: s.site_name, ...uptimeOf(s.hostname, today) })),
    };
  }],
  [/^\/api\/business-services$/, () => ({ services: [] })],
  [/^\/api\/silences$/, (_, q) => {
    const target = q.get("server_id") ?? q.get("relay_id") ?? q.get("site_id");
    return { silences: SILENCES.filter((s) => !target || s.target_id === target || s.site_id === target) };
  }],

  // Incidents
  [/^\/api\/incidents$/, (_, q) => incidents(q)],
  [/^\/api\/incidents\/([^/]+)$/, ([id]) => found(INCIDENTS.find((i) => i.id === id))],
  [/^\/api\/follow-up\/policies$/, () => followUp()],

  // AI remediation and its audit log
  [/^\/api\/remediation\/approvals$/, () => ({ approvals: [APPROVAL] })],
  [/^\/api\/remediation\/scripts$/, () => ({ scripts: [] })],
  [/^\/api\/remediation\/script-runs$/, () => ({ runs: [] })],
  [/^\/api\/remediation\/automation-rules$/, () => ({ rules: [] })],
  [/^\/api\/remediation\/incidents$/, (_, q) => {
    const alert = q.get("alert_incident_id");
    const list = REMEDIATIONS.filter((r) => !alert || r.alert_incident_id === alert);
    // eslint-disable-next-line @typescript-eslint/no-unused-vars
    return { incidents: list.map(({ events, chain, site_name, customer_name, ...row }) => row), next_cursor: null };
  }],
  [/^\/api\/remediation\/incidents\/([^/]+)$/, ([id]) => found(REMEDIATIONS.find((r) => r.id === id))],
  [/^\/api\/remediation\/alerts\/([^/]+)$/, ([id]) => {
    const alert = found(INCIDENTS.find((i) => i.id === id));
    const list = REMEDIATIONS.filter((r) => r.alert_incident_id === id);
    const latest = list[0] ?? null;
    const busy = latest !== null && !["resolved", "escalated", "failed"].includes(latest.state);
    const blocked = alert.state !== "open" ? "alert_resolved" : busy ? "in_progress" : null;
    // Offered, so the page shows "Run AI again"; starting it is refused like every change in the demo.
    return {
      alert_incident_id: id, attempts: list.length, latest_id: latest?.id ?? null, latest_state: latest?.state ?? null,
      can_run: blocked === null, blocked, reason: null, refusal: null,
    };
  }],
  [/^\/api\/remediation\/events$/, (_, q) => {
    const events = AUDIT.filter(
      (e) =>
        (!q.get("incident_id") || e.incident_id === q.get("incident_id")) &&
        (!q.get("server_id") || e.server_id === q.get("server_id")) &&
        (!q.get("site_id") || e.site_id === q.get("site_id")) &&
        (!q.get("kind") || e.kind === q.get("kind")) &&
        (!q.get("actor_kind") || e.actor_kind === q.get("actor_kind")),
    );
    return { events, next_cursor: null };
  }],
  [/^\/api\/commands\/([^/]+)$/, ([id]) => {
    const command = SERVERS.flatMap(commandsOf).find((c) => c.id === id);
    return { ...found(command), stdout: command?.state === "succeeded" ? "Done.\n" : null, stderr: null };
  }],

  // Updates
  [/^\/api\/campaigns$/, () => ({ campaigns: CAMPAIGNS.map(campaignRow), next_cursor: null })],
  [/^\/api\/campaigns\/([^/]+)$/, ([id]) => found(CAMPAIGNS.find((c) => c.id === id))],
  [/^\/api\/mirrors$/, () => ({ mirrors: MIRRORS, keep_snapshots: 5 })],
  [/^\/api\/mirrors\/catalog$/, () => ({
    tools: { available: true, problem: null },
    disk: { free_bytes: 410e9, total_bytes: 500e9 },
    distributions: MIRRORS.map((m) => ({
      id: m.name, name: m.name, family: m.family, size_bytes: 90e9, on_demand_size_bytes: 6e9, index_bytes: 4e8,
      added: true, problem: null, on_demand_problem: null,
    })),
  })],
  [/^\/api\/mirrors\/cache$/, () => ({
    cap_bytes: 50e9, default_cap_bytes: 50e9, cap_set: false, used_bytes: 7.2e9, index_bytes: 1.9e9, package_bytes: 5.3e9, packages: 4812, over_cap: false,
  })],
  [/^\/api\/mirrors\/([^/]+)$/, ([id]) => mirrorDetail(found(MIRRORS.find((m) => m.id === id)))],
  [/^\/api\/auto-update$/, () => autoUpdate()],
  [/^\/api\/auto-update\/(agent|relay)\/versions\/peers$/, ([kind]) => versionPeers(kind!)],
  // No server of the demo runs a release with a known next one (ADR-0182).
  [/^\/api\/major-upgrades$/, () => ({ servers: [] })],
  [/^\/api\/agent-releases$/, () => ({ versions: [AGENT_VERSION, "0.5.2", "0.5.1"], latest: AGENT_VERSION })],

  // Alerting
  [/^\/api\/notifications\/channels$/, () => ({ channels: CHANNELS })],
  [/^\/api\/on-call\/schedules$/, () => ({ schedules: [onCallSchedule()] })],
  [/^\/api\/on-call\/people$/, () => ({
    people: USERS.filter((u) => u.state === "active").map((u) => ({ id: u.id, username: u.username, has_contact: true })),
  })],
  [/^\/api\/on-call\/contact$/, () => ({
    email: "demo@example.com", kind: null, settings: null, has_secret: false, last_sent_at: null, last_error: null, last_error_at: null,
  })],
  [/^\/api\/integrations$/, () => INTEGRATIONS],
  [/^\/api\/integrations\/([^/]+)\/deliveries$/, () => ({
    deliveries: INCIDENTS.slice(0, 4).map((incident, i) => ({
      id: 100 - i, event_id: fakeHash(incident.id).slice(0, 32), event: "incident.opened", occurred_at: incident.opened_at,
      state: "delivered", replay: false, attempts: 1, last_status: 200, last_error: null,
      last_attempt_at: incident.opened_at, next_at: null, finished_at: incident.opened_at,
    })),
    next_before: null,
  })],

  // Dashboards
  [/^\/api\/dashboards$/, () => {
    // eslint-disable-next-line @typescript-eslint/no-unused-vars
    const { spec, revision, ...summaryRow } = DASHBOARD;
    return { dashboards: [summaryRow] };
  }],
  [/^\/api\/dashboards\/([^/]+)$/, ([id]) => {
    if (id !== DASHBOARD.id) throw new NotFound();
    return DASHBOARD;
  }],
  [/^\/api\/dashboards\/([^/]+)\/display-links$/, () => ({ links: [] })],
  [/^\/api\/maps$/, () => ({ maps: [] })],
  [/^\/api\/dashboard-data$/, (_, __, body) => {
    const request = body as { range: string; within: Within; panels: Panel[] };
    return dashboardData(request.range, request.panels);
  }],
  [/^\/api\/display\/data$/, () => dashboardData(DASHBOARD.spec.range, DASHBOARD.spec.panels)],
  [/^\/api\/dashboard-metrics$/, (_, q) => {
    const metric = q.get("metric");
    const instances = metric ? [...new Set(SERVERS.flatMap((s) => instancesOf(s, metric)).filter(Boolean))].sort() : [];
    return { metrics: ALL_METRICS, instances };
  }],
  [/^\/api\/metrics\/query$/, (_, q) => {
    const s = server(q.get("server_id") ?? "");
    const metric = q.get("metric") ?? "";
    const instance = q.get("instance") ?? undefined;
    const now = Math.floor(Date.now() / 60_000) * 60;
    const series = rangeSeries(s, metric, instance, now, now, 60);
    return { query: `${metric}{server_id="${s.id}"}`, series };
  }],
  [/^\/api\/metrics\/query_range$/, (_, q) => {
    const s = server(q.get("server_id") ?? "");
    const metric = q.get("metric") ?? "";
    const instance = q.get("instance") ?? undefined;
    const start = Date.parse(q.get("start") ?? "") / 1000;
    const end = Date.parse(q.get("end") ?? "") / 1000;
    const step = Math.max(15, Number(q.get("step")) || 60);
    return { query: `${metric}{server_id="${s.id}"}`, series: rangeSeries(s, metric, instance, start, end, step) };
  }],

  // Administration
  [/^\/api\/users$/, () => ({ users: USERS })],
  [/^\/api\/tokens$/, () => ({ tokens: [], next_cursor: null })],
  [/^\/api\/install-keys$/, () => ({
    keys: [{
      id: "00000000-0000-4000-8018-000000000001", role: "agent", site_id: SITES[1]!.id, site_name: SITES[1]!.name,
      customer_id: CUSTOMER.id, customer_name: CUSTOMER.name, created_by_username: "alex", created_at: ago(20 * DAY),
      expires_at: later(10 * DAY), max_hosts: 10, host_count: 3, require_approval: true, revoked_at: null, state: "active",
    }],
    pending_hosts: [],
  })],
  [/^\/api\/settings\/agent-install$/, () => AGENT_INSTALL],
  [/^\/api\/settings\/public-address$/, () => PUBLIC_ADDRESS],
  [/^\/api\/settings\/server$/, () => SERVER_SETTINGS],
  [/^\/api\/settings\/ai$/, () => ({
    enabled: true, from_config: false, provider: "openai_compatible", has_key: false,
    base_url: "https://ai.example.com/v1", model: "local-model", models: [],
  })],
  [/^\/api\/settings\/ai\/models$/, () => ({ models: ["local-model"], error: null })],
  [/^\/api\/plugins$/, () => ({ plugins: [] })],
  [/^\/api\/plugins\/usable$/, () => ({ plugins: [] })],
  [/^\/api\/plugins\/publishers$/, () => ({ publishers: [], official: [] })],
  [/^\/api\/checks\/usage$/, () => ({ checks: [] })],
  [/^\/api\/api-keys$/, () => ({ keys: API_KEYS })],
  [/^\/api\/v1\/openapi\.json$/, () => ({
    info: { title: "MonitorFlow API", version: SERVER_VERSION, description: "The demo leaves the API reference out." },
    paths: {},
    tags: [],
  })],
  [/^\/api\/server-update$/, () => SERVER_UPDATE],
  [/^\/api\/server-update\/history$/, () => ({
    updates: [{
      id: SERVER_UPDATE.update!.id, from: "0.5.2", to: SERVER_VERSION, requested_by: "alex", started_at: ago(10 * DAY + 10),
      finished_at: ago(10 * DAY + 3), result: "succeeded", error: null, inferred: false,
    }],
  })],
  [/^\/api\/license$/, () => ({ available: false })],
  [/^\/api\/backups$/, () => BACKUPS],
  [/^\/api\/certificate-authority$/, () => CERTIFICATE_AUTHORITY],
];

function route(url: URL, body: unknown): unknown {
  for (const [pattern, handler] of ROUTES) {
    const match = pattern.exec(url.pathname);
    if (match) return handler(match.slice(1).map(decodeURIComponent), url.searchParams, body);
  }
  throw new NotFound();
}

function certificate(seed: string) {
  return {
    serial: fakeHash(seed).slice(0, 32), not_before: ago(30 * DAY), not_after: later(335 * DAY), issued_at: ago(30 * DAY),
    revoked: false, revoked_at: null, revocation_reason: null,
  };
}

function serverDetail(s: DemoServer): ServerDetail {
  return { ...live(s), certificate: certificate(s.id), ai_commands_change: null };
}

function sourcesOf(s: DemoServer): SourcesStatus {
  if (isWindows(s)) {
    return {
      using: "own", mirrors: [], changing_to: null, failed: null, campaign: false, distribution: null, problem: null, add_mirror: false,
      windows_update_source: {
        origin: "microsoft", wsus_url: null, status_url: null, download_url: null, managed: true, automatic: "download",
        no_internet: false, quality_from_microsoft: false, feature_from_microsoft: false, quality_deferral_days: 7, feature_deferral_days: 0,
      },
    };
  }
  const mirrorNames =
    s.os_family === "rhel" ? [] : s.os_version?.startsWith("Ubuntu 24") ? ["ubuntu-noble"] : s.os_version?.startsWith("Debian") ? ["debian-bookworm", "debian-bookworm-security"] : [];
  const mirrors = MIRRORS.filter((m) => mirrorNames.includes(m.name)).map((m) => ({ name: m.name, snapshot: m.current_snapshot! }));
  return {
    using: mirrors.length ? "mirror" : "own", mirrors, changing_to: null, failed: null, campaign: false,
    distribution: s.distribution, problem: null, add_mirror: false, windows_update_source: null,
  };
}

function incidents(q: Params): IncidentList {
  const list = INCIDENTS.filter(
    (i) =>
      (!q.get("state") || i.state === q.get("state")) &&
      (!q.get("severity") || i.severity === q.get("severity")) &&
      (!q.get("server_id") || i.server_id === q.get("server_id")) &&
      (!q.get("site_id") || i.site_id === q.get("site_id")),
  );
  return { incidents: list.map(incidentRow), next_cursor: null };
}

const THRESHOLDS: EffectiveThreshold[] = [
  { origin: "default", threshold_id: null, metric: "monitorflow_cpu_usage_ratio", instance: null, enabled: true, comparison: ">", warning: 0.85, critical: 0.95, duration_s: 600, hysteresis: 0.05 },
  { origin: "default", threshold_id: null, metric: "monitorflow_memory_available_ratio", instance: null, enabled: true, comparison: "<", warning: 0.1, critical: 0.05, duration_s: 600, hysteresis: 0.02 },
  { origin: "default", threshold_id: null, metric: "monitorflow_disk_used_ratio", instance: null, enabled: true, comparison: ">", warning: 0.85, critical: 0.95, duration_s: 300, hysteresis: 0.02 },
  { origin: "site", threshold_id: "00000000-0000-4000-8019-000000000001", metric: "monitorflow_services_active", instance: null, enabled: true, comparison: "<", warning: null, critical: 1, duration_s: 120, hysteresis: 0 },
];

/** The servers a panel covers. */
function panelServers(panel: Panel): DemoServer[] {
  switch (panel.servers.kind) {
    case "all":
      return SERVERS;
    case "site": {
      const id = panel.servers.id;
      return SERVERS.filter((s) => s.site_id === id);
    }
    case "customer":
      return SERVERS;
    case "list": {
      const ids = panel.servers.ids;
      return SERVERS.filter((s) => ids.includes(s.id));
    }
  }
}

const RANGE_SECONDS: Record<string, number> = { "1h": 3600, "6h": 6 * 3600, "24h": 86_400, "7d": 7 * 86_400, "30d": 30 * 86_400 };

function dashboardData(range: string, panels: Panel[]): DashboardData {
  const now = Math.floor(Date.now() / 1000);
  const span = RANGE_SECONDS[range] ?? 86_400;
  const step = Math.max(60, Math.round(span / 240 / 60) * 60);
  const end = Math.floor(now / step) * step;
  const start = end - span;
  const latest = (s: DemoServer, metric: string, instance?: string): { instance: string; value: number | null }[] =>
    (instance ? [instance] : instancesOf(s, metric)).map((i) => ({ instance: i, value: valueAt(s, metric, i, now - 60) }));

  return {
    start, end, step, at: now, now,
    panels: panels.map((panel): PanelData => {
      const servers = panelServers(panel);
      const metric = panel.metric ?? "";
      const base = { id: panel.id, servers: servers.length, unavailable: 0 };
      switch (baseKind(panel.kind)) {
        case "alerts":
          return {
            ...base, kind: "alerts",
            alerts: INCIDENTS.filter((i) => i.state === "open" && servers.some((s) => s.id === i.server_id)).map((i) => ({
              id: i.id, server_id: i.server_id!, hostname: i.hostname, metric: i.metric, instance: i.instance, severity: i.severity, opened_at: i.opened_at,
            })),
          };
        case "figure": {
          const values = servers.flatMap((s) => latest(s, metric, panel.instance)).map((v) => v.value).filter((v): v is number => v !== null);
          return { ...base, kind: "figure", value: combine(values, panel.combine ?? "avg") };
        }
        case "top": {
          const rows = servers
            .flatMap((s) => latest(s, metric, panel.instance).map((v) => ({ server_id: s.id, hostname: s.hostname, instance: v.instance || null, value: v.value })))
            .filter((r) => r.value !== null)
            .sort((a, b) => (panel.order === "lowest" ? a.value! - b.value! : b.value! - a.value!))
            .slice(0, panel.count ?? 5);
          return { ...base, kind: "top", rows };
        }
        case "text":
          return { ...base, kind: "text" };
        case "map":
          return { ...base, kind: "error", error: "Network maps are not in the demo." };
        case "states":
          return {
            ...base,
            kind: "states",
            cells: servers.map((s) => {
              if (!panel.metric) return { server_id: s.id, hostname: s.hostname, state: demoState(s.id) };
              const values = latest(s, metric, panel.instance).map((v) => v.value).filter((v): v is number => v !== null);
              return { server_id: s.id, hostname: s.hostname, value: values.length ? Math.max(...values) : null };
            }),
          };
        case "pie": {
          const counts = new Map<ServerState, number>();
          for (const s of servers) counts.set(demoState(s.id), (counts.get(demoState(s.id)) ?? 0) + 1);
          return { ...base, kind: "pie", slices: [...counts].map(([state, value]) => ({ label: state, state, value })) };
        }
        case "timeline":
          return {
            ...base,
            kind: "timeline",
            rows: servers.slice(0, panel.count ?? 10).map((s) => ({
              server_id: s.id,
              hostname: s.hostname,
              spans: INCIDENTS.filter((i) => i.server_id === s.id).map((i) => ({
                start: Math.max(start, Math.floor(Date.parse(i.opened_at) / 1000)),
                end: i.resolved_at ? Math.floor(Date.parse(i.resolved_at) / 1000) : now,
                severity: i.severity,
              })).filter((x) => x.end > x.start),
            })),
          };
        case "table": {
          const columns = panel.columns ?? [];
          const worst = (s: DemoServer, metric: string, instance?: string) => {
            const values = latest(s, metric, instance).map((v) => v.value).filter((v): v is number => v !== null);
            return values.length ? Math.max(...values) : null;
          };
          const rows = servers
            .map((s) => ({ server_id: s.id, hostname: s.hostname, values: columns.map((c) => worst(s, c.metric, c.instance)) }))
            .filter((r) => r.values[0] !== null)
            .sort((a, b) => (panel.order === "lowest" ? a.values[0]! - b.values[0]! : b.values[0]! - a.values[0]!))
            .slice(0, panel.count ?? 20);
          return { ...base, kind: "table", rows };
        }
        case "chart": {
          const lines = servers.flatMap((s) =>
            rangeSeries(s, metric, panel.instance, start, end, step).map((series) => ({
              label: series.labels.instance && !panel.instance ? `${s.hostname} ${series.labels.instance}` : s.hostname,
              server_id: s.id,
              points: series.points,
            })),
          );
          if (panel.kind === "heatmap") return { ...base, kind: "chart", series: lines.slice(0, panel.count ?? 10) };
          if ((panel.combine ?? "each") === "each") return { ...base, kind: "chart", series: lines };
          const points = lines[0]?.points.map(([t], i) => {
            const values = lines.map((l) => l.points[i]?.[1]).filter((v): v is number => typeof v === "number");
            return [t, combine(values, panel.combine!)] as [number, number | null];
          }) ?? [];
          return { ...base, kind: "chart", series: [{ label: panel.title, points }] };
        }
      }
    }),
  };
}

/** A demo server's state: its worst open incident, else OK. */
function demoState(serverId: string): ServerState {
  const open = INCIDENTS.filter((i) => i.state === "open" && i.server_id === serverId);
  return open.some((i) => i.severity === "critical") ? "critical" : open.length ? "warning" : "ok";
}

function combine(values: number[], how: string): number | null {
  if (values.length === 0) return null;
  switch (how) {
    case "max":
      return Math.max(...values);
    case "min":
      return Math.min(...values);
    case "sum":
      return values.reduce((a, b) => a + b, 0);
    case "count":
      return values.length;
    default:
      return values.reduce((a, b) => a + b, 0) / values.length;
  }
}

const PUBLIC_ADDRESS: PublicAddress = {
  address: "monitorflow.example.com",
  default: "monitorflow.example.com",
  names: ["monitorflow.example.com"],
  certified: true,
  upstream_url: "https://monitorflow.example.com:7443",
  enroll_url: "https://monitorflow.example.com:7445",
};

const AGENT_INSTALL: AgentInstall = {
  source: "central",
  custom_url: null,
  source_from_config: false,
  custom_url_from_config: false,
  github_url: "https://github.com/Kidoly/monitorflow/releases",
  central: {
    version: AGENT_VERSION, path: `/agent/${AGENT_VERSION}`, sums_sha256: fakeHash("sums"), installer_sha256: fakeHash("installer"), msi_sha256: fakeHash("msi"),
  },
};

const SERVER_SETTINGS: ServerSettings = {
  session_lifetime: { value: 7 * 86_400, default: 7 * 86_400, locked: false },
  session_idle_timeout: { value: 8 * 3600, default: 8 * 3600, locked: false },
  mirror_public_url: { value: null, default: null, locked: false },
  retention: {
    metrics: { value: "90d", locked: false }, logs: { value: "30d", locked: false }, default_metrics: "90d", default_logs: "30d",
    max_days: 3650, updater_alive: true, applying: false, error: null,
  },
};

const SERVER_UPDATE: ServerUpdateView = {
  running_version: SERVER_VERSION,
  enabled: true,
  source: "registry.example.com/monitorflow",
  updater: {
    version: SERVER_VERSION, heartbeat_at: ago(0), alive: true,
    running: { version: SERVER_VERSION, server_image: null, web_image: null, updater_image: null },
  },
  update: {
    id: "00000000-0000-4000-801a-000000000001", from: "0.5.2", to: SERVER_VERSION, requested_by: "alex", state: "succeeded",
    steps: [
      { at: ago(10 * DAY + 10), message: `release ${SERVER_VERSION} verified: server, web and updater images` },
      { at: ago(10 * DAY + 9), message: "images pulled" },
      { at: ago(10 * DAY + 8), message: "database and server data saved to monitorflow-update-0.5.2.dump" },
      { at: ago(10 * DAY + 7), message: "new images pinned; migrating the database" },
      { at: ago(10 * DAY + 6), message: "recreating server and web" },
      { at: ago(10 * DAY + 4), message: `server ${SERVER_VERSION} is healthy` },
      { at: ago(10 * DAY + 3), message: `agent release ${SERVER_VERSION} cached for agents` },
    ],
    error: null, backup: "monitorflow-update-0.5.2.dump", migrated: true, started_at: ago(10 * DAY + 10), finished_at: ago(10 * DAY + 3),
  },
  milestones: [
    { kind: "verified", done_at: ago(10 * DAY + 10) },
    { kind: "images_pulled", done_at: ago(10 * DAY + 9) },
    { kind: "backed_up", done_at: ago(10 * DAY + 8) },
    { kind: "database_migrated", done_at: ago(10 * DAY + 6) },
    { kind: "server_restarted", done_at: ago(10 * DAY + 4) },
    { kind: "agents_ready", done_at: ago(10 * DAY + 3) },
  ],
  requested: false,
  available: null,
};

const BACKUPS: BackupsView = {
  enabled: true, downloads: false, host_dir: true, updater_alive: true, requested: false, keep: 7,
  last: {
    id: "00000000-0000-4000-801b-000000000001", kind: "daily", requested_by: "", state: "succeeded", error: null, warning: null,
    file: "monitorflow-daily.dump", started_at: ago(8 * HOUR), finished_at: ago(8 * HOUR - 2),
  },
  last_success_at: ago(8 * HOUR - 2),
  next_at: later(16 * HOUR),
  files: [0, 1, 2, 3, 4, 5, 6].map((d) => ({
    name: `monitorflow-daily-${d + 1}.dump`, size: 1_250_000_000 - d * 8_000_000, created_at: ago(d * DAY + 8 * HOUR), kind: "daily" as const,
  })),
};

const CERTIFICATE_AUTHORITY: CertificateAuthorityView = {
  fingerprint: fakeHash("certificate-authority"),
  created_at: ago(220 * DAY),
  expires_at: later(9 * 365 * DAY),
  expires_soon: false,
  auto_change_at: later(8 * 365 * DAY),
  restored_retired_at: null,
  cut_off: [],
  rotation: null,
};

function autoUpdate(): AutoUpdateStatus {
  const totals = (n: number) => ({ up_to_date: n, waiting: 0, waiting_for_relay: 0, pending: 0, succeeded: n, failed: 0 });
  const sites = SITES.map((s) => ({
    id: s.id, name: s.name, customer_name: CUSTOMER.name, auto_update: null, enabled: true, phase: null, totals: totals(s.server_count),
  }));
  const history = [{
    kind: "agent" as const, version: AGENT_VERSION, state: "rolling" as const, started_at: ago(9 * DAY), halted_at: null, reason: null, counted_from: ago(9 * DAY),
    // Every server runs the target, so the server reports it finished.
    outcome: "finished" as const,
  }];
  return {
    settings: { enabled: true, enabled_locked: false, paused: false, failure_threshold: 3 },
    server_version: SERVER_VERSION,
    agents: {
      latest: AGENT_VERSION, target: AGENT_VERSION, rollout: null, totals: totals(SERVERS.length), sites, history,
      versions: [{ version: AGENT_VERSION, count: SERVERS.length, up_to_date: true }],
    },
    relays: {
      latest: AGENT_VERSION, target: AGENT_VERSION, rollout: null, totals: totals(1), sites: [sites[0]!], history: [],
      versions: [{ version: AGENT_VERSION, count: 1, up_to_date: true }],
    },
  };
}

/** Every demo agent or relay runs the target. */
function versionPeers(kind: string): AutoUpdateVersionPeerList {
  const site = (id: string) => SITES.find((s) => s.id === id)?.name ?? "";
  const peers = kind === "relay"
    ? [{ id: RELAY.id, name: RELAY.name, site_id: RELAY.site_id }]
    : SERVERS.map((s) => ({ id: s.id, name: s.hostname, site_id: s.site_id }));
  return {
    version: AGENT_VERSION,
    target: AGENT_VERSION,
    total: peers.length,
    peers: peers.map((p) => ({
      ...p, site_name: site(p.site_id), customer_id: CUSTOMER.id, customer_name: CUSTOMER.name, state: "up_to_date" as const,
    })),
  };
}

/** A demo server as an end of a dependency. */
function endOf(s: Server): DependencyEnd {
  return { kind: s.kind === "device" ? "device" : "server", server_id: s.id, relay_id: null, name: s.hostname, site_id: s.site_id, check: null };
}

/** A demo server behind the demo relay, automatically. */
function behindRelay(s: Server): Dependency {
  const parent: DependencyEnd = { kind: "relay", server_id: null, relay_id: RELAY.id, name: RELAY.name, site_id: RELAY.site_id, check: null };
  return { id: null, automatic: true, child: endOf(s), parent, created_at: null, created_by: null, can_change: false };
}

function noDependencies(): DependencyPage {
  return { parents: [], children: [], behind: [], suggestions: [], unreachable: null, unreachable_checks: [], can_change: false };
}


/** Seconds down, by days ago (0 is today), of the demo servers that had an outage. */
const OUTAGES: Record<string, Record<number, number>> = {
  "web-02": { 2: 180 },
  "web-03": { 31: 2_400 },
  "proxy-01": { 9: 300 },
  "db-02": { 40: 4_800 },
  "app-04": { 0: 2_520 },
  "mail-01": { 63: 360 },
  "files-01": { 12: 1_500, 47: 900 },
  "br-web-01": { 18: 7_200 },
  "br-files-01": { 18: 7_200 },
};

/** A demo server's 90 days and its four windows, worked out from `OUTAGES`. */
export function uptimeOf(hostname: string, today: Date) {
  const outages = OUTAGES[hostname] ?? {};
  const percent = (down: number, counted: number) => Math.round((1 - down / counted) * 100_000) / 1000;
  const days = Array.from({ length: 90 }, (_, i) => {
    const ago = 89 - i;
    const down = outages[ago] ?? 0;
    const date = new Date(today.getTime() - ago * 86_400_000).toISOString().slice(0, 10);
    return { date, availability: percent(down, 86_400), down_seconds: down, maintenance_seconds: 0 };
  });
  const over = (n: number) => {
    const last = days.slice(-n);
    return percent(last.reduce((sum, d) => sum + d.down_seconds, 0), n * 86_400);
  };
  return {
    windows: { day: over(1), week: over(7), month: over(30), quarter: over(90) },
    outages: Object.keys(outages).length,
    days,
  };
}
