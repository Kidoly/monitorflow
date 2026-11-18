// Everything else the demo shows, consistent with its fleet: incidents and
// their AI remediation, the audit log, update campaigns, mirrors, channels,
// on-call, a dashboard and the settings pages.

import type {
  ApiKey,
  Campaign,
  CampaignDetail,
  CampaignServer,
  CampaignServerStatus,
  Command,
  Dashboard,
  FollowUpPolicyList,
  Incident,
  IncidentDetail,
  IntegrationList,
  LogLine,
  Mirror,
  MirrorDetail,
  NotificationChannel,
  OnCallSchedule,
  OnCallTurn,
  Package,
  PendingApproval,
  RemediationEvent,
  RemediationEventKind,
  RemediationIncidentDetail,
  ActorKind,
  MaintenanceWindow,
  Silence,
  ThresholdSnapshot,
} from "../api";

import { BRANCH, CUSTOMER, DATACENTER, type DemoServer, SERVERS, USERS, demoId, isWindows, serverByName } from "./fleet";
import { DAY, HOUR, START, ago, later } from "./time";

const user = (name: string) => USERS.find((u) => u.username === name)!;

/** A made-up SHA-256 in hex, stable for `seed`. */
export function fakeHash(seed: string): string {
  let out = "";
  let h = 2166136261;
  for (let i = 0; out.length < 64; i += 1) {
    h ^= seed.charCodeAt(i % seed.length) + i;
    h = Math.imul(h, 16777619) >>> 0;
    out += h.toString(16).padStart(8, "0");
  }
  return out.slice(0, 64);
}

// Incidents ---------------------------------------------------------------

const DISK: ThresholdSnapshot = { metric: "monitorflow_disk_used_ratio", instance: null, comparison: ">", warning: 0.85, critical: 0.95, duration_s: 300, hysteresis: 0.02 };
const CPU: ThresholdSnapshot = { metric: "monitorflow_cpu_usage_ratio", instance: null, comparison: ">", warning: 0.85, critical: 0.95, duration_s: 600, hysteresis: 0.05 };
const MEMORY: ThresholdSnapshot = { metric: "monitorflow_memory_available_ratio", instance: null, comparison: "<", warning: 0.1, critical: 0.05, duration_s: 600, hysteresis: 0.02 };
const SERVICE: ThresholdSnapshot = { metric: "monitorflow_services_active", instance: null, comparison: "<", warning: null, critical: 1, duration_s: 120, hysteresis: 0 };
const OFFLINE: ThresholdSnapshot = { metric: "offline", instance: null, comparison: ">", warning: null, critical: null, duration_s: 300, hysteresis: 0 };

interface IncidentSpec {
  host: string;
  threshold: ThresholdSnapshot;
  instance: string;
  severity: "warning" | "critical";
  openedMinutes: number;
  resolvedMinutes?: number;
  last: number;
  peak: number;
  reason: string;
  ack?: { by: string; minutes: number; note: string };
}

const INCIDENT_SPECS: IncidentSpec[] = [
  { host: "db-02", threshold: DISK, instance: "/var/lib/postgresql", severity: "critical", openedMinutes: 3 * HOUR + 12, last: 0.96, peak: 0.96, reason: "96% used is above the critical limit of 95%" },
  { host: "files-01", threshold: SERVICE, instance: "DFSR", severity: "critical", openedMinutes: 36, last: 0, peak: 0, reason: "The DFSR service is stopped" },
  { host: "app-04", threshold: OFFLINE, instance: "", severity: "critical", openedMinutes: 42, last: 0, peak: 0, reason: "No message from the agent for 47 minutes" },
  { host: "web-03", threshold: CPU, instance: "total", severity: "warning", openedMinutes: 58, last: 0.89, peak: 0.92, reason: "89% is above the warning limit of 85%" },
  {
    host: "mail-01", threshold: MEMORY, instance: "", severity: "warning", openedMinutes: 80, last: 0.085, peak: 0.07, reason: "8% available is below the warning limit of 10%",
    ack: { by: "sam", minutes: 25, note: "Looking into the mail queue, a large newsletter went out." },
  },
  // Resolved ones, newest first.
  { host: "web-02", threshold: SERVICE, instance: "nginx.service", severity: "critical", openedMinutes: 2 * DAY + 3 * HOUR, resolvedMinutes: 2 * DAY + 3 * HOUR - 4, last: 1, peak: 0, reason: "Peak: the nginx.service service was stopped" },
  { host: "build-01", threshold: CPU, instance: "total", severity: "warning", openedMinutes: 3 * DAY + 5 * HOUR, resolvedMinutes: 3 * DAY + 4 * HOUR, last: 0.4, peak: 0.93, reason: "Peak: 93%" },
  { host: "sql-01", threshold: DISK, instance: "D:", severity: "warning", openedMinutes: 5 * DAY, resolvedMinutes: 4 * DAY + 20 * HOUR, last: 0.71, peak: 0.88, reason: "Peak: 88% used" },
  { host: "br-files-01", threshold: OFFLINE, instance: "", severity: "critical", openedMinutes: 6 * DAY + 2 * HOUR, resolvedMinutes: 6 * DAY + HOUR, last: 1, peak: 0, reason: "Peak: no message from the agent for 1 hour" },
  { host: "app-02", threshold: MEMORY, instance: "", severity: "warning", openedMinutes: 8 * DAY, resolvedMinutes: 8 * DAY - 35, last: 0.3, peak: 0.08, reason: "Peak: 8% available" },
  { host: "backup-01", threshold: DISK, instance: "/srv/backup", severity: "warning", openedMinutes: 11 * DAY, resolvedMinutes: 10 * DAY, last: 0.8, peak: 0.87, reason: "Peak: 87% used" },
  { host: "proxy-01", threshold: SERVICE, instance: "haproxy.service", severity: "critical", openedMinutes: 15 * DAY, resolvedMinutes: 15 * DAY - 2, last: 1, peak: 0, reason: "Peak: the haproxy.service service was stopped" },
];

export const INCIDENTS: IncidentDetail[] = INCIDENT_SPECS.map((spec, i) => {
  const server = serverByName(spec.host);
  const open = spec.resolvedMinutes === undefined;
  const events: IncidentDetail["events"] = [
    { id: i * 10 + 1, kind: "opened", severity: spec.severity, value: spec.peak, occurred_at: ago(spec.openedMinutes), recorded_at: ago(spec.openedMinutes), details: {} },
  ];
  if (spec.severity === "critical" && spec.threshold === DISK) {
    events[0]!.severity = "warning";
    events.push({ id: i * 10 + 2, kind: "escalated", severity: "critical", value: spec.last, occurred_at: ago(50), recorded_at: ago(50), details: {} });
  }
  if (spec.ack) {
    events.push({ id: i * 10 + 3, kind: "acknowledged", severity: spec.severity, value: null, occurred_at: ago(spec.ack.minutes), recorded_at: ago(spec.ack.minutes), details: { username: spec.ack.by, note: spec.ack.note } });
  }
  if (!open) {
    events.push({ id: i * 10 + 4, kind: "resolved", severity: null, value: spec.last, occurred_at: ago(spec.resolvedMinutes!), recorded_at: ago(spec.resolvedMinutes!), details: {} });
  }
  return {
    id: demoId(8, i + 1),
    server_id: server.id,
    relay_id: null,
    hostname: server.hostname,
    site_id: server.site_id,
    site_name: server.site_name,
    customer_id: CUSTOMER.id,
    metric: spec.threshold.metric,
    instance: spec.instance,
    severity: spec.severity,
    state: open ? "open" : "resolved",
    opened_at: ago(spec.openedMinutes),
    resolved_at: open ? null : ago(spec.resolvedMinutes!),
    last_value: spec.last,
    last_value_at: open ? ago(0) : ago(spec.resolvedMinutes!),
    peak_value: spec.peak,
    threshold: { ...spec.threshold, instance: spec.instance || null },
    silenced_until: null,
    silenced: false,
    acknowledged_by: spec.ack?.by ?? null,
    acknowledged_at: spec.ack ? ago(spec.ack.minutes) : null,
    acknowledged_note: spec.ack?.note ?? null,
    can_acknowledge: open && !spec.ack,
    reason: spec.reason,
    events,
    notification_groups: [{ id: demoId(11, i + 1), incidents: 1, created_at: ago(spec.openedMinutes - 1) }],
    acknowledged_via: spec.ack ? "page" : null,
    follow_up:
      open && spec.severity === "critical"
        ? { state: "waiting", stop_reason: null, step: 1, steps: 3, started_at: ago(spec.openedMinutes), next_at: later(14), next: ["alex"], policy: "default" }
        : null,
  };
});

/** The row of `GET /api/incidents`: without the detail's fields. */
export function incidentRow(incident: IncidentDetail): Incident {
  // eslint-disable-next-line @typescript-eslint/no-unused-vars
  const { events, notification_groups, acknowledged_via, follow_up, ...row } = incident;
  return row;
}

// AI remediation -----------------------------------------------------------

const DB02 = serverByName("db-02");
const WEB02 = serverByName("web-02");
const MODEL = "local-model";

const PROPOSED =
  "find /var/lib/postgresql/16/main/log -name 'postgresql-*.log' -mtime +14 -delete";

export const APPROVAL: PendingApproval = {
  id: demoId(10, 1),
  incident_id: demoId(9, 1),
  customer_id: CUSTOMER.id,
  customer_name: CUSTOMER.name,
  site_id: DB02.site_id,
  site_name: DB02.site_name,
  server_id: DB02.id,
  hostname: DB02.hostname,
  alert_incident_id: INCIDENTS[0]!.id,
  metric: "monitorflow_disk_used_ratio",
  instance: "/var/lib/postgresql",
  command: PROPOSED,
  diagnosis:
    "The volume holding PostgreSQL is 96% full. PostgreSQL's own log files take 21 GB of it: logging_collector writes one file a day, and nothing removes the old ones. The database files themselves grow slowly.",
  reasoning:
    "Deleting log files older than two weeks frees about 18 GB and leaves recent logs for diagnosis. It touches no database file. Setting log_truncate_on_rotation and a weekly file name would stop it from happening again; that change is left to an administrator.",
  tier: "low_impact",
  rule: { source: "builtin", id: "find-delete-logs" },
  reason: "Deletes files under a log directory only",
  commands_run: 2,
  max_commands: 5,
  opened_at: ago(48),
  proposed_at: ago(6),
  deadline: later(2 * HOUR),
  can_approve: true,
  can_reject: true,
  snapshots: { available: true, reason: null, method: "lvm", detail: "an LVM snapshot of /var/lib/postgresql, kept 3 days", left_out: null },
  shell_acceptance: "accepted",
  shell: "posix",
};

interface EventSpec {
  kind: RemediationEventKind;
  actor: ActorKind;
  actorId: string;
  minutes: number;
  payload: Record<string, unknown>;
}

function chain(incidentId: string, specs: EventSpec[]) {
  let prev = "0".repeat(64);
  return specs.map((spec, i) => {
    const hash = fakeHash(`${incidentId}:${i}`);
    const event = {
      seq: i + 1,
      kind: spec.kind,
      actor_kind: spec.actor,
      actor_id: spec.actorId,
      payload: spec.payload,
      created_at: ago(spec.minutes),
      prev_hash: prev,
      hash,
    };
    prev = hash;
    return event;
  });
}

const DB02_EVENTS = chain(demoId(9, 1), [
  { kind: "state_change", actor: "system", actorId: "engine", minutes: 48, payload: { from: null, to: "open" } },
  { kind: "context_sent", actor: "system", actorId: "engine", minutes: 48, payload: { model: MODEL, context: { redactions: { ip: 3 } } } },
  { kind: "ai_proposal", actor: "ai", actorId: MODEL, minutes: 47, payload: { command: "df -h /var/lib/postgresql && du -sh /var/lib/postgresql/16/main/*", diagnosis: "Find what fills the volume." } },
  { kind: "classification", actor: "system", actorId: "engine", minutes: 47, payload: { tier: "read_only", rule: { source: "builtin", id: "df-du" }, reason: "Reads disk usage only" } },
  { kind: "approval", actor: "system", actorId: "policy", minutes: 47, payload: { command: "df -h /var/lib/postgresql && du -sh /var/lib/postgresql/16/main/*" } },
  { kind: "command_queued", actor: "system", actorId: "engine", minutes: 47, payload: { command: "df -h /var/lib/postgresql && du -sh /var/lib/postgresql/16/main/*" } },
  { kind: "command_result", actor: "peer", actorId: DB02.peer_id, minutes: 46, payload: { exit_code: 0, duration_ms: 1840 } },
  { kind: "ai_proposal", actor: "ai", actorId: MODEL, minutes: 45, payload: { command: "ls -lh --sort=size /var/lib/postgresql/16/main/log | head -20", diagnosis: "The log directory takes 21 GB." } },
  { kind: "classification", actor: "system", actorId: "engine", minutes: 45, payload: { tier: "read_only", rule: { source: "builtin", id: "ls" }, reason: "Lists files only" } },
  { kind: "approval", actor: "system", actorId: "policy", minutes: 45, payload: { command: "ls -lh --sort=size /var/lib/postgresql/16/main/log | head -20" } },
  { kind: "command_queued", actor: "system", actorId: "engine", minutes: 45, payload: { command: "ls -lh --sort=size /var/lib/postgresql/16/main/log | head -20" } },
  { kind: "command_result", actor: "peer", actorId: DB02.peer_id, minutes: 44, payload: { exit_code: 0, duration_ms: 95 } },
  { kind: "ai_proposal", actor: "ai", actorId: MODEL, minutes: 6, payload: { command: PROPOSED, diagnosis: "Old PostgreSQL log files fill the volume." } },
  { kind: "classification", actor: "system", actorId: "engine", minutes: 6, payload: { tier: "low_impact", rule: { source: "builtin", id: "find-delete-logs" }, reason: "Deletes files under a log directory only" } },
  { kind: "state_change", actor: "system", actorId: "engine", minutes: 6, payload: { from: "running", to: "awaiting_approval" } },
]);

const WEB02_INCIDENT = INCIDENTS.find((i) => i.hostname === "web-02")!;
const WEB02_MINUTES = 2 * DAY + 3 * HOUR;
const WEB02_EVENTS = chain(demoId(9, 2), [
  { kind: "state_change", actor: "system", actorId: "engine", minutes: WEB02_MINUTES, payload: { from: null, to: "open" } },
  { kind: "context_sent", actor: "system", actorId: "engine", minutes: WEB02_MINUTES, payload: { model: MODEL } },
  { kind: "ai_proposal", actor: "ai", actorId: MODEL, minutes: WEB02_MINUTES - 1, payload: { command: "systemctl restart nginx.service", diagnosis: "nginx stopped after a configuration reload failed; the configuration now tests clean." } },
  { kind: "classification", actor: "system", actorId: "engine", minutes: WEB02_MINUTES - 1, payload: { tier: "low_impact", rule: { source: "builtin", id: "systemctl-restart" }, reason: "Restarts one service" } },
  { kind: "approval", actor: "user", actorId: user("robin").id, minutes: WEB02_MINUTES - 2, payload: { command: "systemctl restart nginx.service" } },
  { kind: "snapshot", actor: "peer", actorId: WEB02.peer_id, minutes: WEB02_MINUTES - 2, payload: { method: "lvm", name: "mf-before-restart" } },
  { kind: "command_queued", actor: "system", actorId: "engine", minutes: WEB02_MINUTES - 2, payload: { command: "systemctl restart nginx.service" } },
  { kind: "command_result", actor: "peer", actorId: WEB02.peer_id, minutes: WEB02_MINUTES - 3, payload: { exit_code: 0, duration_ms: 412 } },
  { kind: "ai_proposal", actor: "ai", actorId: MODEL, minutes: WEB02_MINUTES - 4, payload: { stop: "resolved", reason: "nginx is running and answers on port 443." } },
  { kind: "state_change", actor: "system", actorId: "engine", minutes: WEB02_MINUTES - 4, payload: { from: "running", to: "resolved" } },
]);

export const REMEDIATIONS: RemediationIncidentDetail[] = [
  {
    id: demoId(9, 1), server_id: DB02.id, hostname: DB02.hostname, alert_incident_id: INCIDENTS[0]!.id,
    metric: APPROVAL.metric, instance: APPROVAL.instance, state: "awaiting_approval", opened_at: ago(48), closed_at: null,
    max_commands: 5, max_duration_s: 3600, event_count: DB02_EVENTS.length,
    site_name: DB02.site_name, customer_name: CUSTOMER.name, events: DB02_EVENTS,
    chain: { valid: true, events: DB02_EVENTS.length, broken_at: null, reason: null, head_hash: DB02_EVENTS.at(-1)!.hash },
  },
  {
    id: demoId(9, 2), server_id: WEB02.id, hostname: WEB02.hostname, alert_incident_id: WEB02_INCIDENT.id,
    metric: WEB02_INCIDENT.metric, instance: WEB02_INCIDENT.instance, state: "resolved", opened_at: ago(WEB02_MINUTES), closed_at: ago(WEB02_MINUTES - 4),
    max_commands: 5, max_duration_s: 3600, event_count: WEB02_EVENTS.length,
    site_name: WEB02.site_name, customer_name: CUSTOMER.name, events: WEB02_EVENTS,
    chain: { valid: true, events: WEB02_EVENTS.length, broken_at: null, reason: null, head_hash: WEB02_EVENTS.at(-1)!.hash },
  },
];

/** The audit log: every remediation event, newest first. */
export const AUDIT: RemediationEvent[] = REMEDIATIONS.flatMap((incident) => {
  const server = SERVERS.find((s) => s.id === incident.server_id)!;
  return incident.events.map((e) => ({
    incident_id: incident.id,
    seq: e.seq,
    kind: e.kind,
    actor_kind: e.actor_kind,
    actor_id: e.actor_id,
    actor_name: e.actor_kind === "user" ? (USERS.find((u) => u.id === e.actor_id)?.username ?? null) : null,
    payload: e.payload,
    created_at: e.created_at,
    hash: e.hash,
    customer_id: CUSTOMER.id,
    customer_name: CUSTOMER.name,
    site_id: server.site_id,
    site_name: server.site_name,
    server_id: server.id,
    hostname: server.hostname,
    metric: incident.metric,
    instance: incident.instance,
  }));
}).sort((a, b) => b.created_at.localeCompare(a.created_at) || b.seq - a.seq);

// Silences -------------------------------------------------------------------

const BACKUP = serverByName("backup-01");
export const SILENCES: Silence[] = [
  {
    id: demoId(12, 1), scope: "server", target_id: BACKUP.id, target_name: BACKUP.hostname, site_id: BACKUP.site_id, customer_id: CUSTOMER.id,
    starts_at: ago(HOUR), ends_at: later(3 * HOUR), active: true, reason: "Disk replacement in the backup array",
    created_by_username: "alex", created_at: ago(HOUR + 5), can_end: true,
  },
  {
    id: demoId(12, 2), scope: "site", target_id: BRANCH.id, target_name: BRANCH.name, site_id: BRANCH.id, customer_id: CUSTOMER.id,
    starts_at: later(4 * DAY), ends_at: later(4 * DAY + 4 * HOUR), active: false, reason: "Power work in the branch office building",
    created_by_username: "sam", created_at: ago(DAY), can_end: true,
  },
];

/** The next `count` Sundays at 02:00 UTC, two hours each, from now. */
function sundays(count: number): { starts_at: string; ends_at: string }[] {
  const day = new Date();
  day.setUTCHours(2, 0, 0, 0);
  day.setUTCDate(day.getUTCDate() + ((7 - day.getUTCDay()) % 7 || 7));
  return Array.from({ length: count }, (_, i) => {
    const start = new Date(day.getTime() + i * 7 * DAY * 60_000);
    return { starts_at: start.toISOString(), ends_at: new Date(start.getTime() + 2 * HOUR * 60_000).toISOString() };
  });
}

export const MAINTENANCE_WINDOWS: MaintenanceWindow[] = [
  {
    id: demoId(13, 1), site_id: DATACENTER.id, site_name: DATACENTER.name, customer_id: CUSTOMER.id,
    servers: [], checks: null, repeat: "weekly", every: 1, weekdays: ["sunday"], month_day: null, month_week: null,
    months: [1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12], start_time: "02:00", duration_minutes: 120, time_zone: "UTC",
    starts_on: "2026-01-04", ends_on: null, reason: "Weekly patches", active_until: null, next: sundays(3),
    created_by_username: "alex", created_at: ago(30 * DAY), updated_at: ago(30 * DAY), can_change: true,
  },
];

// Update campaigns -----------------------------------------------------------

const LINUX = SERVERS.filter((s) => !isWindows(s) && s.site_id === DATACENTER.id);
const WAVES = [
  ["web-04", "api-02", "cache-01"],
  ["web-01", "web-02", "proxy-02", "app-01", "app-02", "app-03", "api-01", "vpn-01"],
  LINUX.map((s) => s.hostname).filter((h) => !["web-04", "api-02", "cache-01", "web-01", "web-02", "proxy-02", "app-01", "app-02", "app-03", "api-01", "vpn-01"].includes(h)),
];

function campaignServer(hostname: string, status: CampaignServerStatus, n: number, minutes: number | null): CampaignServer {
  const server = serverByName(hostname);
  return {
    server_id: server.id, peer_id: server.peer_id, hostname, site_id: server.site_id, status,
    command_id: status === "pending" ? null : demoId(13, n),
    exit_code: status === "succeeded" ? 0 : null, error: status === "skipped" ? "The server is offline" : null,
    finished_at: minutes === null ? null : ago(minutes),
    sources_command_id: null, restore_command_id: null, snapshot_command_id: null,
  };
}

const progress = (servers: CampaignServer[]) => ({
  total: servers.length,
  pending: servers.filter((s) => s.status === "pending").length,
  in_progress: servers.filter((s) => ["queued", "sent", "running"].includes(s.status)).length,
  succeeded: servers.filter((s) => s.status === "succeeded").length,
  failed: servers.filter((s) => ["failed", "expired", "skipped"].includes(s.status)).length,
  cancelled: servers.filter((s) => s.status === "cancelled").length,
});

const WAVE_STATUS: CampaignServerStatus[][] = [
  ["succeeded", "succeeded", "succeeded"],
  ["succeeded", "succeeded", "succeeded", "succeeded", "running", "running", "queued", "queued"],
  WAVES[2]!.map(() => "pending"),
];

const waves: CampaignDetail["waves"] = WAVES.map((hosts, w) => {
  const servers = hosts.map((h, i) =>
    campaignServer(h, WAVE_STATUS[w]![i]!, w * 20 + i + 1, WAVE_STATUS[w]![i] === "succeeded" ? (w === 0 ? 50 * HOUR - i * 7 : 25 - i * 3) : null),
  );
  return {
    index: w,
    state: w === 0 ? "done" : w === 1 ? "running" : "pending",
    started_at: w === 0 ? ago(51 * HOUR) : w === 1 ? ago(40) : null,
    finished_at: w === 0 ? ago(50 * HOUR - 20) : null,
    progress: progress(servers),
    servers,
  };
});

const allServers = waves.flatMap((w) => w.servers);

const campaignBase = {
  customer_id: CUSTOMER.id,
  customer_name: CUSTOMER.name,
  site_id: null,
  error: null,
  command_ttl_s: 4 * 3600,
  failure_threshold: { value: 10, unit: "percent" as const },
  sources: null,
  agent_version: null,
  created_by: "alex",
};

export const CAMPAIGNS: CampaignDetail[] = [
  {
    ...campaignBase,
    id: demoId(14, 1),
    name: "Security updates",
    site_id: DATACENTER.id,
    state: "running",
    spec: { packages: [], security_only: true, allow_reboot: false, include_os: true, third_party: [] },
    windows: [{ kind: "weekly", weekdays: [1, 2, 3, 4, 5, 6, 7], start: "00:00", end: "23:59", timezone: "Europe/Paris" }],
    wave_count: 3,
    current_wave: 1,
    progress: progress(allServers),
    created_at: ago(3 * DAY),
    started_at: ago(51 * HOUR),
    finished_at: null,
    waves,
  },
  {
    ...campaignBase,
    id: demoId(14, 2),
    name: "Windows updates",
    state: "completed",
    spec: { packages: [], security_only: false, allow_reboot: true, include_os: true, third_party: [] },
    windows: [{ kind: "weekly", weekdays: [6], start: "22:00", end: "04:00", timezone: "Europe/Paris" }],
    wave_count: 2,
    current_wave: 1,
    progress: { total: 8, pending: 0, in_progress: 0, succeeded: 8, failed: 0, cancelled: 0 },
    created_at: ago(30 * DAY),
    started_at: ago(27 * DAY),
    finished_at: ago(27 * DAY - 5 * HOUR),
    waves: [],
  },
  {
    ...campaignBase,
    id: demoId(14, 3),
    name: "Agent 0.6.0",
    agent_version: "0.6.0",
    state: "completed",
    spec: { packages: [], security_only: false, allow_reboot: false },
    windows: [],
    wave_count: 2,
    current_wave: 1,
    progress: { total: 30, pending: 0, in_progress: 0, succeeded: 30, failed: 0, cancelled: 0 },
    created_at: ago(9 * DAY),
    started_at: ago(9 * DAY),
    finished_at: ago(9 * DAY - 40),
    waves: [],
  },
  {
    ...campaignBase,
    id: demoId(14, 4),
    name: "Linux updates, last month",
    state: "completed",
    spec: { packages: [], security_only: false, allow_reboot: false, include_os: true, third_party: [] },
    windows: [],
    wave_count: 3,
    current_wave: 2,
    progress: { total: 22, pending: 0, in_progress: 0, succeeded: 21, failed: 1, cancelled: 0 },
    created_at: ago(34 * DAY),
    started_at: ago(33 * DAY),
    finished_at: ago(33 * DAY - 3 * HOUR),
    waves: [],
  },
];

export function campaignRow(detail: CampaignDetail): Campaign {
  // eslint-disable-next-line @typescript-eslint/no-unused-vars
  const { waves: _waves, ...row } = detail;
  return row;
}

// Commands, packages and logs of one server -------------------------------------

export function commandsOf(server: DemoServer): Command[] {
  const out: Command[] = [];
  const inCampaign = allServers.find((s) => s.server_id === server.id && s.command_id);
  if (inCampaign) {
    out.push({
      id: inCampaign.command_id!, server_id: server.id, kind: "apply_updates",
      parameters: { packages: [], security_only: true, allow_reboot: false },
      state: inCampaign.status === "pending" ? "queued" : (inCampaign.status as Command["state"]),
      issued_at: ago(40), expires_at: later(3 * HOUR), sent_at: ago(39), started_at: inCampaign.status === "queued" ? null : ago(38),
      finished_at: inCampaign.finished_at, exit_code: inCampaign.exit_code, error: null, requested_by: "alex", campaign_id: CAMPAIGNS[0]!.id,
    });
  }
  out.push({
    id: demoId(15, SERVERS.indexOf(server) + 1), server_id: server.id, kind: "self_update", parameters: { version: "0.6.0" },
    state: "succeeded", issued_at: ago(9 * DAY), expires_at: ago(9 * DAY - 60), sent_at: ago(9 * DAY), started_at: ago(9 * DAY),
    finished_at: ago(9 * DAY - 2), exit_code: 0, error: null, requested_by: null, campaign_id: CAMPAIGNS[2]!.id,
  });
  return out;
}

const DEBIAN_PACKAGES: [string, string, string | null][] = [
  ["openssl", "3.0.14-1~deb12u2", "3.0.15-1~deb12u1"],
  ["libssl3", "3.0.14-1~deb12u2", "3.0.15-1~deb12u1"],
  ["curl", "7.88.1-10+deb12u7", "7.88.1-10+deb12u8"],
  ["libcurl4", "7.88.1-10+deb12u7", "7.88.1-10+deb12u8"],
  ["linux-image-amd64", "6.1.112-1", "6.1.115-1"],
  ["systemd", "252.30-1~deb12u2", null],
  ["openssh-server", "1:9.2p1-2+deb12u3", null],
  ["tzdata", "2024a-0+deb12u1", "2024b-0+deb12u1"],
  ["ca-certificates", "20230311", null],
  ["bash", "5.2.15-2+b7", null],
  ["coreutils", "9.1-1", null],
  ["python3", "3.11.2-1+b1", null],
  ["rsyslog", "8.2302.0-1", null],
  ["sudo", "1.9.13p3-1+deb12u1", null],
];

const RHEL_PACKAGES: [string, string, string | null][] = [
  ["openssl", "3.0.7-27.el9", "3.0.7-28.el9_4"],
  ["kernel", "5.14.0-427.13.1.el9_4", "5.14.0-427.37.1.el9_4"],
  ["curl", "7.76.1-29.el9_4", "7.76.1-29.el9_4.1"],
  ["systemd", "252-32.el9_4", null],
  ["openssh-server", "8.7p1-38.el9", null],
  ["bash", "5.1.8-9.el9", null],
  ["python3", "3.9.18-3.el9", null],
  ["sudo", "1.9.5p2-10.el9_3", null],
];

const WINDOWS_UPDATES: [string, string, string, string][] = [
  ["KB5990101", "Cumulative Update for Windows Server, x64 (KB5990101)", "Security Updates", "Critical"],
  ["KB5990102", "Servicing Stack Update for Windows Server, x64 (KB5990102)", "Updates", "Important"],
  ["KB5990103", "Update Rollup for Windows Server, x64 (KB5990103)", "Update Rollups", "Unspecified"],
];

export function packagesOf(server: DemoServer): Package[] {
  if (isWindows(server)) {
    const done = allServers.some((s) => s.server_id === server.id && s.status === "succeeded");
    return WINDOWS_UPDATES.slice(0, done ? 1 : 3).map(([name, title, classification, severity]) => ({
      name, arch: "x64", version: "", upgradable_version: "pending", title, classification, severity,
      size_bytes: 300_000_000, reboot_required: classification === "Security Updates",
    }));
  }
  const list = server.os_family === "rhel" ? RHEL_PACKAGES : DEBIAN_PACKAGES;
  const updated = allServers.some((s) => s.server_id === server.id && s.status === "succeeded");
  return list.map(([name, version, upgrade]) => ({
    name, arch: "amd64", version: updated && upgrade ? upgrade : version,
    upgradable_version: updated ? null : upgrade,
    title: null, classification: null, severity: null, size_bytes: null, reboot_required: false,
  }));
}

const LOG_MESSAGES: Record<string, [number, string, string][]> = {
  web: [[4, "nginx", "upstream timed out (110: Connection timed out) while reading response header from upstream"], [3, "php-fpm", "[pool www] server reached pm.max_children setting (20), consider raising it"]],
  db: [[4, "postgres", "checkpoints are occurring too frequently (24 seconds apart)"], [3, "postgres", "could not write to file \"pg_wal/xlogtemp.1834\": No space left on device"]],
  mail: [[4, "postfix/qmgr", "warning: mail queue is growing: 4182 messages"], [4, "dovecot", "imap-login: Disconnected: Too many invalid commands"]],
  files: [[3, "DFSR", "The DFS Replication service stopped replication on volume D:."], [4, "LanmanServer", "The server was unable to allocate from the system nonpaged pool because the pool was empty."]],
  dc: [[4, "NETLOGON", "This computer was not able to set up a secure session with a domain controller in domain CORP."], [4, "DNS", "The DNS server encountered a packet addressed to itself on IP address 192.0.2.31."]],
};

export function logsOf(server: DemoServer): LogLine[] {
  const messages = LOG_MESSAGES[server.role] ?? [[4, "systemd", "Started session cleanup."], [4, "sshd", "Connection closed by 203.0.113.45 port 51122 [preauth]"]];
  const lines: LogLine[] = [];
  for (let i = 0; i < 40; i += 1) {
    const [priority, unit, message] = messages[i % messages.length]!;
    lines.push({ time: ago(i * 17 + (i % 3)), message, priority, unit, identifier: unit, pid: 1000 + ((i * 37) % 900) });
  }
  return lines;
}

// Mirrors --------------------------------------------------------------------

const mirror = (n: number, name: string, family: "apt" | "dnf", upstream: string, distribution: string, components: string[]): Mirror => ({
  id: demoId(16, n), name, family, upstream_url: upstream, distribution, components, architectures: family === "apt" ? ["amd64"] : ["x86_64"],
  sync_interval_s: 6 * 3600, auto_snapshot: true, mode: "on_demand", created_at: ago(120 * DAY), deleting_at: null, delete_error: null,
  current_snapshot: `${name}-${new Date(START - 3 * 3600_000).toISOString().slice(0, 10).replaceAll("-", "")}`,
  last_run: {
    id: demoId(17, n), kind: "sync", trigger: "schedule", requested_by: null, state: "succeeded", snapshot_id: demoId(18, n),
    error: null, started_at: ago(3 * HOUR + n), finished_at: ago(3 * HOUR - 4 + n),
  },
});

export const MIRRORS: Mirror[] = [
  mirror(1, "debian-bookworm", "apt", "https://deb.example.com/debian", "bookworm", ["main", "contrib"]),
  mirror(2, "debian-bookworm-security", "apt", "https://security.example.com/debian-security", "bookworm-security", ["main"]),
  mirror(3, "ubuntu-noble", "apt", "https://ubuntu.example.com/ubuntu", "noble", ["main", "universe"]),
  mirror(4, "rocky-9-baseos", "dnf", "https://rocky.example.com/9/BaseOS/x86_64/os", "", []),
  mirror(5, "rocky-9-appstream", "dnf", "https://rocky.example.com/9/AppStream/x86_64/os", "", []),
];

export function mirrorDetail(m: Mirror): MirrorDetail {
  return {
    ...m,
    keep_snapshots: 5,
    snapshots: [0, 1, 2].map((d) => ({
      id: demoId(18, MIRRORS.indexOf(m) * 10 + d + 1), mirror_id: m.id,
      name: `${m.name}-${new Date(START - (d * 24 + 3) * 3600_000).toISOString().slice(0, 10).replaceAll("-", "")}`,
      state: "published", size_bytes: 1_400_000_000 - d * 20_000_000, error: null,
      created_at: ago(d * DAY + 3 * HOUR), published_at: ago(d * DAY + 3 * HOUR - 4), kept: d === 0 ? "current" : "newest",
    })),
    runs: m.last_run ? [{ ...m.last_run, mirror_id: m.id, output: "Downloaded 14 index files, 0 packages (on demand).\nPublished snapshot.\n" }] : [],
    used_by: { campaigns: [], servers: [] },
  };
}

// Notifications, on-call, follow-up ----------------------------------------------

const channelBase = {
  has_secret: true, customer_id: null, customer_name: null, site_id: null, site_name: null,
  last_error: null, last_error_at: null, follow_up_only: false,
};

export const CHANNELS: NotificationChannel[] = [
  { ...channelBase, id: demoId(19, 1), kind: "email", settings: { host: "smtp.example.com", port: 587, username: "monitorflow@example.com", from: "", to: ["ops@example.com"] }, created_at: ago(200 * DAY), last_sent_at: ago(36) },
  { ...channelBase, id: demoId(19, 2), kind: "webhook", settings: { origin: "https://hooks.example.com" }, created_at: ago(150 * DAY), last_sent_at: ago(36) },
  {
    ...channelBase, id: demoId(19, 3), kind: "email", site_id: BRANCH.id, site_name: BRANCH.name, customer_id: null,
    settings: { host: "smtp.example.com", port: 587, username: "monitorflow@example.com", from: "", to: ["branch-it@example.com"] },
    created_at: ago(90 * DAY), last_sent_at: ago(6 * DAY),
  },
];

const ROTATION = ["alex", "sam", "robin"];

function onCallTurns(): OnCallTurn[] {
  // Weekly hand-offs on Monday at 09:00, in the viewer's time zone.
  const start = new Date(START);
  start.setHours(9, 0, 0, 0);
  start.setDate(start.getDate() - ((start.getDay() + 6) % 7));
  if (start.getTime() > START) start.setDate(start.getDate() - 7);
  const turns: OnCallTurn[] = [];
  for (let w = 0; w < 5; w += 1) {
    const from = new Date(start.getTime() + w * 7 * 86_400_000);
    const to = new Date(from.getTime() + 7 * 86_400_000);
    const name = ROTATION[(w + 1) % ROTATION.length]!;
    turns.push({ starts_at: from.toISOString(), ends_at: to.toISOString(), user_id: user(name).id, username: name, override_id: null, problem: null });
  }
  return turns;
}

export function onCallSchedule(): OnCallSchedule {
  const turns = onCallTurns();
  return {
    id: demoId(20, 1), name: "Infrastructure", customer_id: CUSTOMER.id, customer_name: CUSTOMER.name, site_id: null, site_name: null,
    time_zone: Intl.DateTimeFormat().resolvedOptions().timeZone || "UTC", rotation: "weekly", handoff_time: "09:00", handoff_day: 1,
    members: ROTATION.map((name) => ({ user_id: user(name).id, username: name, problem: null })),
    overrides: [], now: turns[0]!, next: turns[1]!, turns, fallback: ["ops@example.com"], mail_host: "smtp.example.com", can_manage: true,
  };
}

export function followUp(): FollowUpPolicyList {
  const schedule = onCallSchedule();
  return {
    policies: [],
    default_steps: [
      { after_minutes: 0, on_call: [{ id: schedule.id, name: schedule.name }], users: [], channels: [{ id: CHANNELS[0]!.id, name: "ops@example.com" }], administrators: false },
      { after_minutes: 15, on_call: [], users: [{ id: user("alex").id, name: "alex" }], channels: [], administrators: false },
      { after_minutes: 30, on_call: [], users: [], channels: [], administrators: true },
    ],
    schedules: [{ id: schedule.id, name: schedule.name, customer_id: CUSTOMER.id, site_id: null }],
    channels: CHANNELS.map((c) => ({ id: c.id, name: c.kind === "email" ? c.settings.to.join(", ") : c.kind === "webhook" ? c.settings.origin : c.kind, customer_id: c.customer_id, site_id: c.site_id })),
  };
}

export const INTEGRATIONS: IntegrationList = {
  endpoints: [
    {
      id: demoId(21, 1), name: "Ticketing", origin: "https://tickets.example.com", events: ["incident.opened", "incident.resolved"],
      customer_id: null, customer_name: null, site_id: null, site_name: null, allow_private: false, state: "on",
      created_at: ago(60 * DAY), failing_since: null, last_success_at: ago(36), pending: 0, last_error: null,
    },
  ],
  events: [
    "incident.opened", "incident.escalated", "incident.acknowledged", "incident.resolved",
    "server.added", "server.removed", "server.offline", "server.online",
    "update.started", "update.succeeded", "update.failed",
    "campaign.started", "campaign.succeeded", "campaign.failed",
    "approval.waiting", "approval.decided",
  ],
};

export const API_KEYS: ApiKey[] = [
  {
    id: demoId(22, 1), name: "Inventory export", prefix: "q7Zt2", role: "viewer", customer_id: null, customer_name: null, site_id: null, site_name: null,
    areas: ["servers"], created_by: user("alex").id, created_by_username: "alex", created_at: ago(45 * DAY), expires_at: later(320 * DAY), last_used_at: ago(2 * HOUR),
  },
];

// Dashboard ------------------------------------------------------------------

const WEB_IDS = SERVERS.filter((s) => s.role === "web" && s.site_id === DATACENTER.id).map((s) => s.id);
const DB_IDS = SERVERS.filter((s) => s.role === "db" && !isWindows(s)).map((s) => s.id);

export const DASHBOARD: Dashboard = {
  id: demoId(23, 1),
  name: "Operations overview",
  shared_with: { kind: "customer", id: CUSTOMER.id, name: CUSTOMER.name },
  panels: 6,
  updated_at: ago(5 * DAY),
  updated_by_username: "alex",
  can_edit: true,
  can_link: true,
  revision: 7,
  spec: {
    version: 1,
    range: "24h",
    panels: [
      { id: "p1", title: "Open alerts", kind: "alerts", width: 2, servers: { kind: "all" } },
      { id: "p2", title: "Average CPU", kind: "figure", width: 1, servers: { kind: "all" }, metric: "monitorflow_cpu_usage_ratio", instance: "total", combine: "avg" },
      { id: "p3", title: "CPU of the web servers", kind: "chart", width: 2, servers: { kind: "list", ids: WEB_IDS }, metric: "monitorflow_cpu_usage_ratio", instance: "total", combine: "each" },
      { id: "p4", title: "Fullest disks", kind: "top", width: 1, servers: { kind: "all" }, metric: "monitorflow_disk_used_ratio", order: "highest", count: 5 },
      { id: "p5", title: "Database disk used", kind: "chart", width: 2, servers: { kind: "list", ids: DB_IDS }, metric: "monitorflow_disk_used_ratio", instance: "/var/lib/postgresql", combine: "each" },
      { id: "p6", title: "Least memory available", kind: "top", width: 1, servers: { kind: "all" }, metric: "monitorflow_memory_available_ratio", order: "lowest", count: 5 },
    ],
  },
};
