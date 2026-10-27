// Typed client for the MonitorFlow REST API (crates/server/src/api/mod.rs).
//
// Authentication is the server's HttpOnly session cookie only: this module
// never sees or stores a token. Requests go to the page's own origin, which
// the reverse proxy (or `next dev` in development) forwards to the API.

/** Header the API requires on every state-changing request (ADR-0006). */
export const CSRF_HEADER = "X-MonitorFlow-CSRF";

/** Where the client sends the browser when the session is missing or dead. */
export const LOGIN_PATH = "/login/";

export type Role = "viewer" | "operator" | "approver" | "administrator";

/**
 * A role granted on one site (`site_id`), on one customer and all its sites
 * (`customer_id`), or on everything when both are null (ADR-0026).
 */
export interface RoleGrant {
  role: Role;
  customer_id?: string | null;
  site_id: string | null;
}

/** `GET /api/auth/me` and the body of a successful login. */
export interface Me {
  id: string;
  username: string;
  roles: RoleGrant[];
}

export type ServerStatus = "online" | "offline";

/** One entry of `GET /api/servers`. Timestamps are RFC 3339 in UTC. */
export interface Server {
  id: string;
  /** The agent certificate ID. */
  peer_id: string;
  hostname: string;
  site_id: string;
  site_name: string;
  customer_id: string;
  customer_name: string;
  relay_id: string | null;
  /** `debian` or `rhel`. */
  os_family: string;
  os_version: string | null;
  agent_version: string | null;
  created_at: string;
  revoked_at: string | null;
  status: ServerStatus;
  /** Start of the current stream, when online. */
  connected_at: string | null;
  /** Last message from the agent, live or as last stored. */
  last_heartbeat: string | null;
  /** Peer ID of the relay carrying the live stream, if any. */
  via_relay: string | null;
  /** Open incidents of the server (ADR-0019). */
  open_incidents: number;
  /** Latest expiry among the agent's unrevoked certificates; null when it has none. */
  cert_not_after: string | null;
}

/** The certificate the CA issued last to an agent, revoked or not. */
export interface Certificate {
  /** Lowercase hexadecimal. */
  serial: string;
  not_before: string;
  /** Expiry. */
  not_after: string;
  issued_at: string;
  revoked: boolean;
  revoked_at: string | null;
  revocation_reason: string | null;
}

/** `GET /api/servers/{id}`: the listing row plus the agent's current certificate. */
export interface ServerDetail extends Server {
  /** Null when the CA never issued one, like a server row created by hand. */
  certificate: Certificate | null;
}

/** One page of `GET /api/servers`, sorted by site name, hostname and peer ID. */
export interface ServerList {
  servers: Server[];
  /** Pass as `cursor` for the next page; null on the last one. */
  next_cursor: string | null;
}

/** One metric of `GET /api/servers/{id}/metrics`, with the instances seen. */
export interface MetricInfo {
  /** Full name, like `monitorflow_cpu_usage_ratio`. */
  name: string;
  instances: string[];
}

/** Metrics a server wrote in the last day (ADR-0018). */
export interface MetricList {
  metrics: MetricInfo[];
  /** The backend's series limit was hit; some metrics may be missing. */
  truncated: boolean;
}

/** Parameters of `GET /api/metrics/query_range`. */
export interface RangeQuery {
  serverId: string;
  metric: string;
  instance?: string;
  start: Date;
  end: Date;
  /** Seconds between points. */
  step: number;
  agg?: "avg" | "min" | "max" | "sum" | "count";
  /** Summarizes each series over a window of one step before `agg` (ADR-0018). */
  rollup?: Rollup;
}

/** How each point of a range summarizes the samples of its step. */
export type Rollup = "avg" | "max" | "min" | "last";

/** One series: its labels and `[unix seconds, value]` points; null is not a number. */
export interface MetricSeries {
  labels: Record<string, string>;
  points: [number, number | null][];
}

export interface QueryResult {
  /** The PromQL the server built, for display. */
  query: string;
  series: MetricSeries[];
}

/** Which layer an effective sensor comes from (ADR-0016). */
export type SensorOrigin = "default" | "site" | "server";

/** One sensor as the agent receives it. */
export interface EffectiveSensor {
  origin: SensorOrigin;
  /** The `sensor_configs` row; null for a built-in default. */
  entry_id: string | null;
  kind: string;
  /** Metric name prefix, like `disk_data`. */
  name: string;
  instance: string;
  interval_ms: number;
  /** Null: the agent's default. */
  timeout_ms: number | null;
  params: Record<string, string>;
}

/** `GET /api/servers/{id}/sensors`: the effective configuration and its drift. */
export interface EffectiveSensors {
  server_id: string;
  site_id: string;
  /** Revision of the current configuration. */
  revision: number;
  /** Revision the agent last acknowledged; null if it never did. */
  applied_revision: number | null;
  applied_at: string | null;
  in_sync: boolean;
  sensors: EffectiveSensor[];
}

/** How a value is compared with a threshold: the alarm is `value <comparison> threshold`. */
export type Comparison = ">" | ">=" | "<" | "<=";

/** Which layer an effective threshold comes from (ADR-0017). */
export type ThresholdOrigin = "default" | "site" | "server";

/** One threshold as the engine applies it, or a muted one (ADR-0019). */
export interface EffectiveThreshold {
  origin: ThresholdOrigin;
  /** The `thresholds` row; null for a built-in default. */
  threshold_id: string | null;
  metric: string;
  /** Null covers every instance; `""` is the series without one. */
  instance: string | null;
  /** False: muted by a disabled row, which the engine skips. */
  enabled: boolean;
  comparison: Comparison;
  warning: number | null;
  critical: number | null;
  duration_s: number;
  hysteresis: number;
}

/** `GET /api/servers/{id}/thresholds/effective`. */
export interface EffectiveThresholds {
  server_id: string;
  site_id: string;
  revision: number;
  thresholds: EffectiveThreshold[];
}

/** One entry of `GET /api/sites`. */
export interface Site {
  id: string;
  name: string;
  server_count: number;
  /** Enrolled relays, revoked ones included. */
  relay_count: number;
  customer_id: string;
  customer_name: string;
  /** Agents are updated to the newest release the server holds (ADR-0053). */
  agent_auto_update: boolean;
}

/** One entry of `GET /api/customers`. */
export interface Customer {
  id: string;
  name: string;
  /** Sites of the customer the user reads. */
  site_count: number;
}

export interface CustomerList {
  customers: Customer[];
}

/** One relay of `GET /api/relays` (ADR-0029). */
export interface Relay {
  id: string;
  /** The relay certificate ID. */
  peer_id: string;
  name: string;
  site_id: string;
  site_name: string;
  customer_id: string;
  customer_name: string;
  created_at: string;
  revoked_at: string | null;
  /** Online while the relay's own stream is open. */
  status: ServerStatus;
  connected_at: string | null;
  last_heartbeat: string | null;
  /** Latest expiry among unrevoked certificates; null when none. */
  cert_not_after: string | null;
  /** Names agents use to reach the relay, from its certificate. */
  server_names: string[];
  /** Servers whose latest connection came through the relay. */
  server_count: number;
  /** Agent streams it forwards right now. */
  connected_agents: number;
}

/** A server attached to a relay. */
export interface AttachedServer {
  id: string;
  peer_id: string;
  hostname: string;
  /** Online while its stream goes through this relay. */
  status: ServerStatus;
}

/** `GET /api/relays/{id}`. */
export interface RelayDetail extends Relay {
  certificate: Certificate | null;
  servers: AttachedServer[];
}

export interface RelayList {
  relays: Relay[];
  next_cursor: string | null;
}

/** RFC 5280 reasons the CA records. */
export type RevocationReason =
  | "unspecified"
  | "key_compromise"
  | "affiliation_changed"
  | "superseded"
  | "cessation_of_operation";

/** Answer of the revoke routes. */
export interface Revoked {
  revoked_at: string;
  reason: RevocationReason;
  serials: string[];
  closed_streams: number;
}

export interface SiteList {
  sites: Site[];
}

export type IncidentSeverity = "warning" | "critical";
export type IncidentState = "open" | "resolved";

/** The threshold an incident opened under, as it was then (ADR-0017). */
export interface ThresholdSnapshot {
  metric: string;
  instance: string | null;
  comparison: Comparison;
  warning: number | null;
  critical: number | null;
  duration_s: number;
  hysteresis: number;
}

/** One incident of `GET /api/incidents`. Times are sample times. */
export interface Incident {
  id: string;
  server_id: string;
  hostname: string;
  site_id: string;
  site_name: string;
  metric: string;
  /** `""` for a series without an instance. */
  instance: string;
  /** The current one, or the last one once resolved. */
  severity: IncidentSeverity;
  state: IncidentState;
  opened_at: string;
  resolved_at: string | null;
  last_value: number;
  last_value_at: string;
  /** The worst value seen while open. */
  peak_value: number;
  threshold: ThresholdSnapshot;
}

/** One page of incidents: open ones first, then newest first. */
export interface IncidentList {
  incidents: Incident[];
  next_cursor: string | null;
}

export type IncidentEventKind = "opened" | "escalated" | "deescalated" | "resolved";

/** One change of an incident, as the engine made it. */
export interface IncidentEvent {
  id: number;
  kind: IncidentEventKind;
  /** Severity after the change; null once resolved. */
  severity: IncidentSeverity | null;
  value: number | null;
  /** Sample time, or the server clock without a sample. */
  occurred_at: string;
  recorded_at: string;
  /** For instance `{"reason": "threshold_removed"}`. */
  details: Record<string, unknown>;
}

/** `GET /api/incidents/{id}`: the incident and its events in the order they happened. */
export interface IncidentDetail extends Incident {
  events: IncidentEvent[];
}

/** Filters of `GET /api/incidents`; all optional. */
export interface IncidentQuery {
  state?: IncidentState;
  severity?: IncidentSeverity;
  server_id?: string;
  site_id?: string;
  /** 1 to 500; the API's default is 100. */
  limit?: number;
  cursor?: string;
}

export type CommandState = "queued" | "sent" | "running" | "succeeded" | "failed" | "expired" | "cancelled";

/** One command of `GET /api/servers/{id}/commands`, without its output (ADR-0031). */
export interface Command {
  id: string;
  server_id: string;
  /** `apply_updates`, `configure_sources`, `revert_sources`, `shell` or `self_update`. */
  kind: string;
  /** The payload as JSON for kinds the API knows, else null. */
  parameters: Record<string, unknown> | null;
  state: CommandState;
  issued_at: string;
  expires_at: string;
  sent_at: string | null;
  started_at: string | null;
  finished_at: string | null;
  exit_code: number | null;
  error: string | null;
  /** The user who queued it; null for the system. */
  requested_by: string | null;
  campaign_id: string | null;
}

/** `GET /api/commands/{id}`: the command and the last 64 KiB of each output. */
export interface CommandDetail extends Command {
  stdout: string | null;
  stderr: string | null;
}

/** `GET /api/agent-releases`: agent versions the server holds for self-updates (ADR-0053). */
export interface AgentReleases {
  /** Newest first. */
  versions: string[];
  /** The newest without a pre-release suffix, else the newest; null when none. */
  latest: string | null;
}

/** Steps of a server update (ADR-0056); the last three are final. */
export type ServerUpdateState =
  | "verifying"
  | "downloading"
  | "backing_up"
  | "updating"
  | "checking"
  | "caching_agents"
  | "rolling_back"
  | "succeeded"
  | "failed"
  | "rolled_back";

/** The last server update, as the updater container reports it. */
export interface ServerUpdateRun {
  id: string;
  from: string | null;
  to: string;
  requested_by: string;
  state: ServerUpdateState;
  /** Oldest first. */
  steps: { at: string; message: string }[];
  error: string | null;
  /** File name of the database dump in the backups volume. */
  backup: string | null;
  /** The new version migrated the database: a rollback needs the backup restored. */
  migrated: boolean;
  started_at: string;
  finished_at: string | null;
}

/** `GET /api/server-update`. */
export interface ServerUpdateView {
  running_version: string;
  /** The server is set up for updates from the web interface. */
  enabled: boolean;
  source: string;
  /** Null until the updater has reported once. */
  updater: {
    version: string;
    heartbeat_at: string;
    alive: boolean;
    /** Images pinned in `.env`; null is a local build. */
    running: {
      version: string;
      server_image: string | null;
      web_image: string | null;
      updater_image: string | null;
    } | null;
  } | null;
  update: ServerUpdateRun | null;
  /** A request waits for the updater. */
  requested: boolean;
}

/** `POST /api/server-update/check`. */
export interface ServerUpdateCheck {
  source: string;
  running_version: string;
  /** False when the source could not be reached, as on an offline server. */
  reachable: boolean;
  error: string | null;
  latest: {
    version: string;
    newer: boolean;
    release_notes_url: string | null;
    images: { server: string; web: string; updater: string };
    web_changes: boolean;
    updater_changes: boolean;
    agent_release_cached: boolean;
  } | null;
}

/** One page of a server's commands, newest first. */
export interface CommandList {
  commands: Command[];
  next_cursor: string | null;
}

export type RemediationState = "open" | "running" | "awaiting_approval" | "resolved" | "escalated" | "failed";

/** One incident of the AI remediation loop (ADR-0042), read-only. */
export interface RemediationIncident {
  id: string;
  server_id: string;
  hostname: string;
  /** The threshold incident that opened it. */
  alert_incident_id: string | null;
  metric: string;
  instance: string;
  state: RemediationState;
  opened_at: string;
  closed_at: string | null;
  max_commands: number;
  max_duration_s: number;
  event_count: number;
}

export interface RemediationIncidentList {
  incidents: RemediationIncident[];
  next_cursor: string | null;
}

export type RemediationEventKind =
  | "context_sent"
  | "ai_proposal"
  | "classification"
  | "approval"
  | "rejection"
  | "edit"
  | "command_queued"
  | "command_result"
  | "snapshot"
  | "escalation"
  | "state_change"
  | "note";

/** `user`, `system`, `ai` (the model) or `peer` (the agent), as in the audit log. */
export type ActorKind = "user" | "system" | "ai" | "peer";

/** One event of an incident's hash-chained history (ADR-0042). */
export interface RemediationIncidentEvent {
  seq: number;
  kind: RemediationEventKind;
  actor_kind: ActorKind;
  actor_id: string;
  /** Fields depend on `kind`; AI text in it is untrusted. */
  payload: Record<string, unknown>;
  created_at: string;
  prev_hash: string;
  hash: string;
}

/** The hash chain of an incident, recomputed on each request. */
export interface ChainReport {
  valid: boolean;
  events: number;
  broken_at: number | null;
  reason: "gap" | "prev_hash" | "hash" | "head" | null;
  head_hash: string | null;
}

/** `GET /api/remediation/incidents/{id}`. */
export interface RemediationIncidentDetail extends RemediationIncident {
  site_name: string;
  customer_name: string;
  events: RemediationIncidentEvent[];
  chain: ChainReport;
}

/** One event of `GET /api/remediation/events`, with its incident's server (ADR-0045). */
export interface RemediationEvent {
  incident_id: string;
  seq: number;
  kind: RemediationEventKind;
  actor_kind: ActorKind;
  actor_id: string;
  /** The user's name for user actors. */
  actor_name: string | null;
  /** Without the rendered context and command output; the incident detail has them. */
  payload: Record<string, unknown>;
  created_at: string;
  hash: string;
  customer_id: string;
  customer_name: string;
  site_id: string;
  site_name: string;
  server_id: string;
  hostname: string;
  metric: string;
  instance: string;
}

export interface RemediationEventList {
  events: RemediationEvent[];
  next_cursor: string | null;
}

export interface RemediationEventQuery {
  customer_id?: string;
  site_id?: string;
  incident_id?: string;
  server_id?: string;
  kind?: RemediationEventKind;
  actor_kind?: ActorKind;
  actor_id?: string;
  from?: string;
  to?: string;
  limit?: number;
}

export type RiskTier = "read_only" | "low_impact" | "high_impact" | "forbidden";

/** Why a server cannot be snapshotted before a change (ADR-0044). */
export type SnapshotUnsupported = "disabled" | "not_mapped" | "unknown_cluster";

/** One command waiting for approval (ADR-0045). */
export interface PendingApproval {
  id: string;
  incident_id: string;
  customer_id: string;
  customer_name: string;
  site_id: string;
  site_name: string;
  server_id: string;
  hostname: string;
  alert_incident_id: string | null;
  metric: string;
  instance: string;
  command: string;
  /** The AI's text: untrusted, shown as data. */
  diagnosis: string;
  reasoning: string;
  tier: RiskTier;
  rule: { source: "builtin" | "configured"; id: string };
  reason: string;
  commands_run: number;
  max_commands: number;
  opened_at: string;
  proposed_at: string;
  /** The incident escalates then; a decision after it is refused. */
  deadline: string;
  /** What the user's roles allow; the API checks again. */
  can_approve: boolean;
  can_reject: boolean;
  snapshots: { available: boolean; reason: SnapshotUnsupported | null };
}

export interface PendingApprovalList {
  approvals: PendingApproval[];
}

/** Answer of approve and reject. */
export interface ApprovalDecision {
  id: string;
  incident_id: string;
  state: "approved" | "rejected";
  command: string;
  edited: boolean;
}

export type TokenRole = "agent" | "relay";
export type TokenState = "pending" | "used" | "expired";

/** One enrollment token of `GET /api/tokens`, never with its secret. */
export interface EnrollmentToken {
  id: string;
  role: TokenRole;
  peer_id: string;
  site_id: string;
  site_name: string;
  customer_id: string;
  customer_name: string;
  server_names: string[];
  /** Null for tokens made with the server CLI. */
  created_by: string | null;
  created_by_username: string | null;
  created_at: string;
  expires_at: string;
  used_at: string | null;
  state: TokenState;
}

export interface TokenList {
  tokens: EnrollmentToken[];
  next_cursor: string | null;
}

/** Body of `POST /api/tokens`. */
export interface TokenRequest {
  role: TokenRole;
  peer_id: string;
  site_id: string;
  /** 60 to 604800 seconds. */
  ttl_s: number;
  /** Relays only: names agents connect to. */
  server_names: string[];
}

/** `POST /api/tokens`: the only answer that carries the token itself. */
export interface CreatedToken extends EnrollmentToken {
  token: string;
  upstream_url: string;
  enroll_url: string;
  /** Where the install commands may download the agent from (ADR-0054). */
  install: AgentInstall;
}

/** Where install commands download the agent from (ADR-0054). */
export type InstallSource = "github" | "central" | "custom";

/** The newest release the server holds, as `central` commands pin it; checksums in hex. */
export interface CentralRelease {
  version: string;
  /** URL path under the web interface, like `/agent/0.3.0`. */
  path: string;
  sums_sha256: string;
  /** `null` when the cached release lacks install.sh, or the MSI. */
  installer_sha256: string | null;
  msi_sha256: string | null;
}

/** `GET /api/settings/agent-install`, and the `install` of a new token. */
export interface AgentInstall {
  /** The server-wide default. */
  source: InstallSource;
  custom_url: string | null;
  /** Set by the server's configuration, so not editable here. */
  source_from_config: boolean;
  custom_url_from_config: boolean;
  github_url: string;
  /** `null` when the server holds no release. */
  central: CentralRelease | null;
}

export type CampaignState = "draft" | "scheduled" | "running" | "paused" | "stopped" | "completed" | "failed";
export type CampaignAction = "start" | "pause" | "resume" | "stop";

/** A wave fails when more of its servers failed than `value` servers or percent (ADR-0033). */
export interface FailureThreshold {
  value: number;
  unit: "count" | "percent";
}

/** A maintenance window; with none, a campaign may queue at any time. */
export type CampaignWindow =
  | {
      kind: "weekly";
      /** ISO: 1 is Monday, 7 Sunday. */
      weekdays: number[];
      /** Local `HH:MM`; an end before the start closes the next day. */
      start: string;
      end: string;
      /** IANA name, like `Europe/Paris`. */
      timezone: string;
    }
  | { kind: "once"; starts_at: string; ends_at: string };

/** Explicit servers, a number of servers, or a percentage of all targets (rounded up). */
export type WaveSpec = { servers: string[] } | { size: number } | { percent: number };

/** The `ApplyUpdates` every command of a campaign carries. */
export interface UpdateSpec {
  /** Empty upgrades everything. */
  packages: string[];
  security_only: boolean;
  allow_reboot: boolean;
}

/** Published mirror snapshots a campaign installs from (ADR-0038). */
export interface SourcesPin {
  /** `replace` sets the host's other sources aside, `add` keeps them. */
  mode: "replace" | "add";
  mirrors: { name: string; snapshot: string }[];
}

/** Servers by status: `failed` includes expired and skipped, `in_progress` queued, sent and running. */
export interface CampaignProgress {
  total: number;
  pending: number;
  in_progress: number;
  succeeded: number;
  failed: number;
  cancelled: number;
}

/** One campaign of `GET /api/campaigns`. */
export interface Campaign {
  id: string;
  customer_id: string;
  customer_name: string;
  name: string;
  site_id: string | null;
  state: CampaignState;
  /** Why it ended, for final states. */
  error: string | null;
  spec: UpdateSpec;
  command_ttl_s: number;
  failure_threshold: FailureThreshold;
  windows: CampaignWindow[];
  /** The pinned snapshots, if any. */
  sources: SourcesPin | null;
  /** Set for an agent update campaign: the version agents update to (ADR-0053). */
  agent_version: string | null;
  wave_count: number;
  /** Index from 0; null until the first wave is queued. */
  current_wave: number | null;
  progress: CampaignProgress;
  created_by: string | null;
  created_at: string;
  started_at: string | null;
  finished_at: string | null;
}

/** `pending` before any command, a command state, or `skipped` with an `error`. */
export type CampaignServerStatus =
  | "pending"
  | "queued"
  | "sent"
  | "running"
  | "succeeded"
  | "failed"
  | "expired"
  | "cancelled"
  | "skipped";

export interface CampaignServer {
  server_id: string;
  peer_id: string;
  hostname: string;
  site_id: string;
  status: CampaignServerStatus;
  command_id: string | null;
  exit_code: number | null;
  error: string | null;
  finished_at: string | null;
  /** With pinned snapshots: the `configure_sources` before the update and the restore after it. */
  sources_command_id: string | null;
  restore_command_id: string | null;
}

export interface CampaignWave {
  index: number;
  state: "pending" | "running" | "done";
  started_at: string | null;
  finished_at: string | null;
  progress: CampaignProgress;
  servers: CampaignServer[];
}

/** `GET /api/campaigns/{id}` and the answer of every write. */
export interface CampaignDetail extends Campaign {
  waves: CampaignWave[];
}

/** One page of campaigns, newest first. */
export interface CampaignList {
  campaigns: Campaign[];
  next_cursor: string | null;
}

/** Body of `POST /api/campaigns`; a `site_id` or `server_ids` is required. */
export interface CampaignRequest {
  customer_id: string;
  name: string;
  site_id?: string;
  server_ids: string[];
  waves: WaveSpec[];
  packages: string[];
  security_only: boolean;
  allow_reboot: boolean;
  /** 60 to 604800 seconds. */
  command_ttl_s: number;
  failure_threshold: FailureThreshold;
  windows: CampaignWindow[];
  /** An agent update campaign instead of packages (ADR-0053). */
  agent_version?: string;
}

/** A package of a server's latest report (ADR-0031). */
export interface Package {
  name: string;
  arch: string;
  version: string;
  /** Null when no upgrade is available. */
  upgradable_version: string | null;
  /** Windows updates only (ADR-0049): null for Linux packages. */
  title: string | null;
  classification: string | null;
  severity: string | null;
  size_bytes: number | null;
  /** A Windows update that may need a restart, or waits for one. */
  reboot_required: boolean;
}

/** `GET /api/servers/{id}/packages`: the latest report, by name and architecture. */
export interface PackageList {
  /** Null until the agent sent a report. */
  reboot_required: boolean | null;
  /** When the agent collected the report, by its clock. */
  reported_at: string | null;
  packages: Package[];
  next_cursor: string | null;
}

export type MirrorRunState = "running" | "succeeded" | "failed";

/** A sync or snapshot run of a mirror, without its output (ADR-0030). */
export interface MirrorRunSummary {
  id: string;
  kind: "sync" | "snapshot";
  trigger: "schedule" | "manual";
  /** `user:<id>` for manual runs. */
  requested_by: string | null;
  state: MirrorRunState;
  snapshot_id: string | null;
  error: string | null;
  started_at: string;
  finished_at: string | null;
}

/** A run with the tools' output. */
export interface MirrorRun extends MirrorRunSummary {
  mirror_id: string;
  output: string;
}

/** One mirror of `GET /api/mirrors`; mirrors are global. */
export interface Mirror {
  id: string;
  name: string;
  family: "apt" | "dnf";
  upstream_url: string;
  /** apt suite; empty for dnf. */
  distribution: string;
  components: string[];
  /** Empty: every architecture of the upstream. */
  architectures: string[];
  /** Null: synced only on request. */
  sync_interval_s: number | null;
  auto_snapshot: boolean;
  created_at: string;
  /** Name of the newest published snapshot. */
  current_snapshot: string | null;
  last_run: MirrorRunSummary | null;
}

export interface MirrorList {
  mirrors: Mirror[];
}

export interface MirrorSnapshot {
  id: string;
  mirror_id: string;
  name: string;
  state: "creating" | "published" | "failed";
  size_bytes: number | null;
  error: string | null;
  created_at: string;
  published_at: string | null;
}

/** `GET /api/mirrors/{id}`: the newest 100 snapshots and 10 runs first. */
export interface MirrorDetail extends Mirror {
  snapshots: MirrorSnapshot[];
  runs: MirrorRun[];
}

export interface Health {
  status: "ok";
}

/**
 * A failed request. `status` is the HTTP status, or 0 when the server could
 * not be reached. `message` is the API's generic `{"error"}` text.
 */
export class ApiError extends Error {
  readonly status: number;

  constructor(status: number, message: string, options?: ErrorOptions) {
    super(message, options);
    this.name = "ApiError";
    this.status = status;
  }
}

let unauthorizedHandler: () => void = () => {
  // Fallback outside the authenticated layout, which installs a router-based
  // handler instead.
  // eslint-disable-next-line @next/next/no-location-assign-relative-destination
  window.location.assign(LOGIN_PATH);
};

/**
 * Replaces what happens on a 401, by default a full navigation to the login
 * page. Returns a function that restores the previous handler.
 */
export function setUnauthorizedHandler(handler: () => void): () => void {
  const previous = unauthorizedHandler;
  unauthorizedHandler = handler;
  return () => {
    unauthorizedHandler = previous;
  };
}

interface RequestOptions {
  method?: "GET" | "POST" | "PUT" | "DELETE";
  body?: unknown;
  signal?: AbortSignal;
  /** Call the unauthorized handler on 401. Off for login, where 401 means bad credentials. */
  redirectOn401?: boolean;
}

async function request<T>(path: string, options: RequestOptions = {}): Promise<T> {
  const method = options.method ?? "GET";
  const headers: Record<string, string> = { Accept: "application/json" };
  if (method !== "GET") {
    headers[CSRF_HEADER] = "1";
  }
  if (options.body !== undefined) {
    headers["Content-Type"] = "application/json";
  }

  let response: Response;
  try {
    response = await fetch(path, {
      method,
      headers,
      body: options.body === undefined ? undefined : JSON.stringify(options.body),
      credentials: "same-origin",
      cache: "no-store",
      signal: options.signal,
    });
  } catch (cause) {
    if (isAbortError(cause)) {
      throw cause;
    }
    throw new ApiError(0, "network error", { cause });
  }

  if (!response.ok) {
    const message = await errorMessage(response);
    if (response.status === 401 && (options.redirectOn401 ?? true)) {
      unauthorizedHandler();
    }
    throw new ApiError(response.status, message);
  }
  if (response.status === 204) {
    return undefined as T;
  }
  return (await response.json()) as T;
}

async function errorMessage(response: Response): Promise<string> {
  try {
    const body: unknown = await response.json();
    if (body && typeof body === "object" && "error" in body && typeof body.error === "string") {
      return body.error;
    }
  } catch {
    // Not JSON, e.g. an error page from the reverse proxy.
  }
  return response.statusText || `HTTP ${response.status}`;
}

/** A 401: the unauthorized handler has already sent the user to the login page. */
export function isUnauthorized(error: unknown): boolean {
  return error instanceof ApiError && error.status === 401;
}

export function isAbortError(error: unknown): boolean {
  return error instanceof DOMException && error.name === "AbortError";
}

/** A short English sentence for showing `error` to the user. */
export function describeError(error: unknown): string {
  if (!(error instanceof ApiError)) {
    return "Something went wrong.";
  }
  switch (error.status) {
    case 0:
      return "Cannot reach the MonitorFlow server.";
    case 401:
      return "Your session has ended. Sign in again.";
    case 403:
      return "You do not have access to this. Ask an administrator for a role.";
    default:
      return error.status >= 500
        ? "The server could not handle the request. Try again later."
        : `The request failed (${error.message}).`;
  }
}

export const api = {
  health: (signal?: AbortSignal) => request<Health>("/api/health", { signal }),

  login: (username: string, password: string) =>
    request<Me>("/api/auth/login", {
      method: "POST",
      body: { username, password },
      redirectOn401: false,
    }),

  logout: () => request<void>("/api/auth/logout", { method: "POST" }),

  me: (signal?: AbortSignal) => request<Me>("/api/auth/me", { signal }),

  /** One page of servers, from `cursor` when given. */
  serversPage: (cursor?: string, signal?: AbortSignal) =>
    request<ServerList>(
      cursor === undefined ? "/api/servers" : `/api/servers?cursor=${encodeURIComponent(cursor)}`,
      { signal },
    ),

  /** Every server the user may read, following `next_cursor` across pages. */
  servers: async (signal?: AbortSignal): Promise<ServerList> => {
    const servers: Server[] = [];
    let cursor: string | undefined;
    do {
      const page = await api.serversPage(cursor, signal);
      servers.push(...page.servers);
      cursor = page.next_cursor ?? undefined;
    } while (cursor !== undefined);
    return { servers, next_cursor: null };
  },

  /** One server with its certificate; 404 when missing or outside the user's sites. */
  server: (id: string, signal?: AbortSignal) =>
    request<ServerDetail>(`/api/servers/${encodeURIComponent(id)}`, { signal }),

  serverSensors: (serverId: string, signal?: AbortSignal) =>
    request<EffectiveSensors>(`/api/servers/${encodeURIComponent(serverId)}/sensors`, { signal }),

  /** The server's whole package report, following `next_cursor` across pages. */
  serverPackages: async (serverId: string, signal?: AbortSignal): Promise<PackageList> => {
    const base = `/api/servers/${encodeURIComponent(serverId)}/packages?limit=5000`;
    const packages: Package[] = [];
    let cursor: string | undefined;
    let page: PackageList;
    do {
      page = await request<PackageList>(cursor === undefined ? base : `${base}&cursor=${encodeURIComponent(cursor)}`, {
        signal,
      });
      packages.push(...page.packages);
      cursor = page.next_cursor ?? undefined;
    } while (cursor !== undefined);
    return { ...page, packages, next_cursor: null };
  },

  sites: (signal?: AbortSignal) => request<SiteList>("/api/sites", { signal }),

  customers: (signal?: AbortSignal) => request<CustomerList>("/api/customers", { signal }),

  /** Turns automatic agent updates of the site on or off; customer administrators only (ADR-0053). */
  setAgentAutoUpdate: (siteId: string, enabled: boolean) =>
    request<Site>(`/api/sites/${encodeURIComponent(siteId)}/agent-auto-update`, {
      method: "PUT",
      body: { enabled },
    }),

  /** Creates the site `name` under `customerId`; customer administrators only. */
  createSite: (customerId: string, name: string) =>
    request<Site>(`/api/customers/${encodeURIComponent(customerId)}/sites`, {
      method: "POST",
      body: { name },
    }),

  /** Every relay the user may read, following `next_cursor` across pages. */
  relays: async (signal?: AbortSignal): Promise<RelayList> => {
    const relays: Relay[] = [];
    let cursor: string | undefined;
    do {
      const path = cursor === undefined ? "/api/relays" : `/api/relays?cursor=${encodeURIComponent(cursor)}`;
      const page = await request<RelayList>(path, { signal });
      relays.push(...page.relays);
      cursor = page.next_cursor ?? undefined;
    } while (cursor !== undefined);
    return { relays, next_cursor: null };
  },

  relay: (id: string, signal?: AbortSignal) =>
    request<RelayDetail>(`/api/relays/${encodeURIComponent(id)}`, { signal }),

  /** The newest enrollment tokens of the customers the user administers. */
  tokens: (signal?: AbortSignal) => request<TokenList>("/api/tokens", { signal }),

  createToken: (body: TokenRequest) => request<CreatedToken>("/api/tokens", { method: "POST", body }),

  /** The default install source; customer administrators (ADR-0054). */
  agentInstall: (signal?: AbortSignal) => request<AgentInstall>("/api/settings/agent-install", { signal }),

  /** Changes the default install source; global administrators only, audited. */
  setAgentInstall: (body: { source: InstallSource; custom_url: string | null }) =>
    request<AgentInstall>("/api/settings/agent-install", { method: "PUT", body }),

  /** Revokes an unused token; 409 once used. */
  revokeToken: (id: string) => request<void>(`/api/tokens/${encodeURIComponent(id)}`, { method: "DELETE" }),

  /** Revokes a server or relay and its certificates; cannot be undone. */
  revoke: (kind: "servers" | "relays", id: string, reason: RevocationReason) =>
    request<Revoked>(`/api/${kind}/${encodeURIComponent(id)}/revoke`, {
      method: "POST",
      body: { reason },
    }),

  incidents: (query: IncidentQuery, signal?: AbortSignal) => {
    const params = new URLSearchParams();
    for (const [key, value] of Object.entries(query)) {
      if (value !== undefined) params.set(key, String(value));
    }
    const search = params.toString();
    return request<IncidentList>(search ? `/api/incidents?${search}` : "/api/incidents", { signal });
  },

  incident: (id: string, signal?: AbortSignal) =>
    request<IncidentDetail>(`/api/incidents/${encodeURIComponent(id)}`, { signal }),

  serverThresholds: (serverId: string, signal?: AbortSignal) =>
    request<EffectiveThresholds>(`/api/servers/${encodeURIComponent(serverId)}/thresholds/effective`, {
      signal,
    }),

  serverMetrics: (serverId: string, signal?: AbortSignal) =>
    request<MetricList>(`/api/servers/${encodeURIComponent(serverId)}/metrics`, { signal }),

  /** The newest 500 campaigns of the customers the user reads as a whole. */
  campaigns: (signal?: AbortSignal) => request<CampaignList>("/api/campaigns?limit=500", { signal }),

  campaign: (id: string, signal?: AbortSignal) =>
    request<CampaignDetail>(`/api/campaigns/${encodeURIComponent(id)}`, { signal }),

  /** Creates a draft campaign; customer administrators only. */
  createCampaign: (body: CampaignRequest) =>
    request<CampaignDetail>("/api/campaigns", { method: "POST", body }),

  /** 409 when the campaign's state does not allow `action`. */
  campaignAction: (id: string, action: CampaignAction) =>
    request<CampaignDetail>(`/api/campaigns/${encodeURIComponent(id)}/${action}`, { method: "POST" }),

  /** Every mirror, by name; global administrators only. */
  mirrors: (signal?: AbortSignal) => request<MirrorList>("/api/mirrors", { signal }),

  mirror: (id: string, signal?: AbortSignal) =>
    request<MirrorDetail>(`/api/mirrors/${encodeURIComponent(id)}`, { signal }),

  /** Starts a sync; 409 while another run of the mirror is going. */
  syncMirror: (id: string) =>
    request<MirrorRunSummary>(`/api/mirrors/${encodeURIComponent(id)}/sync`, { method: "POST" }),

  /** Starts a snapshot named after the time; 409 while busy or before the first sync. */
  snapshotMirror: (id: string) =>
    request<MirrorRunSummary>(`/api/mirrors/${encodeURIComponent(id)}/snapshots`, { method: "POST", body: {} }),

  /** One page of the server's commands, newest first. */
  serverCommands: (serverId: string, query: { limit?: number; cursor?: string } = {}, signal?: AbortSignal) => {
    const params = new URLSearchParams();
    if (query.limit !== undefined) params.set("limit", String(query.limit));
    if (query.cursor !== undefined) params.set("cursor", query.cursor);
    const search = params.toString();
    return request<CommandList>(
      `/api/servers/${encodeURIComponent(serverId)}/commands${search ? `?${search}` : ""}`,
      { signal },
    );
  },

  agentReleases: (signal?: AbortSignal) => request<AgentReleases>("/api/agent-releases", { signal }),

  /** Server updates; global administrators only (ADR-0056). */
  serverUpdate: (signal?: AbortSignal) => request<ServerUpdateView>("/api/server-update", { signal }),

  checkServerUpdate: () => request<ServerUpdateCheck>("/api/server-update/check", { method: "POST" }),

  startServerUpdate: (version: string) =>
    request<{ id: string }>("/api/server-update", { method: "POST", body: { version } }),

  /** Queues a `self_update` to `version`; customer administrators only (ADR-0053). */
  updateAgent: (serverId: string, version: string) =>
    request<CommandDetail>(`/api/servers/${encodeURIComponent(serverId)}/commands`, {
      method: "POST",
      body: { kind: "self_update", version },
    }),

  command: (id: string, signal?: AbortSignal) =>
    request<CommandDetail>(`/api/commands/${encodeURIComponent(id)}`, { signal }),

  /** The remediation incidents a threshold incident opened, newest first. */
  remediationIncidents: (alertIncidentId: string, signal?: AbortSignal) =>
    request<RemediationIncidentList>(
      `/api/remediation/incidents?alert_incident_id=${encodeURIComponent(alertIncidentId)}&limit=1`,
      { signal },
    ),

  /** Every command waiting for approval in the user's sites, nearest deadline first. */
  approvals: (signal?: AbortSignal) => request<PendingApprovalList>("/api/remediation/approvals", { signal }),

  /** Runs the pending command, or `command` instead when given; 409 once decided or past the deadline. */
  approve: (id: string, command?: string) =>
    request<ApprovalDecision>(`/api/remediation/approvals/${encodeURIComponent(id)}/approve`, {
      method: "POST",
      body: command === undefined ? {} : { command },
    }),

  /** Drops the pending command; the reason goes back to the AI. */
  reject: (id: string, reason: string) =>
    request<ApprovalDecision>(`/api/remediation/approvals/${encodeURIComponent(id)}/reject`, {
      method: "POST",
      body: { reason },
    }),

  /** One remediation incident with its events and verified chain. */
  remediationIncident: (id: string, signal?: AbortSignal) =>
    request<RemediationIncidentDetail>(`/api/remediation/incidents/${encodeURIComponent(id)}`, { signal }),

  /** Remediation events across incidents, newest first. */
  remediationEvents: (query: RemediationEventQuery, signal?: AbortSignal) => {
    const params = new URLSearchParams();
    for (const [key, value] of Object.entries(query)) {
      if (value !== undefined) params.set(key, String(value));
    }
    const search = params.toString();
    return request<RemediationEventList>(search ? `/api/remediation/events?${search}` : "/api/remediation/events", {
      signal,
    });
  },

  /** The latest value of each series of `metric`, as of now. */
  queryInstant: (query: { serverId: string; metric: string; instance?: string }, signal?: AbortSignal) => {
    const params = new URLSearchParams({ server_id: query.serverId, metric: query.metric });
    if (query.instance !== undefined) params.set("instance", query.instance);
    return request<QueryResult>(`/api/metrics/query?${params}`, { signal });
  },

  queryRange: (query: RangeQuery, signal?: AbortSignal) => {
    const params = new URLSearchParams({ server_id: query.serverId, metric: query.metric });
    if (query.instance !== undefined) params.set("instance", query.instance);
    params.set("start", query.start.toISOString());
    params.set("end", query.end.toISOString());
    params.set("step", String(query.step));
    if (query.agg) params.set("agg", query.agg);
    if (query.rollup) params.set("rollup", query.rollup);
    return request<QueryResult>(`/api/metrics/query_range?${params}`, { signal });
  },
};
