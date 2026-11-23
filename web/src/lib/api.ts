// Typed client for the MonitorFlow REST API (crates/server/src/api/mod.rs).
//
// Authentication is the server's HttpOnly session cookie only: this module
// never sees or stores a token. Requests go to the page's own origin, which
// the reverse proxy (or `next dev` in development) forwards to the API.

import { t } from "@/i18n";

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
  /** Whether the user sees more than one customer (ADR-0104); read it with `useMultiCustomer`. */
  multi_customer?: boolean;
  /** Whether the user sees more than one site; read it with `useMultiSite`. */
  multi_site?: boolean;
  /** The sign-in provider that keeps the person's password; null for a password kept here (ADR-0145). */
  provider?: AccountProvider | null;
}

/** The sign-in provider an account signs in through (ADR-0145). */
export interface AccountProvider {
  id: string;
  name: string;
  kind: IdentityProviderKind;
}

export type IdentityProviderKind = "oidc" | "ldap";

/** `GET /api/auth/providers`: what the login page offers. */
export interface LoginProvider {
  id: string;
  name: string;
  kind: IdentityProviderKind;
}

export interface OidcSettings {
  kind: "oidc";
  issuer: string;
  client_id: string;
  scopes: string;
  groups_claim: string;
  ca_pem: string | null;
}

export interface LdapSettings {
  kind: "ldap";
  /** `ldaps://host[:port]`, or `ldap://host[:port]` with `starttls`. */
  url: string;
  starttls: boolean;
  ca_pem: string | null;
  bind_dn: string;
  user_base_dn: string;
  user_filter: string;
  group_base_dn: string | null;
  group_filter: string;
  group_name_attribute: string;
  nested_groups: boolean;
}

export type IdentityProviderSettings = OidcSettings | LdapSettings;

/** One line of "Groups and roles": no customer and no site is everything. */
export interface GroupMapping {
  group: string;
  role: Role;
  customer_id: string | null;
  site_id: string | null;
}

/** A sign-in provider, without its secret. */
export interface IdentityProvider {
  id: string;
  kind: IdentityProviderKind;
  name: string;
  enabled: boolean;
  has_secret: boolean;
  settings: IdentityProviderSettings;
  mappings: GroupMapping[];
  accounts: number;
  created_at: string;
  updated_at: string;
}

export interface IdentityProviderList {
  providers: IdentityProvider[];
  /** What to enter as the redirect URI in an OpenID Connect provider. */
  redirect_uri: string;
  local_administrators: number;
}

export interface IdentityProviderRequest {
  name: string;
  enabled: boolean;
  /** Empty or null keeps the stored one. */
  secret: string | null;
  settings: IdentityProviderSettings;
}

export interface IdentityProviderTest {
  id?: string;
  secret: string | null;
  settings: IdentityProviderSettings;
  username?: string;
  password?: string;
}

export interface IdentityProviderTestResult {
  ok: boolean;
  error: string | null;
  oidc: { issuer: string; keys: number; client_checked: boolean } | null;
  ldap: { active_directory: boolean; dn: string | null; groups: string[] } | null;
  roles: RoleGrant[];
}

/** `device`: no agent, watched from the central server or a relay (ADR-0148). */
export type ServerStatus = "online" | "offline" | "device";

/** A server's one status on the fleet's pages, worst first (ADR-0130). */
/** `waiting`: made by an import, waiting for its agent (ADR-0146). */
/** `unreachable`: behind a parent that is down (ADR-0152). */
export type ServerState =
  | "critical"
  | "warning"
  | "acknowledged"
  | "silenced"
  | "unreachable"
  | "offline"
  | "ok"
  | "waiting";

/**
 * `GET /api/servers/summary`: servers per state, out of `total`; `waiting` is absent from a central server older
 * than ADR-0146, `unreachable` from one older than ADR-0152.
 */
export type ServerSummary = Record<Exclude<ServerState, "waiting" | "unreachable"> | "total", number> & {
  waiting?: number;
  unreachable?: number;
};

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
  /** `debian`, `rhel`, `windows`, or `linux` for any other distribution. */
  os_family: string;
  os_version: string | null;
  /** False when the host takes no package updates or sources (ADR-0063); absent is true. */
  package_updates?: boolean;
  /** `accepted`, `off` or `unsupported` by command kind (ADR-0076); null, absent or a missing kind is unknown. */
  command_kinds?: Record<string, string> | null;
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
  /** Its one status (ADR-0130); absent from a central server older than it. */
  state?: ServerState;
  /** Latest expiry among the agent's unrevoked certificates; null when it has none. */
  cert_not_after: string | null;
  /** Offline incidents of this one (ADR-0072); absent is on. Muted when this or `site_offline_alerts` is false. */
  offline_alerts?: boolean;
  site_offline_alerts?: boolean;
  /** While a silence covers it, when the last one ends (ADR-0079). */
  silenced_until?: string | null;
  /** That silence: who made it, until when and why (ADR-0130); `window` for a maintenance window (ADR-0151). */
  silence?: { by: string | null; until: string; reason: string; window?: boolean } | null;
  /** The latest acknowledgement of its open incidents (ADR-0130). */
  acknowledgement?: { by: string; at: string; note: string | null } | null;
  /** Made by an import and waiting for its agent, which takes it over by enrolling as `peer_id` (ADR-0146). */
  awaiting_agent?: boolean;
  /** `device`: an address without an agent (ADR-0148); absent is `agent`. */
  kind?: "agent" | "device";
  /** A device's address; null for an agent. */
  address?: string | null;
  /** While a parent it depends on is down (ADR-0152). */
  unreachable?: Unreachable | null;
}

/** What a dependency's end is (ADR-0152). */
export type DependencyEndKind = "server" | "device" | "relay";

/** A check of a server or device; `instance` `""` is every instance. */
export interface DependencyCheck {
  name: string;
  instance: string;
}

/** One end of a dependency, named. */
export interface DependencyEnd {
  kind: DependencyEndKind;
  server_id: string | null;
  relay_id: string | null;
  name: string;
  site_id: string;
  /** One of its checks; null for all of it. */
  check: DependencyCheck | null;
}

/** `child` is behind `parent`; `automatic` for a server behind its relay or a check behind the relay that runs it. */
export interface Dependency {
  id: string | null;
  automatic: boolean;
  child: DependencyEnd;
  parent: DependencyEnd;
  created_at: string | null;
  created_by: string | null;
  can_change: boolean;
}

/** Why something cannot be checked now: the nearest parent that is down and its incident. */
export interface Unreachable {
  because: DependencyEnd;
  incident_id: string;
  since: string;
}

/** `GET /api/servers/{id}/dependencies` and `GET /api/relays/{id}/dependencies`. */
export interface DependencyPage {
  parents: Dependency[];
  children: Dependency[];
  /** Everything behind it, directly or through others. */
  behind: DependencyEnd[];
  suggestions: { parent: DependencyEnd; reason: "proxmox_host" | "network_device" }[];
  unreachable: Unreachable | null;
  unreachable_checks: (Unreachable & { check: DependencyCheck })[];
  can_change: boolean;
}

/** One end in `POST /api/dependencies`: a server or device, one of its checks, or a relay for a parent. */
export interface DependencyEndRequest {
  server_id?: string;
  relay_id?: string;
  check?: DependencyCheck;
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
  /** The last time AI commands were turned on or off from the web interface; null if never (ADR-0098). */
  ai_commands_change?: AiCommandsChange | null;
}

/** An `ai_commands` command: who asked to turn AI commands on or off, and what came of it (ADR-0098). */
export interface AiCommandsChange {
  command_id: string;
  on: boolean;
  state: CommandState;
  /** The username; null when the account is gone. */
  requested_by: string | null;
  issued_at: string;
  finished_at: string | null;
  error: string | null;
}

/** One page of `GET /api/servers`, sorted by site name, hostname and peer ID. */
export interface ServerList {
  servers: Server[];
  /** Pass as `cursor` for the next page; null on the last one. */
  next_cursor: string | null;
}

/** Time ranges of charts and dashboards (ADR-0020). */
export type TimeRangeLabel = "1h" | "6h" | "24h" | "7d" | "30d";

/** Which servers a dashboard panel shows, within the dashboard's sharing (ADR-0095). */
export type PanelServers =
  | { kind: "all" }
  | { kind: "site"; id: string }
  | { kind: "customer"; id: string }
  | { kind: "list"; ids: string[] };

/**
 * How a panel is drawn (ADR-0166). `area`, `columns` and `heatmap` draw
 * a chart's data, `gauge` a figure's, `bars` a top list's.
 */
export type PanelKind =
  | "chart"
  | "figure"
  | "top"
  | "alerts"
  | "gauge"
  | "bars"
  | "area"
  | "columns"
  | "heatmap"
  | "table"
  | "text"
  | "states"
  | "pie"
  | "timeline";

/** How a panel writes values when the metric's name does not say it well. */
export type PanelUnit = "ratio" | "bytes" | "bytes_per_second" | "bits_per_second" | "per_second" | "seconds" | "celsius" | "number";
export type Combine = "each" | "avg" | "max" | "min" | "sum" | "count";

/** One panel of a dashboard; fields not of its kind are absent. */
export interface Panel {
  id: string;
  title: string;
  kind: PanelKind;
  /** Columns of the three-column grid. */
  width: 1 | 2 | 3;
  servers: PanelServers;
  metric?: string;
  instance?: string;
  combine?: Combine;
  order?: "highest" | "lowest";
  count?: number;
  /** From the metric's name when absent. */
  unit?: PanelUnit;
  /** The scale; from the values when absent. */
  min?: number;
  max?: number;
  /** Where values turn amber and red, in the metric's unit. */
  thresholds?: { warning?: number; critical?: number };
  /** Whether a chart names its lines; true when absent. */
  legend?: boolean;
  /** The first colour of the palette, 1 to 8. */
  color?: number;
  /** A table's columns, sorted by the first. */
  columns?: { metric: string; instance?: string }[];
  /** A text panel's Markdown. */
  text?: string;
}

export interface DashboardSpec {
  version: 1;
  range: TimeRangeLabel;
  panels: Panel[];
}

/** Who sees a dashboard: only its owner, a site, or a customer. */
export type Sharing = { kind: "personal" } | { kind: "site"; id: string } | { kind: "customer"; id: string };

export interface DashboardSummary {
  id: string;
  name: string;
  /** With the site's or customer's `name`. */
  shared_with: Sharing & { name?: string };
  panels: number;
  updated_at: string;
  updated_by_username: string | null;
  can_edit: boolean;
  /** Whether the user may make display links for it (ADR-0099). */
  can_link: boolean;
}

export interface Dashboard extends DashboardSummary {
  spec: DashboardSpec;
  /** Pass back when saving. */
  revision: number;
}

export interface DashboardRequest {
  name: string;
  shared_with: Sharing;
  spec: DashboardSpec;
  revision?: number;
}

export interface ChartLine {
  label: string;
  server_id?: string;
  points: [number, number | null][];
}

export interface TopRow {
  server_id: string;
  hostname: string;
  instance: string | null;
  value: number | null;
}

/** A row of a table panel: a value per column, null where not reported. */
export interface TableRow {
  server_id: string;
  hostname: string;
  values: (number | null)[];
}

export interface PanelAlert {
  id: string;
  server_id: string;
  hostname: string;
  metric: string;
  instance: string;
  severity: "warning" | "critical";
  opened_at: string;
  /** When the last silence covering it ends, if one does now (ADR-0151). */
  silenced_until?: string | null;
  /** Whether a maintenance window covers it now. */
  in_maintenance?: boolean;
}

/** What one panel shows; `error` is a sentence for the panel. */
export type PanelData = { id: string; servers: number; unavailable: number } & (
  | { kind: "chart"; series: ChartLine[] }
  | { kind: "figure"; value: number | null }
  | { kind: "top"; rows: TopRow[] }
  | { kind: "table"; rows: TableRow[] }
  | { kind: "text" }
  /** Worst first; `state` without a metric, `value` with one. */
  | { kind: "states"; cells: { server_id: string; hostname: string; state?: ServerState; value?: number | null }[] }
  /** A slice with neither `state` nor `server_id` is the other servers. */
  | { kind: "pie"; slices: { label: string; state?: ServerState; server_id?: string; value: number }[] }
  /** Stretches in unix seconds, from the range's start to `now`. */
  | { kind: "timeline"; rows: { server_id: string; hostname: string; spans: { start: number; end: number; severity: "warning" | "critical" }[] }[] }
  /** `unreachable`: its servers behind a parent that is down (ADR-0152); absent before it. */
  | { kind: "alerts"; alerts: PanelAlert[]; unreachable?: (Unreachable & { server_id: string; hostname: string })[] }
  | { kind: "error"; error: string }
);

/** `POST /api/dashboard-data`. */
export interface DashboardData {
  start: number;
  end: number;
  step: number;
  /** When figures and top lists were read, unix seconds. */
  at: number;
  /** The server's clock when it answered, unix seconds. */
  now: number;
  panels: PanelData[];
}

/** What a screen shows through a display link (ADR-0099). */
export interface ScreenView {
  /** Where the screen is: "Reception TV". */
  name: string;
  seconds_per_page: number;
  refresh_seconds: number;
  expires_at: string | null;
  dashboards: { id: string; name: string; spec: DashboardSpec }[];
}

/** A display link, without its secret. */
export interface DisplayLink {
  id: string;
  name: string;
  /** Those the reader sees, in the order shown. */
  dashboards: { id: string; name: string }[];
  /** How many more it shows that the reader does not see. */
  other_dashboards: number;
  seconds_per_page: number;
  created_by_username: string;
  created_at: string;
  /** null: never. */
  expires_at: string | null;
  last_used_at: string | null;
}

export type DisplayLinkExpiry = "30d" | "1y" | "never";

export interface DisplayLinkRequest {
  name: string;
  dashboard_ids: string[];
  seconds_per_page: number;
  expires: DisplayLinkExpiry;
}

/** Parts of MonitorFlow an API key may be limited to (ADR-0101). */
export type ApiKeyArea = "servers" | "alerts" | "updates" | "remediation" | "dashboards" | "administration";

export type ApiKeyExpiry = "30d" | "90d" | "1y" | "never";

/** An API key of the public API, without its secret (ADR-0101). */
export interface ApiKey {
  id: string;
  name: string;
  /** The first characters after `mfk_`, to tell keys apart. */
  prefix: string;
  /** The highest role it acts with. */
  role: Role;
  /** Its reach: both null for everything. */
  customer_id: string | null;
  customer_name: string | null;
  site_id: string | null;
  site_name: string | null;
  /** Null for every area. */
  areas: ApiKeyArea[] | null;
  created_by: string;
  created_by_username: string;
  created_at: string;
  expires_at: string | null;
  last_used_at: string | null;
}

export interface ApiKeyRequest {
  name: string;
  role: Role;
  customer_id?: string;
  site_id?: string;
  areas?: ApiKeyArea[];
  expires: ApiKeyExpiry;
}

/** `within` of `POST /api/dashboard-data`: the dashboard's sharing, or everything. */
export type Within = { kind: "all" } | { kind: "site"; id: string } | { kind: "customer"; id: string };

/** Metrics panels can show, and what one of them watches. */
export interface DashboardCatalog {
  metrics: string[];
  instances: string[];
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

/** One line of a server's system logs (ADR-0091). */
export interface LogLine {
  /** RFC 3339. */
  time: string;
  message: string;
  /** syslog priority, 0 emergency to 7 debug; null when unknown. */
  priority: number | null;
  unit: string | null;
  identifier: string | null;
  pid: number | null;
}

/** `GET /api/servers/{id}/logs`. */
export interface LogList {
  /** Newest first. */
  lines: LogLine[];
  /** More lines matched than the limit. */
  more: boolean;
  /** The units of the range, sorted, for the unit filter. */
  units: string[];
}

export interface LogQuery {
  start?: Date;
  end?: Date;
  /** "error": errors and worse; otherwise warnings and worse, all that is forwarded. */
  level?: "error" | "warning";
  unit?: string;
  /** Text anywhere in the message, without regard to case. */
  q?: string;
  limit?: number;
}

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

/** One instance of a sensor's metric, as `GET /api/servers/{id}/sensor-values` gives it. */
export interface SensorSeries {
  /** "" for a metric without instances. */
  instance: string;
  /** The last hour, one worst value per step. */
  points: [number, number | null][];
  last_value: number | null;
  last_at: string | null;
  /** Set when the last sample is late: "No data for 5 minutes". */
  reason: string | null;
}

/** A sensor's recent values, by its name and configured instance. */
export interface SensorValue {
  name: string;
  instance: string;
  metric: string;
  series: SensorSeries[];
  /** Why there is no series at all: "No data yet". */
  reason: string | null;
}

/** Body of `GET /api/servers/{id}/sensor-values`: every sensor in one answer. */
export interface SensorValueList {
  start: number;
  end: number;
  step: number;
  sensors: SensorValue[];
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
  /** Who runs it (ADR-0148): null or absent for the agent, `central` or `relay`. */
  probe?: Probe["probe"] | null;
  probe_relay_id?: string | null;
}

/** Where a site's network checks can run from (ADR-0148). */
export interface Probe {
  probe: "central" | "relay";
  relay_id: string | null;
  /** `central` or the relay's name: the instance its checks default to. */
  key: string;
  name: string | null;
  online: boolean;
  /** What a device's new check runs from when it names none. */
  default: boolean;
}

/** A network scan's own SNMP credentials, without secrets (ADR-0154). */
export interface NetworkScanSnmp {
  version: string;
  port: number;
  user: string | null;
  auth_protocol: string | null;
  priv_protocol: string | null;
}

/** One run of a network scan. */
export interface NetworkScanRun {
  id: string;
  state: "queued" | "running" | "done" | "failed";
  trigger: "person" | "schedule";
  queued_at: string;
  started_at: string | null;
  finished_at: string | null;
  addresses: number;
  scanned: number;
  answered: number;
  compared: boolean;
  new: number;
  gone: string[];
  dropped: number;
  ping_error: string | null;
  /** `interrupted`, `relay_disconnected`, `relay_outdated`, `relay_removed`, or the probe's words. */
  error: string | null;
}

export type ScanEvery = "hour" | "day" | "week";

/** A network scan of a site's addresses (ADR-0154). */
export interface NetworkScan {
  id: string;
  site_id: string;
  site_name: string;
  name: string;
  targets: string[];
  addresses: number;
  ports: number[];
  probe: "central" | "relay";
  probe_relay_id: string | null;
  probe_key: string;
  every: ScanEvery | null;
  notify: boolean;
  snmp: NetworkScanSnmp | null;
  created_at: string;
  updated_at: string;
  last_run: NetworkScanRun | null;
}

/** Body of `POST /api/network-scans`; `PUT` takes the same without `site_id` and `run`. */
export interface NetworkScanRequest {
  site_id?: string;
  name: string;
  targets: string[];
  ports?: number[];
  probe?: string;
  every?: ScanEvery | null;
  notify?: boolean;
  snmp?: SnmpCredentialsRequest;
  remove_snmp?: boolean;
  run?: boolean;
}

export type ProposalAction = "agent" | "snmp_device" | "device" | "monitored" | "dismissed";

/** What a scan proposes for an address. */
export interface NetworkScanProposal {
  action: ProposalAction;
  os: "linux" | "windows" | null;
  monitored_by: { server_id: string; name: string; matched_by: "address" | "mac" | "name" } | null;
  name: string;
  template: string | null;
  checks: { kind: "ping" | "port" | "snmp"; name: string; port: number | null }[];
}

/** An address that answered a scan. */
export interface NetworkScanHost {
  address: string;
  ping: boolean;
  rtt_seconds: number | null;
  ports: number[];
  name: string | null;
  mac: string | null;
  snmp: boolean;
  snmp_description: string | null;
  snmp_name: string | null;
  snmp_object_id: string | null;
  snmp_groups: string[];
  new: boolean;
  proposal: NetworkScanProposal;
}

export interface NetworkScanResults {
  run: NetworkScanRun | null;
  current: NetworkScanRun | null;
  hosts: NetworkScanHost[];
}

export interface NetworkScanAddResult {
  added: { address: string; server_id: string; name: string; template: string | null }[];
  skipped: { address: string; reason: "not_found" | "monitored" | "no_credentials" }[];
}

/** Body of `POST /api/devices`. */
export interface DeviceRequest {
  site_id: string;
  name: string;
  address: string;
  /** `central` or a relay's ID; absent: the site's default. */
  probe?: string;
  /** Add a ping every minute and its alerts; true when absent. */
  ping?: boolean;
  /** How to reach it over SNMP (ADR-0149); needed with `template`. */
  snmp?: SnmpCredentialsRequest;
  /** The kind of device whose SNMP checks and alerts it gets. */
  template?: string;
  /** For a vCenter or ESXi host (ADR-0156): its `vmware` check and alerts come with it. */
  vmware?: VmwareCredentialsRequest;
}

/** vCenter or ESXi credentials as sent; what is left out keeps the stored value (ADR-0156). */
export interface VmwareCredentialsRequest {
  port?: number;
  user?: string;
  password?: string;
  /** A confirmed certificate's SHA-256; "" goes back to the roots. */
  fingerprint?: string;
}

/** A device's vCenter credentials, never the password. */
export interface VmwareCredentials {
  port: number;
  user: string;
  fingerprint: string | null;
  password_set: boolean;
  updated_at: string;
}

/** What a vCenter or ESXi host answered to a test. */
export interface VmwareTested {
  answers: boolean;
  error: string | null;
  product: string | null;
  version: string | null;
  api_type: string | null;
  certificate: {
    fingerprint: string;
    trusted: boolean;
    by_roots: boolean;
    subject: string;
    not_after: string | null;
  } | null;
  /** null when not tried: no credentials, or a certificate not trusted yet. */
  login: boolean | null;
  objects: Record<string, number>;
  probe: string;
}

/** One host, cluster, virtual machine or datastore of a source. */
export interface VirtualObject {
  id: string;
  kind: "vm" | "host" | "cluster" | "datastore";
  name: string;
  key: string;
  details: Record<string, string>;
  metrics: Record<string, number>;
  collected_at: string;
  seen_at: string;
  stale: boolean;
  server: { id: string; hostname: string; kind: string; awaiting_agent: boolean } | null;
  matched_by: "uuid" | "mac" | "address" | "name" | "person" | null;
  link_fixed: boolean;
}

/** `GET /api/servers/{id}/virtualization`. */
export interface Virtualization {
  source: {
    up: boolean;
    reason: string | null;
    error: string | null;
    details: Record<string, string>;
    seen_at: string;
  } | null;
  objects: VirtualObject[];
  guest_of: { object: VirtualObject; source_id: string; source_name: string }[];
}

/** SNMP credentials as sent; a secret left out keeps the stored one while the version stays. */
export interface SnmpCredentialsRequest {
  version: "v1" | "v2c" | "v3";
  port?: number;
  community?: string;
  user?: string;
  auth_protocol?: string;
  auth_password?: string;
  priv_protocol?: string;
  priv_password?: string;
}

/** A device's SNMP credentials as the API shows them: never a secret. */
export interface SnmpCredentials {
  version: "v1" | "v2c" | "v3";
  port: number;
  user: string | null;
  auth_protocol: string | null;
  priv_protocol: string | null;
  community_set: boolean;
  auth_password_set: boolean;
  priv_password_set: boolean;
  updated_at: string;
}

/** The state a lookup entry gives (ADR-0157). */
export type SnmpState = "ok" | "warning" | "critical";

/** One entry of a lookup: an integer, a range from `value` to `max`, or a text. */
export interface SnmpLookupEntry {
  value?: number;
  max?: number;
  text?: string;
  name: string;
  state: SnmpState;
}

/** Names and states for the values of an enumerated object, built in or a customer's own. */
export interface SnmpLookup {
  id: string;
  builtin: boolean;
  customer_id: string | null;
  name: string;
  entries: SnmpLookupEntry[];
  otherwise: SnmpState;
  used_by: number;
  updated_at: string | null;
  can_change: boolean;
}

export interface SnmpLookupRequest {
  customer_id?: string;
  name: string;
  entries: SnmpLookupEntry[];
  otherwise?: SnmpState;
}

/** An uploaded MIB module. */
export interface SnmpMib {
  id: string;
  customer_id: string;
  module: string;
  file_name: string;
  objects: number;
  uploaded_at: string;
  can_change: boolean;
}

export interface SnmpOfferedLookup {
  name: string;
  module: string;
  entries: SnmpLookupEntry[];
}

/** What a channel reading an object should be. */
export interface SnmpSuggestion {
  type: SnmpValueType;
  unit: string;
  multiply: number;
  divide: number;
  lookup: string | null;
  table: string | null;
  name_column: string | null;
}

/** An object the library or a MIB names. */
export interface SnmpObject {
  name: string;
  module: string;
  source: "library" | "mib";
  oid: string;
  kind: "node" | "scalar" | "table" | "entry" | "column" | "notification";
  syntax: string | null;
  enumeration: [number, string][];
  units: string | null;
  description: string;
  suggestion: SnmpSuggestion;
}

export type SnmpValueType = "gauge" | "counter" | "text";

/** A channel's limits: an ordinary threshold of the device. */
export interface SnmpLimits {
  comparison: string;
  warning: number | null;
  critical: number | null;
  duration_s: number;
}

export interface SnmpChannelRequest {
  oid: string;
  name: string;
  key?: string;
  type?: SnmpValueType;
  unit?: string;
  multiply?: number;
  divide?: number;
  lookup?: string;
  match?: string;
  expect?: string;
  severity?: "warning" | "critical";
  limits?: SnmpLimits | null;
}

export interface SnmpCheckRequest {
  name: string;
  probe?: string;
  interval_ms?: number;
  enabled?: boolean;
  table?: string;
  name_column?: string;
  channels: SnmpChannelRequest[];
}

export interface SnmpChannelValue {
  instance: string;
  value: number | null;
  state: number | null;
  text: string | null;
  at: string | null;
}

export interface SnmpChannel extends SnmpChannelRequest {
  key: string;
  metric: string;
  state_metric: string | null;
  values: SnmpChannelValue[];
}

/** A custom SNMP check of a device (ADR-0157). */
export interface SnmpCheck {
  entry_id: string;
  name: string;
  instance: string;
  probe: string | null;
  probe_relay_id: string | null;
  interval_ms: number;
  enabled: boolean;
  table: string | null;
  name_column: string | null;
  channels: SnmpChannel[];
}

export interface SnmpReadObject {
  oid: string;
  name: string | null;
  type: string;
  value: string;
}

export interface SnmpRead {
  probe: string;
  objects: SnmpReadObject[];
  cut: boolean;
}

export interface SnmpTestedChannel {
  key: string;
  name: string;
  oid: string;
  type: string | null;
  raw: string | null;
  value: number | null;
  state: number | null;
  text: string | null;
  note: string | null;
}

export interface SnmpTested {
  probe: string;
  rows: { instance: string; channels: SnmpTestedChannel[] }[];
  cut: boolean;
}

/** A kind of SNMP device (ADR-0149). */
export interface SnmpTemplate {
  key: string;
  groups: string[];
  thresholds: { metric: string; comparison: Comparison; warning: number | null; critical: number | null; duration_s: number }[];
}

/** What an address answered over SNMP. */
export interface Detected {
  answers: boolean;
  error: string | null;
  description: string | null;
  name: string | null;
  object_id: string | null;
  groups: string[];
  template: string | null;
  probe: string;
}

/** How often a site's built-in checks run, and whether the site set it (ADR-0097). */
export interface CheckIntervalOf {
  check_interval_ms: number;
  check_interval_origin: "default" | "site";
}

/** `GET /api/servers/{id}/sensors`: the effective configuration and its drift. */
export interface EffectiveSensors extends CheckIntervalOf {
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
  /** What the rule judges (ADR-0153); absent from older servers: `value`. */
  kind?: RuleKind;
  comparison: Comparison;
  warning: number | null;
  critical: number | null;
  duration_s: number;
  hysteresis: number;
  function?: RuleFunction | null;
  window_s?: number | null;
  offset_s?: number | null;
  target?: number | null;
  conditions?: RuleCondition[];
}

/** What a threshold judges (ADR-0153). */
export type RuleKind = "value" | "window" | "compare" | "forecast" | "nodata";
/** A window's function, or how a comparison compares. */
export type RuleFunction = "avg" | "min" | "max" | "change" | "rate" | "ratio" | "difference";

/** A condition joined with "and" to a threshold, on the same server. */
export interface RuleCondition {
  metric: string;
  /** Null: the instance of the series judged, else the one without. */
  instance: string | null;
  function: "value" | "avg" | "min" | "max" | "change" | "rate";
  window_s?: number | null;
  comparison: Comparison;
  value: number;
}

/** A rule MonitorFlow suggests: post `rule` to add it, with `enabled: false` to decline it. */
export interface RecommendedThreshold {
  key: string;
  rule: ThresholdRequest;
}

/** `GET /api/servers/{id}/thresholds/effective`. */
export interface EffectiveThresholds {
  server_id: string;
  site_id: string;
  revision: number;
  thresholds: EffectiveThreshold[];
  recommended?: RecommendedThreshold[];
}

/** An incident a rule would have opened, in a preview. Unix milliseconds. */
export interface PreviewIncident {
  severity: "warning" | "critical";
  opened_at: number;
  resolved_at: number | null;
  peak_value: number;
}

/** One series replayed by a preview. */
export interface PreviewSeries {
  instance: string;
  /** `[Unix ms, judged value]`. */
  points: [number, number][];
  incidents: PreviewIncident[];
}

/** `POST /api/servers/{id}/thresholds/preview`. */
export interface ServerPreview {
  server_id: string;
  from: number;
  to: number;
  days: number;
  series: PreviewSeries[];
  truncated: boolean;
}

/** `POST /api/sites/{id}/thresholds/preview`. */
export interface SitePreview {
  site_id: string;
  from: number;
  to: number;
  days: number;
  servers: { server_id: string; hostname: string; incidents: number; series: PreviewSeries[] }[];
  more_servers: boolean;
}

/** `GET /api/sites/{id}/sensors`: the built-in defaults overlaid with the site's rows. */
export interface SiteSensors extends CheckIntervalOf {
  site_id: string;
  sensors: EffectiveSensor[];
}

/** `GET /api/sites/{id}/thresholds/effective`: the built-in defaults overlaid with the site's rows. */
export interface SiteThresholds {
  site_id: string;
  thresholds: EffectiveThreshold[];
  recommended?: RecommendedThreshold[];
}

/** Where a sensor or threshold row applies: every server of a site, or one server. */
/** A check a recommendation proposes (ADR-0134): a kind and its parameters. */
export interface ProposedCheck {
  kind: string;
  params: Record<string, string>;
}

/** What the agent saw that nothing checks yet (`GET /api/servers/{id}/recommendations`). */
export interface CheckRecommendation {
  key: string;
  title: string;
  evidence: string[];
  checks: ProposedCheck[];
  dismissed_by: string | null;
  dismissed_at: string | null;
}

export interface RecommendationList {
  /** When the agent last reported what runs; null before it has. */
  reported_at: string | null;
  recommendations: CheckRecommendation[];
}

export type ConfigOwner = { kind: "sites" | "servers"; id: string };

/** Body of a sensor row create or update (ADR-0016); an update replaces every field. */
export interface SensorConfigRequest {
  kind: string;
  name: string;
  instance?: string;
  interval_ms: number;
  timeout_ms?: number | null;
  params?: Record<string, string | number | boolean>;
  enabled?: boolean;
  /** `central` or a relay's ID: it runs from there instead of the agent (ADR-0148). */
  probe?: string | null;
}

/** One stored row of `sensor_configs`; exactly one of `site_id` and `server_id` is set. */
export interface SensorConfigEntry extends Required<
  Omit<SensorConfigRequest, "timeout_ms" | "probe">
> {
  id: string;
  site_id: string | null;
  server_id: string | null;
  timeout_ms: number | null;
  /** Who runs it (ADR-0148): null or absent for the agent. */
  probe?: "central" | "relay" | null;
  probe_relay_id?: string | null;
  /** A website check's password is set; it is never returned (ADR-0150). */
  secret_set?: boolean;
}

/** The certificate of a website check's last https connection (ADR-0150). */
export interface WebsiteCertificate {
  ok: boolean;
  /** `expired`, `not_yet_valid`, `self_signed`, `untrusted`, `name`, `revoked` or `other`. */
  reason: string | null;
  name_ok: boolean | null;
  days_left: number | null;
  not_after: string | null;
  issuer: string | null;
  subject: string | null;
}

/** One website check and its last run, from `GET /api/servers/{id}/website-checks`. */
export interface WebsiteCheck {
  name: string;
  instance: string;
  entry_id: string | null;
  origin: "default" | "site" | "server";
  url: string;
  run_by: "agent" | "central" | "relay";
  relay: string | null;
  interval_ms: number;
  last_at: string | null;
  up: boolean | null;
  /** Why not: `dns`, `connect`, `timeout`, `tls`, `certificate`, `status`, `content`, `redirects`, `refused`, `protocol`. */
  reason: string | null;
  status_code: number | null;
  response_seconds: number | null;
  redirects: number | null;
  final_url: string | null;
  content_ok: boolean | null;
  certificate: WebsiteCertificate | null;
}

export interface WebsiteCheckList {
  checks: WebsiteCheck[];
}

/** Body of a threshold create or update (ADR-0019); an update replaces every field. */
export interface ThresholdRequest {
  metric: string;
  instance: string | null;
  /** `value` when absent. */
  kind?: RuleKind;
  comparison: Comparison;
  warning: number | null;
  critical: number | null;
  duration_s: number;
  hysteresis: number;
  enabled: boolean;
  function?: RuleFunction;
  window_s?: number;
  offset_s?: number;
  target?: number;
  conditions?: RuleCondition[];
}

/** One stored row of `thresholds`; exactly one of `site_id` and `server_id` is set. */
export interface ThresholdEntry extends ThresholdRequest {
  id: string;
  site_id: string | null;
  server_id: string | null;
}

/** One entry of `GET /api/sites`. */
export interface Site {
  id: string;
  name: string;
  server_count: number;
  /** Enrolled relays, without removed ones. */
  relay_count: number;
  customer_id: string;
  customer_name: string;
  /** The site's automatic update override; `null` follows the server-wide setting (ADR-0055). */
  agent_auto_update: boolean | null;
  /** Whether automatic updates apply to the site's agents and relays. */
  agent_auto_update_effective: boolean;
  /** Whether its servers and relays open incidents when offline (ADR-0072); absent is on. */
  offline_alerts?: boolean;
}

/** Server-wide automatic update settings (ADR-0055). */
export interface AutoUpdateSettings {
  /** Default of the sites that inherit it. */
  enabled: boolean;
  /** Set by the server's configuration: the web interface cannot change `enabled`. */
  enabled_locked: boolean;
  paused: boolean;
  failure_threshold: number;
}

export type RolloutKind = "agent" | "relay";

/** The automatic rollout of one release. */
export interface Rollout {
  kind: RolloutKind;
  version: string;
  state: "rolling" | "halted";
  started_at: string;
  halted_at: string | null;
  reason: string | null;
  /** Failures count from here: the start or the last resume. */
  counted_from: string;
}

/** Where a rollout of the history stands; `state` only tells whether it halted. */
export type RolloutOutcome = "rolling" | "halted" | "finished" | "superseded";

export interface HistoryRollout extends Rollout {
  outcome: RolloutOutcome;
}

export interface AutoUpdateTotals {
  up_to_date: number;
  /** Older and not tried yet. */
  waiting: number;
  /** Of `waiting`, agents whose relay is older or being updated. */
  waiting_for_relay: number;
  pending: number;
  succeeded: number;
  failed: number;
}

export type SitePhase = "canary" | "rolling" | "blocked";

export interface AutoUpdateSite {
  id: string;
  name: string;
  customer_name: string;
  auto_update: boolean | null;
  enabled: boolean;
  phase: SitePhase | null;
  totals: AutoUpdateTotals;
}

/** Automatic updates of one kind of peer. */
export interface AutoUpdateKind {
  /** Newest stable release the server holds. */
  latest: string | null;
  /** What peers move to: `latest` unless the server is older. */
  target: string | null;
  rollout: Rollout | null;
  totals: AutoUpdateTotals;
  sites: AutoUpdateSite[];
  history: HistoryRollout[];
  /** How many run each version, newest first. */
  versions: AutoUpdateVersionCount[];
}

/** One version in use and how many agents or relays run it. */
export interface AutoUpdateVersionCount {
  /** `null` for those that have not reported one yet. */
  version: string | null;
  count: number;
  /** On the target or newer; `false` without a target or for a development build. */
  up_to_date: boolean;
}

/** One agent (a server) or relay of `GET /api/auto-update/{kind}/versions/peers`. */
export interface AutoUpdateVersionPeer {
  id: string;
  name: string;
  site_id: string;
  site_name: string;
  customer_id: string;
  customer_name: string;
  /** Towards the target; `null` without a target or outside every total. */
  state: AutoUpdatePeerState | null;
}

/** `GET /api/auto-update/{kind}/versions/peers`: one page and the total. */
export interface AutoUpdateVersionPeerList {
  version: string | null;
  target: string | null;
  total: number;
  /** By customer, site and name. */
  peers: AutoUpdateVersionPeer[];
}

/** `GET /api/auto-update`. */
export interface AutoUpdateStatus {
  settings: AutoUpdateSettings;
  server_version: string;
  agents: AutoUpdateKind;
  /** Updated before the agents behind them. */
  relays: AutoUpdateKind;
}

/** One of the totals of `GET /api/auto-update`. */
export type AutoUpdatePeerState = "up_to_date" | "waiting" | "pending" | "failed";

/** One agent (a server) or relay of `GET /api/auto-update/{kind}/peers`. */
export interface AutoUpdatePeer {
  /** The server's ID for an agent, the relay's for a relay. */
  id: string;
  /** The server's hostname or the relay's name. */
  name: string;
  site_id: string;
  site_name: string;
  customer_id: string;
  customer_name: string;
  version: string | null;
  state: AutoUpdatePeerState;
  /** A waiting agent whose relay is older or being updated. */
  waiting_for_relay: boolean;
  /** Why its update failed, in the peer's words. */
  error: string | null;
  finished_at: string | null;
}

/** `GET /api/auto-update/{kind}/peers`: one page and the total. */
export interface AutoUpdatePeerList {
  target: string | null;
  /** The matching total of `GET /api/auto-update`. */
  total: number;
  /** Failed ones newest first, the others by customer, site and name. */
  peers: AutoUpdatePeer[];
}

/** The most peers one request lists. */
export const AUTO_UPDATE_PEERS_MAX = 500;

/** Fields of `PUT /api/settings/auto-update`; absent ones are unchanged. */
export interface AutoUpdateChange {
  enabled?: boolean;
  paused?: boolean;
  failure_threshold?: number;
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
  /** While it waits for an administrator: the names it reported, on no certificate yet (ADR-0066). */
  pending_names?: string[] | null;
  /** The address it enrolled from, while it waits. */
  pending_source?: string | null;
  /** Servers whose latest connection came through the relay. */
  server_count: number;
  /** Agent streams it forwards right now. */
  connected_agents: number;
  /** Version of its last Hello; null before it reported one (ADR-0055). */
  relay_version?: string | null;
  /** Its latest self-update. */
  last_update?: RelayCommand | null;
  /** Offline incidents of this one (ADR-0072); absent is on. Muted when this or `site_offline_alerts` is false. */
  offline_alerts?: boolean;
  site_offline_alerts?: boolean;
  /** While a silence covers it, when the last one ends (ADR-0079). */
  silenced_until?: string | null;
}

/** A relay self-update (ADR-0055). */
export interface RelayCommand {
  id: string;
  relay_id: string;
  version: string;
  state:
    | "queued"
    | "sent"
    | "running"
    | "succeeded"
    | "failed"
    | "expired"
    | "cancelled";
  /** Null when queued by automatic updates. */
  requested_by: string | null;
  issued_at: string;
  expires_at: string;
  finished_at: string | null;
  stdout: string | null;
  error: string | null;
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
  /** Its latest self-updates, newest first. */
  updates?: RelayCommand[];
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
  /** Set for every kind but `value` (ADR-0153). */
  kind?: RuleKind;
  function?: RuleFunction | null;
  window_s?: number | null;
  offset_s?: number | null;
  target?: number | null;
  conditions?: RuleCondition[];
}

/** One incident of `GET /api/incidents`. Times are sample times. */
export interface Incident {
  id: string;
  /** Null for a relay's offline incident. */
  server_id: string | null;
  /** Set for a relay's offline incident (ADR-0072). */
  relay_id?: string | null;
  /** The server's hostname, or the relay's name. */
  hostname: string;
  site_id: string;
  site_name: string;
  customer_id?: string;
  /** `offline` when the server or relay stopped reporting (ADR-0072). */
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
  /** While open and covered by a silence, of its place or of its check, when the last one ends (ADR-0079, ADR-0151). */
  silenced_until?: string | null;
  /** While open, whether a maintenance window covers it now (ADR-0151). */
  in_maintenance?: boolean;
  /** Whether a silence ever held back a notification of it. */
  silenced?: boolean;
  /** The kind of rule that opened it (ADR-0153); absent from older servers. */
  kind?: RuleKind;
  /** While a forecast is open, when it expects its target. */
  expected_at?: string | null;
  /** The parent's incident that speaks for it: it opened a moment before its parent went down and was never told (ADR-0152). */
  hidden_by?: string | null;
  /** Who said they are on it, and when (ADR-0082); null until someone does. */
  acknowledged_by?: string | null;
  acknowledged_at?: string | null;
  /** What they said they are doing; null when acknowledged from a link or by a script without one. */
  acknowledged_note?: string | null;
  /** Open, not acknowledged, and the reader an operator or above of its site. */
  can_acknowledge?: boolean;
  /** Why it opened, in one sentence: "9% available is below the warning limit of 12%"; once resolved, its peak. */
  reason?: string;
}

/** What a silence covers (ADR-0079). */
export type SilenceScope = "server" | "relay" | "site" | "customer" | "all";

/** One active or upcoming silence of `GET /api/silences`. */
export interface Silence {
  id: string;
  scope: SilenceScope;
  /** Null for everything. */
  target_id: string | null;
  target_name: string | null;
  site_id: string | null;
  customer_id: string | null;
  starts_at: string;
  ends_at: string;
  /** Started, as opposed to upcoming. */
  active: boolean;
  reason: string;
  created_by_username: string | null;
  created_at: string;
  /** Only these checks of its server or site; null or absent for all of it (ADR-0151). */
  checks?: SilencedCheck[] | null;
  /** The maintenance window it is an occurrence of; null for a one-off silence. */
  window_id?: string | null;
  /** Whether this user may end it. */
  can_end: boolean;
}

/** A check by its identity: its name and what it watches, `""` for every instance (ADR-0151). */
export interface SilencedCheck {
  name: string;
  instance: string;
}

/** Body of `POST /api/silences`. */
export interface SilenceRequest {
  scope: SilenceScope;
  target_id?: string;
  /** Now when absent. */
  starts_at?: string;
  ends_at: string;
  reason: string;
  /** Only these checks of a server or site. */
  checks?: SilencedCheck[];
}

/** How a maintenance window comes back (ADR-0151). */
export type MaintenanceRepeat = "daily" | "weekly" | "monthly";

/** `monday` to `sunday`. */
export type WeekdayName = "monday" | "tuesday" | "wednesday" | "thursday" | "friday" | "saturday" | "sunday";

/** Body of `POST /api/maintenance-windows` and `PUT /api/maintenance-windows/{id}`. */
export interface MaintenanceWindowRequest {
  site_id: string;
  /** Empty: the whole site. */
  server_ids: string[];
  /** Null or absent: everything of the servers. */
  checks?: SilencedCheck[] | null;
  repeat: MaintenanceRepeat;
  every: number;
  weekdays: WeekdayName[];
  month_day?: number | null;
  /** 1 to 4, or 5 for the last week. */
  month_week?: number | null;
  /** 1 to 12; empty is every month. */
  months: number[];
  /** `HH:MM`, local. */
  start_time: string;
  duration_minutes: number;
  time_zone: string;
  starts_on?: string | null;
  ends_on?: string | null;
  reason: string;
}

/** One window of a rule. */
export interface MaintenanceOccurrence {
  starts_at: string;
  ends_at: string;
}

/** One maintenance window of `GET /api/maintenance-windows`. */
export interface MaintenanceWindow extends Omit<MaintenanceWindowRequest, "server_ids" | "starts_on"> {
  id: string;
  site_name: string;
  customer_id: string;
  /** The whole site; else only `servers`, which may be none left once removed. */
  whole_site?: boolean;
  servers: { id: string; hostname: string }[];
  starts_on: string;
  /** When the one running now ends; null when none runs. */
  active_until: string | null;
  /** The running window, if any, then the next ones. */
  next: MaintenanceOccurrence[];
  created_by_username: string | null;
  created_at: string;
  updated_at: string;
  can_change: boolean;
}

/** One page of incidents: open ones first, then newest first. */
export interface IncidentList {
  incidents: Incident[];
  next_cursor: string | null;
}

export type IncidentEventKind =
  | "opened"
  | "escalated"
  | "deescalated"
  | "resolved"
  | "silenced"
  | "silence_ended"
  | "acknowledged"
  | "followed_up";

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

/** A notification that sent this incident together with others (ADR-0080). */
export interface NotificationGroup {
  /** `group` of `GET /api/incidents` lists them. */
  id: string;
  /** How many incidents it sent, this one included. */
  incidents: number;
  created_at: string;
}

/** Where an incident is in its follow-up: who is told next when nobody acknowledges (ADR-0082). */
export interface FollowUp {
  /** `waiting` for the next step at `next_at`, `silenced` until a silence ends, or `stopped`. */
  state: "waiting" | "silenced" | "stopped";
  stop_reason: "acknowledged" | "resolved" | "not_critical" | "finished" | null;
  /** Steps told so far, and how many there are. */
  step: number;
  steps: number;
  started_at: string;
  next_at: string | null;
  /** Who the next step tells, people first; empty when nobody can be. */
  next: string[];
  policy: "default" | "everything" | "customer" | "site";
}

/** `GET /api/incidents/{id}`: the incident and its events in the order they happened. */
export interface IncidentDetail extends Incident {
  events: IncidentEvent[];
  /** Oldest first; absent from servers before ADR-0080. */
  notification_groups?: NotificationGroup[];
  /** `page` or `link`: where it was acknowledged. */
  acknowledged_via?: "page" | "link" | null;
  /** Null when no follow-up started: a warning, or not sent yet. */
  follow_up?: FollowUp | null;
  /** While it says a parent is down: everything behind it, unreachable meanwhile (ADR-0152). */
  hides?: DependencyEnd[];
  /** Incidents children opened a moment before it, never told. */
  hidden?: string[];
}

/** Something a follow-up step names; `name` is null once removed. */
export interface FollowUpNamed {
  id: string;
  name: string | null;
}

/** One step of a follow-up as `GET /api/follow-up/policies` shows it. */
export interface FollowUpStepView {
  after_minutes: number;
  on_call: FollowUpNamed[];
  users: FollowUpNamed[];
  channels: FollowUpNamed[];
  administrators: boolean;
}

/** One step as saved. */
export interface FollowUpStep {
  after_minutes: number;
  on_call: string[];
  users: string[];
  channels: string[];
  administrators: boolean;
}

/** The follow-up of one place. */
export interface FollowUpPolicy {
  id: string;
  /** The customer it covers, or the site's; null with `site_id` too: everything. */
  customer_id: string | null;
  customer_name: string | null;
  site_id: string | null;
  site_name: string | null;
  steps: FollowUpStepView[];
  updated_at: string;
}

/** A schedule or channel a step may name. */
export interface FollowUpChoice {
  id: string;
  name: string;
  customer_id: string | null;
  site_id: string | null;
}

/** `GET /api/follow-up/policies`, administrators only. */
export interface FollowUpPolicyList {
  policies: FollowUpPolicy[];
  default_steps: FollowUpStepView[];
  schedules: FollowUpChoice[];
  channels: FollowUpChoice[];
}

/** Body of `PUT /api/follow-up/policies`: the steps of one place, replacing any. */
export interface FollowUpPolicyRequest {
  customer_id: string | null;
  site_id: string | null;
  steps: FollowUpStep[];
}

/** What an acknowledge link acknowledges, and as whom. */
export interface AcknowledgeLink {
  username: string;
  expires_at: string;
  used_at: string | null;
  incidents: {
    id: string;
    text: string;
    state: IncidentState;
    acknowledged_by: string | null;
    acknowledged_at: string | null;
  }[];
}

/** Filters of `GET /api/incidents`; all optional. */
export interface IncidentQuery {
  state?: IncidentState;
  severity?: IncidentSeverity;
  server_id?: string;
  site_id?: string;
  /** The incidents one notification sent together (ADR-0080). */
  group?: string;
  /** 1 to 500; the API's default is 100. */
  limit?: number;
  cursor?: string;
}

export type CommandState =
  | "queued"
  | "sent"
  | "running"
  | "succeeded"
  | "failed"
  | "expired"
  | "cancelled";

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
/** What a person follows of an update, in order. */
export type ServerUpdateMilestoneKind =
  | "verified"
  | "images_pulled"
  | "backed_up"
  | "database_migrated"
  | "server_restarted"
  | "agents_ready";

export interface ServerUpdateMilestone {
  kind: ServerUpdateMilestoneKind;
  /** Null while not reached, and for good once the update ended without it. */
  done_at: string | null;
}

/** How an update of the server's history ended (ADR-0172). */
export type ServerUpdateResult = "in_progress" | "succeeded" | "failed" | "rolled_back" | "unknown";

/** One update of the server, `GET /api/server-update/history`. */
export interface ServerUpdateHistoryEntry {
  id: string;
  from: string | null;
  to: string;
  /** Null when the updater started it (command line) or it is not known. */
  requested_by: string | null;
  started_at: string;
  finished_at: string | null;
  result: ServerUpdateResult;
  /** Why it failed or rolled back, in the updater's words. */
  error: string | null;
  /** `result` was deduced from the updates after it, not recorded. */
  inferred: boolean;
}

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
  /** The milestones of `update` in order, from its steps; empty without one. */
  milestones: ServerUpdateMilestone[];
  /** A request waits for the updater. */
  requested: boolean;
  /** A newer release the daily check saw (ADR-0061). */
  available: {
    version: string;
    release_notes_url: string | null;
    first_seen_at: string;
  } | null;
}

/** A tool an import reads (ADR-0146), from `GET /api/imports/sources`. */
export interface ImportSource {
  id: string;
  /** The product's name. */
  name: string;
  /** File extensions an upload accepts, without the dot; empty when it reads no files. */
  extensions: string[];
  many_files: boolean;
  /** The connection's fields; empty when it cannot connect. */
  connect: { key: string; secret: boolean; optional: boolean; placeholder: string }[];
}

/** Which list of the preview an item is in. */
export type ImportList = "exact" | "approximated" | "not_movable";

/** A note: a code the catalog translates (`imports.note.<code>`), its arguments, the English text. */
export interface ImportNote {
  code: string;
  args?: Record<string, string>;
  text: string;
}

/** What an import item creates. */
export type ImportCreate =
  | { object: "server"; peer_id: string; hostname: string; os_family: string; addresses: string[] }
  /** A device, watched from a probe (ADR-0148). */
  | { object: "device"; peer_id: string; hostname: string; address: string }
  | {
      object: "check";
      server: string;
      kind: string;
      name: string;
      interval_ms: number;
      timeout_ms?: number;
      params: Record<string, string>;
      /** Run from the site's default probe (ADR-0148). */
      probe?: boolean;
    }
  | {
      object: "threshold";
      server: string;
      metric: string;
      instance: string | null;
      comparison: Comparison;
      warning: number | null;
      critical: number | null;
      duration_s: number;
      hysteresis: number;
      /** What a richer rule judges (ADR-0153); absent for a plain threshold. */
      kind?: RuleKind;
      function?: RuleFunction;
      window_s?: number;
      offset_s?: number;
      target?: number;
      conditions?: RuleCondition[];
    }
  | { object: "silence"; server: string | null; starts_at: string; ends_at: string; reason: string; checks?: SilencedCheck[] }
  | ({ object: "window"; servers: string[]; checks?: SilencedCheck[] } & Omit<MaintenanceWindowRequest, "site_id" | "server_ids" | "checks">)
  | {
      object: "network_scan";
      name: string;
      targets: string[];
      addresses: number;
      ports: number[];
      every: "hour" | "day" | "week" | null;
      /** `central`, or the name of the relay it runs from. */
      probe: string;
      has_snmp: boolean;
    }
  | { object: "channel"; kind: string; settings: Record<string, unknown>; has_secret: boolean; site_id: string | null }
  | {
      object: "service";
      name: string;
      at_least: number | null;
      target: number | null;
      members: { server: string; check?: SilencedCheck }[];
    }
  /** A dependency (ADR-0152): the server, or one of its checks, behind the parent or one of its checks. */
  | { object: "dependency"; server: string; check?: SilencedCheck; parent: string; parent_check?: SilencedCheck };

/** One line of an import's preview. */
export interface ImportItem {
  id: string;
  /** `server`, `check`, `threshold`, `silence`, `channel`, `builtin` or `other`. */
  kind: string;
  list: ImportList;
  note?: ImportNote;
  origin: { kind: string; name: string; detail?: string };
  hosts: string[];
  /** MonitorFlow has it already. */
  present: boolean;
  /** Items it needs: leaving one out leaves this one out. */
  needs: string[];
  create?: ImportCreate;
}

/** What a confirmation and an undo did. */
export interface ImportResult {
  created: Record<string, number>;
  skipped: number;
  present: number;
  failed: { item: string; error: string }[];
  removed: Record<string, number>;
  kept_servers: string[];
}

/** One import: a preview, then confirmed, then perhaps undone. */
export interface ImportRun {
  id: string;
  source: string;
  source_name: string;
  site_id: string;
  site_name: string;
  input: string;
  state: "preview" | "confirmed" | "undone";
  created_at: string;
  created_by: string | null;
  expires_at: string | null;
  confirmed_at: string | null;
  undone_at: string | null;
  counts: { exact: number; approximated: number; not_movable: number; present: number; to_create: number };
  /** Absent in lists. */
  items?: ImportItem[];
  result: ImportResult | null;
}

/** Body of `POST /api/imports`: files or a connection. */
export interface ImportRequest {
  source: string;
  site_id?: string;
  files?: { name: string; content: string; base64?: boolean }[];
  connection?: Record<string, string>;
  /** IANA zone of maintenance windows whose source does not say it (ADR-0151). */
  time_zone?: string;
}

export type BackupKind = "daily" | "manual" | "update";

/** One backup file in the backups volume (ADR-0058). */
export interface BackupFile {
  name: string;
  size: number;
  created_at: string;
  kind: BackupKind;
}

/** The last backup, running or finished. */
export interface BackupRun {
  id: string;
  kind: BackupKind;
  /** Empty for automatic ones. */
  requested_by: string;
  state: "running" | "succeeded" | "failed";
  error: string | null;
  /** What a backup that succeeded left out or noticed. */
  warning: string | null;
  file: string | null;
  started_at: string;
  finished_at: string | null;
}

/** A paid subscription (ADR-0147). Community, the free tier, has no key. */
export type LicenseTier = "basic" | "standard" | "premium" | "enterprise";

/** What a license key says (ADR-0100, ADR-0147). */
export interface License {
  /** 2, or 1 for a key of ADR-0100, which says no tier, managed servers or contact. */
  format?: number;
  id: string;
  customer: string;
  /** `enterprise` when it includes the Enterprise edition, `core` otherwise. */
  edition: string;
  /** Absent from servers older than ADR-0147, whose keys are all Enterprise. */
  tier?: LicenseTier;
  /** Managed servers covered; `null` for a format 1 key. */
  managed_servers?: number | null;
  /** Central servers allowed on the database, the first one included. */
  server_instances: number;
  /** An email address or an `https://` address. */
  support?: string | null;
  /** `YYYY-MM-DD`. */
  issued: string;
  /** Last valid day, `YYYY-MM-DD` (UTC). */
  expires: string;
}

export type LicenseState = "none" | "valid" | "expires_soon" | "expired" | "invalid";

/** `GET /api/license`: the Enterprise license key and where it stands. */
export interface LicenseView {
  /** Whether this build includes the Enterprise edition. Without it, nothing else is set. */
  available: boolean;
  state?: LicenseState;
  license?: License | null;
  /** Why an installed key no longer verifies. */
  problem?: string | null;
  installed_at?: string | null;
  installed_by?: string | null;
  /** Central servers added under a license, oldest first. */
  added?: AddedServer[];
  /** Managed servers now, whose agent is not revoked (ADR-0147). */
  managed_servers_in_use?: number;
}

/** A central server added with `install-server.sh --join` (ADR-0100). */
export interface AddedServer {
  /** The name of its host. */
  name: string;
  admitted_at: string;
  running: boolean;
}

/** `GET /api/backups`. */
export interface BackupsView {
  /** The updater service, which takes backups, is set up. */
  enabled: boolean;
  /** Files can be downloaded. */
  downloads: boolean;
  /** Backups are in `BACKUP_DIR`, readable by root alone: no downloads. */
  host_dir: boolean;
  updater_alive: boolean;
  /** "Back up now" waits for the updater. */
  requested: boolean;
  keep: number;
  last: BackupRun | null;
  last_success_at: string | null;
  next_at: string | null;
  /** Newest first. */
  files: BackupFile[];
}

/** A server or relay during a change of certificate authority (ADR-0088). */
export interface CaPeerState {
  /** `agent` is a server on screen. */
  role: "agent" | "relay";
  peer_id: string;
  name: string;
  last_seen_at: string | null;
  /** Its certificate expires then: it stops being waited for. */
  not_after: string;
  /** Its certificate comes from the authority that issues now. */
  moved: boolean;
  /** It trusts every authority the server trusts. */
  trusts_all: boolean;
  /** Its version cannot follow the change: it updates itself first. */
  too_old: boolean;
  /** Its certificate comes from an authority no longer trusted: it needs a new install. */
  cut_off: boolean;
}

/** A change of certificate authority under way. */
export interface CaRotation {
  stage: "introduced" | "switched";
  emergency: boolean;
  started_at: string;
  switched_at: string | null;
  /** Emergency only: when the old authority stops being trusted. */
  distrust_at: string | null;
  /** The new authority while introduced, the old one once switched. */
  other_fingerprint: string | null;
  /** Servers and relays with a valid certificate. */
  total: number;
  /** Those done with the current step. */
  done: number;
  /** Those not done, up to 50, last seen first. */
  waiting: CaPeerState[];
}

/** `GET /api/certificate-authority`; global administrators only (ADR-0088). */
export interface CertificateAuthorityView {
  /** SHA-256, hex. */
  fingerprint: string;
  created_at: string;
  expires_at: string;
  /** Less than a year left. */
  expires_soon: boolean;
  /** A planned change starts by itself then if nobody starts one. */
  auto_change_at: string;
  /** A restore brought back an authority this server retired then. */
  restored_retired_at: string | null;
  /** Machines that missed an emergency change, up to 50. */
  cut_off: CaPeerState[];
  rotation: CaRotation | null;
}

/** `POST /api/agent-releases/import` and `/import-url`. */
export interface ReleaseImported {
  version: string;
  /** Files of a whole release that were not given. */
  missing: string[];
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

export type RemediationState =
  | "open"
  | "running"
  | "awaiting_approval"
  | "resolved"
  | "escalated"
  | "failed";

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
  /** The AI's latest diagnosis, untrusted text (ADR-0175); absent from older servers. */
  diagnosis?: string | null;
  diagnosed_at?: string | null;
  /** What it said to do next with that diagnosis, untrusted text. */
  ai_advice?: string | null;
  /** The command it proposed with it; with `diagnosis_only`, a suggestion that never ran. */
  ai_command?: string | null;
  /** Why no command could run, so the AI only diagnosed: a refusal code, `device` or `offline`. */
  diagnosis_only?: string | null;
  /** Once closed, the code, English sentence, detail and limit of the event that closed it. */
  close_code?: string | null;
  close_reason?: string | null;
  close_detail?: string | null;
  close_limit?: number | null;
}

export interface RemediationIncidentList {
  incidents: RemediationIncident[];
  next_cursor: string | null;
}

/** Why AI remediation cannot run on an alert now (ADR-0165). */
export type RemediationRunBlocked =
  | "alert_resolved"
  | "no_agent"
  | "in_progress"
  | "ai_off"
  | "commands_refused"
  | "role"
  | "changed";

/** `GET /api/remediation/alerts/{id}`: an alert's attempts and whether the user may start one. */
export interface RemediationRunStatus {
  alert_incident_id: string;
  attempts: number;
  latest_id: string | null;
  latest_state: RemediationState | null;
  can_run: boolean;
  blocked: RemediationRunBlocked | null;
  /** `blocked` in English. */
  reason: string | null;
  /** When the server refuses AI commands: `commands_off`, `commands_off_windows`, `system_unsupported` or `agent_too_old`. */
  refusal: string | null;
  /** Why an attempt now would only diagnose (ADR-0175): a refusal code, `device` or `offline`. */
  diagnosis_only?: string | null;
  diagnosis_only_reason?: string | null;
  /** On an open alert without any remediation, why: `ai_off`, `too_old` or `soon`. */
  no_attempt?: "ai_off" | "too_old" | "soon" | null;
  no_attempt_reason?: string | null;
}

export interface RemediationRunStarted {
  id: string;
  alert_incident_id: string;
  attempt: number;
  previous_id: string | null;
  state: RemediationState;
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

/** Why a server cannot be snapshotted before a change (ADR-0044, ADR-0093). */
export type SnapshotUnsupported = "disabled" | "not_mapped" | "unknown_cluster" | "cannot_snapshot";

/** A Proxmox VM (`qemu`) or container (`lxc`). */
export type GuestKind = "qemu" | "lxc";

/** Whether a server is snapshotted before AI's changes. */
export interface SnapshotSupport {
  available: boolean;
  reason: SnapshotUnsupported | null;
  /** The mapped guest's kind; absent from servers before ADR-0093. */
  kind?: GuestKind | null;
  /** With `cannot_snapshot`: what Proxmox cannot snapshot, in words. */
  detail?: string | null;
  /** What a snapshot does not hold, like a bind mount. */
  left_out?: string | null;
  /** Which snapshot a change gets when available: `proxmox`, or a local `lvm` or `zfs` one; absent from servers before ADR-0094. */
  method?: "proxmox" | "lvm" | "zfs" | null;
  /** Why no local snapshot, as data to word in the active language; `detail` says it in English. */
  local_reason?: LocalReason;
}

/** Why a server cannot get a local snapshot (ADR-0094), with the values of its sentence. */
export interface LocalReason {
  code: "windows" | "not_reported" | "sudo_rules" | "inspect" | "no_root" | "root_not_covered" | "no_space" | "pool_full";
  fs_type?: string;
  mount_point?: string;
  group?: string;
  free_bytes?: number;
  needed_bytes?: number;
  pool_bytes?: number;
  error?: string;
}

/** `GET /api/servers/{id}/proxmox`: the server's Proxmox guest; customer administrators. */
export interface ProxmoxView {
  server_id: string;
  mapping: {
    cluster: string;
    node: string;
    vmid: number;
    kind: GuestKind;
    updated_by: string | null;
    updated_at: string;
    /** Null until Proxmox said whether it can snapshot the guest. */
    checked_at: string | null;
    cannot_snapshot: string | null;
    left_out: string | null;
    /** Null when mapped by hand, else what discovery matched (ADR-0093). */
    matched_by?: ProxmoxSignal | null;
    /** The guest's name, once discovery saw it. */
    name?: string | null;
  } | null;
  snapshots: SnapshotSupport;
  /** Clusters that serve the server; empty when Proxmox is off. */
  clusters: string[];
  /** Guests that match the server equally: the page asks which one. */
  candidates?: ProxmoxGuest[];
}

/** What matched a server to its guest: its SMBIOS UUID, a MAC address, or only its name. */
export type ProxmoxSignal = "uuid" | "mac" | "hostname";

/** A VM or container as the last discovery pass saw it. */
export interface ProxmoxGuest {
  cluster: string;
  node: string;
  vmid: number;
  kind: GuestKind;
  name: string;
  matched_by: ProxmoxSignal | null;
  /** Mapped to another server already. */
  taken: boolean;
}

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
  snapshots: SnapshotSupport;
  /** `accepted`, `off` or `unsupported`; null or absent is unknown. Approving is refused when off or unsupported (ADR-0076). */
  shell_acceptance?: string | null;
  /** What runs the command: `posix` (/bin/sh on Linux) or `powershell` (Windows PowerShell); absent from older servers (ADR-0084). */
  shell?: "posix" | "powershell";
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
  /** The address in `upstream_url` and `enroll_url`. */
  address: PublicAddress;
}

export type InstallKeyState = "active" | "expired" | "used_up" | "revoked";

/** One install key of `GET /api/install-keys`, never with its secret (ADR-0073). */
export interface InstallKey {
  id: string;
  role: TokenRole;
  site_id: string;
  site_name: string;
  customer_id: string;
  customer_name: string;
  created_by_username: string | null;
  created_at: string;
  expires_at: string;
  /** Null for no limit. */
  max_hosts: number | null;
  host_count: number;
  require_approval: boolean;
  revoked_at: string | null;
  state: InstallKeyState;
}

/** A host enrolled by a key that asks for approval, which cannot connect yet. */
export interface PendingHost {
  peer_id: string;
  hostname: string;
  site_id: string;
  site_name: string;
  customer_name: string;
  install_key_id: string;
  /** IP address it enrolled from. */
  source: string | null;
  created_at: string;
}

export interface InstallKeyList {
  keys: InstallKey[];
  pending_hosts: PendingHost[];
}

/** Body of `POST /api/install-keys`. */
export interface InstallKeyRequest {
  /** Agents when absent; a `relay` key works for one host, for a day (ADR-0066). */
  role?: TokenRole;
  site_id: string;
  /** One hour to one year; 30 days when absent. */
  ttl_s?: number;
  /** No limit when absent. */
  max_hosts?: number;
  require_approval: boolean;
}

/** `POST /api/install-keys`: the only answer that carries the key, in `token` as installers take it. */
export interface CreatedInstallKey extends InstallKey {
  token: string;
  upstream_url: string;
  enroll_url: string;
  install: AgentInstall;
  address: PublicAddress;
}

/** `GET /api/settings/public-address`, and the `address` of a new token. */
export interface PublicAddress {
  /** The host the install commands connect to, without a port. */
  address: string;
  /** The first server name, used when none is chosen. */
  default: string;
  /** The names the server's certificate is issued for. */
  names: string[];
  /** Whether the certificate covers `address`; agents refuse to connect otherwise. */
  certified: boolean;
  /** At `address`, with the published ports. */
  upstream_url: string;
  enroll_url: string;
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

/** A setting of `GET /api/settings/server`; `locked`: the server's configuration sets it. */
export interface Setting<T> {
  value: T;
  default: T;
  locked: boolean;
}

/** How long one store keeps data, as its flag says: `90d`. */
export interface KeptData {
  value: string;
  locked: boolean;
}

/** `retention` of the server settings; the updater applies changes (ADR-0069). */
export interface RetentionSettings {
  metrics: KeptData;
  logs: KeptData;
  default_metrics: string;
  default_logs: string;
  max_days: number;
  updater_alive: boolean;
  applying: boolean;
  error: string | null;
}

/** `GET /api/settings/server` (ADR-0069). */
export interface ServerSettings {
  /** Seconds. */
  session_lifetime: Setting<number>;
  session_idle_timeout: Setting<number>;
  mirror_public_url: Setting<string | null>;
  /** `null` without an updater reporting it. */
  retention: RetentionSettings | null;
}

export type CampaignState =
  | "draft"
  | "scheduled"
  | "running"
  | "paused"
  | "stopped"
  | "completed"
  | "failed";
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
  | {
      /** ADR-0167: a day of the month, or a weekday of a week of it, in `months`; one month is yearly. */
      kind: "monthly";
      /** 1 to 31; or null with `month_week` and `weekday`. */
      month_day?: number | null;
      /** 1 to 4, 5 for the last. */
      month_week?: number | null;
      /** ISO weekday. */
      weekday?: number | null;
      /** 1 is January; the API answers all twelve for every month. */
      months: number[];
      start: string;
      end: string;
      timezone: string;
    }
  | { kind: "once"; starts_at: string; ends_at: string };

/** Explicit servers, a number of servers, or a percentage of all targets (rounded up). */
export type WaveSpec =
  { servers: string[] } | { size: number } | { percent: number };

/** The `ApplyUpdates` every command of a campaign carries. */
export interface UpdateSpec {
  /** Empty upgrades everything. */
  packages: string[];
  security_only: boolean;
  allow_reboot: boolean;
  /** OS updates included (ADR-0138); absent from servers before it, which update only the OS. */
  include_os?: boolean;
  /** Third-party managers to update after the OS (ADR-0138). */
  third_party?: ThirdPartySelection[];
}

/** A third-party package manager (ADR-0138): winget on Windows, Flatpak and Snap on Linux. */
export type ThirdPartyManagerName = "winget" | "flatpak" | "snap";

/** What an update asks of one third-party manager; empty `packages` for every pending app. */
export interface ThirdPartySelection {
  manager: ThirdPartyManagerName;
  packages: string[];
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
  /** The request's `server_ids` and `waves` as last written; null before ADR-0166, absent from older servers. */
  server_ids?: string[] | null;
  wave_specs?: WaveSpec[] | null;
  wave_count: number;
  /** Index from 0; null until the first wave is queued. */
  current_wave: number | null;
  progress: CampaignProgress;
  /** How many earlier runs `GET /api/campaigns/{id}/runs` lists (ADR-0173); absent from older servers. */
  previous_runs?: number;
  created_by: string | null;
  created_at: string;
  started_at: string | null;
  finished_at: string | null;
}

/** The settings a past run ran with: its request (ADR-0173). */
export interface CampaignRunSettings {
  name: string;
  site_id: string | null;
  server_ids: string[];
  wave_specs: WaveSpec[];
  spec: UpdateSpec;
  agent_version: string | null;
  command_ttl_s: number;
  failure_threshold: FailureThreshold;
  windows: CampaignWindow[];
  sources: SourcesPin | null;
}

/** A past run of a campaign, as it ended (ADR-0173). */
export interface CampaignRun {
  /** 1 for the first run. */
  number: number;
  state: "stopped" | "completed" | "failed";
  error: string | null;
  started_at: string | null;
  finished_at: string;
  settings: CampaignRunSettings;
  progress: CampaignProgress;
  waves: CampaignWave[];
}

/** Which servers run again: all of them, or those that failed in the last run. */
export type CampaignRelaunch = "all" | "failed";

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
  /** The local snapshot taken before the update, if any (ADR-0094). */
  snapshot_command_id?: string | null;
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

/** Body of `POST /api/campaigns` and `PUT /api/campaigns/{id}`; a `site_id` or `server_ids` is required. */
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
  /** False: third-party apps only (ADR-0138). */
  include_os?: boolean;
  third_party?: ThirdPartySelection[];
  /** Mirror snapshots to pin (ADR-0038); the form keeps those of the campaign it edits. */
  sources?: SourcesPin;
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

/** A third-party manager of a server's latest report (ADR-0138). */
export interface ThirdPartyManager {
  manager: ThirdPartyManagerName;
  /** `unusable`: installed, but its apps could not be listed; see `error`. */
  state: "available" | "absent" | "unusable";
  version: string | null;
  error: string | null;
  installed: number;
  pending: number;
}

/** An app a third-party manager installed. */
export interface ThirdPartyPackage {
  manager: ThirdPartyManagerName;
  /** What the manager's update takes: winget ID, Flatpak ref, snap name. */
  id: string;
  name: string | null;
  version: string;
  /** Null when no update is pending. */
  available_version: string | null;
  /** Updates touch `machine` installs only. */
  scope: "machine" | "user";
  origin: string | null;
}

/** `GET /api/servers/{id}/third-party`. */
export interface ThirdPartyList {
  /** False until the agent reports third-party managers. */
  reported: boolean;
  managers: ThirdPartyManager[];
  packages: ThirdPartyPackage[];
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
  /** What the next sync downloads (ADR-0142). */
  mode: MirrorMode;
  created_at: string;
  /** Set while the mirror is being removed (ADR-0075). */
  deleting_at: string | null;
  /** Why removing it has not finished yet. */
  delete_error: string | null;
  /** Name of the newest published snapshot. */
  current_snapshot: string | null;
  last_run: MirrorRunSummary | null;
}

export interface MirrorList {
  mirrors: Mirror[];
  /** Published snapshots each mirror keeps, on top of those in use. */
  keep_snapshots: number;
}

/** `full` copies every package; `on_demand` only the indexes, packages being fetched when first asked for (ADR-0142). */
export type MirrorMode = "full" | "on_demand";

/** One distribution of `GET /api/mirrors/catalog` (ADR-0067). */
export interface MirrorDistribution {
  id: string;
  name: string;
  family: "apt" | "dnf";
  /** In full mode. */
  size_bytes: number;
  /** On demand: indexes and the packages a fleet typically installs (ADR-0142). */
  on_demand_size_bytes: number;
  /** What on demand needs at least. */
  index_bytes: number;
  added: boolean;
  /** Why it cannot be added in full mode now, in an English sentence. */
  problem: string | null;
  /** Why it cannot be added on demand now, in an English sentence. */
  on_demand_problem: string | null;
}

/** `GET /api/mirrors/cache`: the storage cap of on-demand mirrors (ADR-0142). */
export interface MirrorCache {
  cap_bytes: number;
  default_cap_bytes: number;
  cap_set: boolean;
  used_bytes: number;
  index_bytes: number;
  package_bytes: number;
  packages: number;
  /** The last eviction could not get under the cap. */
  over_cap: boolean;
}

export interface MirrorCatalog {
  tools: { available: boolean; problem: string | null };
  disk: { free_bytes: number; total_bytes: number } | null;
  distributions: MirrorDistribution[];
}

export interface MirrorSnapshot {
  id: string;
  mirror_id: string;
  name: string;
  /** `pruning` while retention removes it; `pruned` ones are not listed. */
  state: "creating" | "published" | "failed" | "pruning" | "pruned";
  size_bytes: number | null;
  error: string | null;
  created_at: string;
  published_at: string | null;
  /** Why it stays; null when the next cleanup removes it (ADR-0074). */
  kept: "current" | "campaign" | "server" | "relay" | "newest" | null;
}

/** `GET /api/mirrors/{id}`: the newest 100 snapshots and 10 runs first. */
export interface MirrorDetail extends Mirror {
  snapshots: MirrorSnapshot[];
  keep_snapshots: number;
  runs: MirrorRun[];
  /** What keeps it from being removed; relays are not here, their copy goes with it. */
  used_by: {
    campaigns: { id: string; name: string }[];
    servers: { id: string; hostname: string }[];
  };
}

/** One volume of a local snapshot (ADR-0094). */
export interface LocalSnapshotVolume {
  name: string;
  mount_point: string;
  kind: "lvm" | "lvm_thin" | "zfs";
  /** `vg/lv` or `dataset@name`. */
  source: string;
  origin: string;
  created_at: string;
  size_bytes: number;
  used_percent: number;
  invalid: boolean;
}

/** `GET /api/servers/{id}/rollback` (ADR-0094). */
export interface Rollback {
  server_id: string;
  /** What a change gets first, as the approval queue says it (ADR-0094). */
  snapshots: SnapshotSupport;
  /** System mount points a local snapshot leaves out. */
  not_covered: string[];
  /** MonitorFlow's local snapshots on the server, oldest first. */
  local_snapshots: {
    name: string;
    kind: "lvm" | "lvm_thin" | "zfs";
    created_at: string;
    delete_after: string;
    volumes: LocalSnapshotVolume[];
  }[];
  reported_at: string | null;
}

/** `GET /api/servers/{id}/sources` (ADR-0074). */
/** Why a server cannot use a mirror (ADR-0074). */
export type SourcesProblem = "no_package_updates" | "windows" | "no_distribution" | "no_catalog" | "not_enabled" | "no_mirror" | "copying";

export interface SourcesStatus {
  using: "mirror" | "own";
  mirrors: { name: string; snapshot: string }[];
  /** A change on its way to the server. */
  changing_to: "mirror" | "own" | null;
  /** Why the last change failed, when nothing succeeded or started since. */
  failed: string | null;
  /** An update campaign has the sources for now. */
  campaign: boolean;
  /** The catalog's name for what the server runs. */
  distribution: string | null;
  /** Why the server cannot use a mirror, in a sentence. */
  problem: string | null;
  /** The fix is adding `distribution` on the Mirrors page. */
  add_mirror: boolean;
  /** `problem` as a code, worded from the translations; absent from older servers. */
  problem_code?: SourcesProblem;
  /**
   * Windows servers: where Windows Update takes updates from (ADR-0086).
   * `null` on Linux and until an agent that reports it sent its updates.
   */
  windows_update_source: WindowsUpdateSource | null;
}

/** Where a Windows server takes its updates from, as its policy sets it. */
export interface WindowsUpdateSource {
  /** `wsus_without_url`: policy turns WSUS on without naming a server. */
  origin: "microsoft" | "wsus" | "wsus_without_url";
  /** With `microsoft`, a server policy names but doesn't turn on. */
  wsus_url: string | null;
  /** Where it reports to, when not the WSUS server. */
  status_url: string | null;
  /** Where downloads come from, when not the WSUS server. */
  download_url: string | null;
  /** Any Windows Update policy is set. */
  managed: boolean;
  /** `null` when policy leaves it to Windows. */
  automatic: "off" | "notify" | "download" | "install" | "local_choice" | null;
  /** With WSUS: policy forbids Windows Update Internet locations. */
  no_internet: boolean;
  /** With WSUS: these come from Microsoft instead, by policy. */
  quality_from_microsoft: boolean;
  feature_from_microsoft: boolean;
  /** Deferrals of updates from Microsoft, in days; 0 for none. */
  quality_deferral_days: number;
  feature_deferral_days: number;
}

/** `GET /api/sites/{id}/sources`, by host name. */
export interface SiteSourcesList {
  servers: (SourcesStatus & { id: string; hostname: string; os_family: string })[];
}

/** What a change of a whole site did. */
export interface SiteSourcesChange {
  queued: string[];
  skipped: { id: string; hostname: string; reason: string }[];
}

export interface Health {
  status: "ok";
}

/** `GET /api/setup` (ADR-0071). */
export interface SetupStatus {
  /** No account exists yet: sign-in cannot work until the setup link is used. */
  pending: boolean;
}

/** One account of `GET /api/users` (ADR-0059). */
export interface UserAccount {
  id: string;
  username: string;
  /** `invited`: has not chosen a password with their link yet. */
  state: "active" | "invited" | "disabled";
  created_at: string;
  roles: RoleGrant[];
  /** Expiry of the user's unused password link, if any. */
  link_expires_at: string | null;
  /** The sign-in provider whose groups give the roles; null for a password kept here (ADR-0145). */
  provider?: AccountProvider | null;
}

export interface UserList {
  users: UserAccount[];
}

/** A new password link; its token is only returned once. */
export interface PasswordLink {
  token: string;
  expires_at: string;
}

/** A role and where it applies; no customer and no site is everything. */
export interface GrantRequest {
  role: Role;
  customer_id?: string;
  site_id?: string;
}

/** `POST /api/password-link`: whose link it is. */
export interface LinkOwner {
  username: string;
  /** The user never had a password: the link is an invitation. */
  invited: boolean;
}

/** Page where a password link is used. */
export const WELCOME_PATH = "/welcome/";

/** The address to give a user for `link`, on this page's origin. */
export function passwordLinkUrl(link: PasswordLink): string {
  return `${window.location.origin}${WELCOME_PATH}#${link.token}`;
}

/** Shortest password the server accepts. */
export const MIN_PASSWORD_LENGTH = 12;

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

export interface RequestOptions {
  method?: "GET" | "POST" | "PUT" | "PATCH" | "DELETE";
  body?: unknown;
  /** Sent as multipart, instead of `body`; the browser sets the content type. */
  form?: FormData;
  /** Sent as it is, instead of `body`: an uploaded file. */
  file?: Blob;
  signal?: AbortSignal;
  /** Call the unauthorized handler on 401. Off for login, where 401 means bad credentials. */
  redirectOn401?: boolean;
  /** Lets the request outlive the page, for a save sent as it closes. */
  keepalive?: boolean;
}

/** The main sidebar as the person arranged it (ADR-0137); item identifiers are `NavItem.id`. */
export interface SidebarPreferences {
  /** Icons only, without labels. */
  collapsed?: boolean;
  /** Items the person does not use. */
  hidden?: string[];
  /** Items in the person's order, within their section. */
  order?: string[];
}

/** `GET /api/me/preferences`: the signed-in person's own; a missing key is the default. */
export interface Preferences {
  theme?: "system" | "light" | "dark";
  language?: "en" | "fr";
  sidebar?: SidebarPreferences;
}

/** SMTP settings of an email channel; the password is never sent back (ADR-0060). */
export interface EmailSettings {
  host: string;
  port: number;
  /** Empty without authentication. */
  username: string;
  /** Empty: the username is the sender. */
  from: string;
  to: string[];
}

/** What a webhook channel sends to, in a request; on edit an empty `url` keeps the stored one. */
export interface WebhookSettings {
  url: string;
}

/** A webhook channel as shown: its URL's path and query may hold a token, so only scheme, host and port (ADR-0123). */
export interface WebhookView {
  origin: string;
}

/** A Slack, Teams or Discord channel; its webhook URL is the secret, never sent back (ADR-0077). */
export interface ChatSettings {
  /** Tells channels of one service apart; may be empty. */
  name: string;
  /** Host of the webhook URL. */
  host: string;
}

export type ChatKind = "slack" | "teams" | "discord";

/** One channel of `GET /api/notifications/channels`: those of what the user administers (ADR-0078). */
export type NotificationChannel = (
  | { kind: "email"; settings: EmailSettings }
  | { kind: "webhook"; settings: WebhookView }
  | { kind: ChatKind; settings: ChatSettings }
  | { kind: "plugin"; settings: PluginChannelSettings }
) & {
  id: string;
  /** Whether a password, signing secret or chat webhook URL is stored; it never leaves the server. */
  has_secret: boolean;
  /** The customer it covers, or the site's; null with `site_id` too: everything. */
  customer_id: string | null;
  customer_name: string | null;
  /** The site it covers; null for a whole customer or everything. */
  site_id: string | null;
  site_name: string | null;
  created_at: string;
  last_sent_at: string | null;
  /** The last delivery's or test's error in plain words; null once one works. */
  last_error: string | null;
  last_error_at: string | null;
  /** Hears only follow-up steps that name it, and their resolution (ADR-0082). */
  follow_up_only?: boolean;
};

export interface NotificationChannelList {
  channels: NotificationChannel[];
}

/** Body of creating or editing a channel; on edit an empty `secret` keeps the stored one. */
export type NotificationChannelRequest = (
  | {
      kind: "email";
      settings: Partial<EmailSettings> & Pick<EmailSettings, "host" | "to">;
    }
  | { kind: "webhook"; settings: WebhookSettings }
  | { kind: ChatKind; settings: Pick<ChatSettings, "name"> }
  | { kind: "plugin"; settings: PluginChannelSettings }
) & {
  /** For a plugin, its secret settings as a JSON object. */
  secret?: string | null;
  customer_id: string | null;
  site_id: string | null;
  /** Only follow-up steps that name it, not every event it covers. */
  follow_up_only?: boolean;
};

/** One integration of `GET /api/integrations`: where signed events go (ADR-0102). Never its secret. */
export interface IntegrationEndpoint {
  id: string;
  name: string;
  /** The URL's scheme, host and port: its path and query may hold a token (ADR-0123). */
  origin: string;
  /** The events it wants, among `IntegrationList.events`. */
  events: string[];
  /** The customer it covers, or the site's; null with `site_id` too: everything. */
  customer_id: string | null;
  customer_name: string | null;
  site_id: string | null;
  site_name: string | null;
  /** Delivers to private addresses; set by a global administrator. */
  allow_private: boolean;
  /** `disabled`: turned off after a day without a success. */
  state: "on" | "paused" | "disabled";
  created_at: string;
  /** The first failure since the last success; null while it works. */
  failing_since: string | null;
  last_success_at: string | null;
  /** Deliveries waiting to be sent or tried again. */
  pending: number;
  last_error: string | null;
}

export interface IntegrationList {
  endpoints: IntegrationEndpoint[];
  /** Every event an integration can ask for, in order. */
  events: string[];
}

export interface IntegrationRequest {
  name: string;
  /** On edit, empty keeps the stored one. */
  url: string;
  /** Empty: every event. */
  events: string[];
  customer_id: string | null;
  site_id: string | null;
  allow_private: boolean;
}

/** An integration with its signing secret: only when created or given a new secret. */
export interface IntegrationWithSecret {
  endpoint: IntegrationEndpoint;
  secret: string;
}

/** Body of `PUT /api/integrations/{id}`: a new secret, shown once, only when the URL changed. */
export interface IntegrationUpdated {
  endpoint: IntegrationEndpoint;
  secret: string | null;
}

export interface IntegrationTestResult {
  ok: boolean;
  /** The receiver's HTTP status, when it answered. */
  status: number | null;
  error: string | null;
}

/** One delivery of an integration's log, newest first. */
export interface IntegrationDelivery {
  id: number;
  event_id: string;
  event: string;
  occurred_at: string;
  state: "pending" | "delivered" | "failed";
  /** Sent again by an administrator. */
  replay: boolean;
  attempts: number;
  last_status: number | null;
  last_error: string | null;
  last_attempt_at: string | null;
  /** When the next try is due, while pending. */
  next_at: string | null;
  finished_at: string | null;
}

export interface IntegrationDeliveryList {
  deliveries: IntegrationDelivery[];
  /** Pass as `before` for older ones; null at the end. */
  next_before: number | null;
}

/** One event by its ID, or every event from `from` to `to` (now when absent). */
export type IntegrationReplay = { event_id: string } | { from: string; to?: string };

/** The PSA connection of `GET /api/psa/connection` (ADR-0164, ADR-0166). Never its secret. */
export interface PsaConnection {
  kind: "halopsa";
  base_url: string;
  tenant: string | null;
  client_id: string;
  /** Why the saved secret does not open; null when it does. */
  secret_error: string | null;
  enabled: boolean;
  /** The PSA agent the application acts as, from the last success. */
  agent_name: string | null;
  last_success_at: string | null;
  last_error: string | null;
  last_error_at: string | null;
  created_at: string;
  updated_at: string;
  tickets: PsaTicketSettings;
}

/** What tickets the PSA connection opens and how it closes them (ADR-0166). */
export interface PsaTicketSettings {
  ticket_type_id: number | null;
  ticket_type_name: string | null;
  category: string | null;
  min_severity: "warning" | "critical";
  /** Null adds a note only. */
  resolved_status_id: number | null;
  resolved_status_name: string | null;
}

export interface PsaTicketSettingsRequest {
  ticket_type_id: number | null;
  category: string | null;
  min_severity: "warning" | "critical";
  resolved_status_id: number | null;
}

/** One incident's ticket in the PSA. */
export interface PsaTicket {
  incident_id: string;
  ticket_id: number | null;
  state: "creating" | "open" | "closed" | "skipped" | "failed";
  error: string | null;
  metric: string;
  severity: string;
  server_name: string | null;
  site_name: string | null;
  created_at: string;
  updated_at: string;
  closed_at: string | null;
}

export interface PsaConnectionState {
  connection: PsaConnection | null;
}

/** Body of `PUT /api/psa/connection` and of its test; an empty secret keeps the saved one for the same address. */
export interface PsaConnectionRequest {
  base_url?: string;
  tenant?: string | null;
  client_id?: string;
  client_secret?: string;
  enabled?: boolean;
}

/** A client or site of the PSA, as its lists give it. */
export interface PsaRecord {
  id: number;
  name: string;
}

export interface PsaRecordList {
  records: PsaRecord[];
}

/** A MonitorFlow site and the PSA client and site it is matched to; nulls when not matched. */
export interface PsaSiteLink {
  site_id: string;
  site_name: string;
  customer_id: string;
  customer_name: string;
  client_id: number | null;
  client_name: string | null;
  psa_site_id: number | null;
  psa_site_name: string | null;
  linked_at: string | null;
}

export interface PsaSiteLinkList {
  sites: PsaSiteLink[];
}

export interface PsaConnectionTest {
  ok: boolean;
  agent_name: string | null;
  error: string | null;
}

/** A channel run by a plugin (ADR-0103); its secret settings never come back. */
export interface PluginChannelSettings {
  plugin: string;
  name: string;
  values: Record<string, string>;
}

/** One field of a plugin's settings form. */
export interface PluginSetting {
  key: string;
  label: string;
  type: "text" | "url" | "number" | "bool" | "secret";
  required?: boolean;
  default?: string;
  help?: string;
}

export type PluginKind = "check" | "channel";

/** Where a plugin may be used: everything when both IDs are null. */
export interface PluginPlace {
  customer_id: string | null;
  customer_name: string | null;
  site_id: string | null;
  site_name: string | null;
}

/** An installed plugin, for global administrators (ADR-0103). */
export interface InstalledPlugin {
  id: string;
  name: string;
  kind: PluginKind;
  version: string;
  title: string;
  description: string;
  official: boolean;
  /** MonitorFlow, the publisher's name, or null for a key no longer trusted. */
  publisher: string | null;
  fingerprint: string;
  /** What it does and asks for, in words. */
  capabilities: string[];
  settings: PluginSetting[];
  enabled: boolean;
  places: PluginPlace[];
  channels: number;
  checks: number;
  installed_at: string;
  updated_at: string;
}

/** What an uploaded plugin file is, before it is installed. */
export interface PluginInspection {
  name: string;
  kind: PluginKind;
  version: string;
  title: string;
  description: string;
  official: boolean;
  publisher: string;
  fingerprint: string;
  capabilities: string[];
  settings: PluginSetting[];
  sha256: string;
  update: { from_version: string; adds: string[]; drops: string[] } | null;
}

/** A plugin turned on where the user can see, for the channel and check forms. */
/** How many servers run one check (`GET /api/checks/usage`). */
export interface CheckUsage {
  kind: string;
  name: string;
  plugin: string | null;
  servers: number;
}

export interface UsablePlugin {
  name: string;
  kind: PluginKind;
  version: string;
  title: string;
  description: string;
  capabilities: string[];
  settings: PluginSetting[];
  everywhere: boolean;
  customer_ids: string[];
  site_ids: string[];
}

export interface PluginPublisher {
  id: string;
  name: string;
  public_key: string;
  fingerprint: string;
  /** Installed plugins it signed. */
  plugins: string[];
  created_at: string;
}

export interface PluginPublisherList {
  publishers: PluginPublisher[];
  /** Fingerprints of MonitorFlow's own keys. */
  official: string[];
}

/** Why the person on call cannot be told: that period is a gap (ADR-0081). */
export type OnCallProblem = "nobody" | "removed" | "disabled" | "no_access" | "no_contact" | "no_mail_server";

/** One period with one person on call. */
export interface OnCallTurn {
  /** The place in the turn, 0 for the first person (ADR-0170). */
  seat: number;
  starts_at: string;
  ends_at: string;
  /** Null for a removed user's turn or a schedule without people. */
  user_id: string | null;
  username: string | null;
  /** Set when someone covers instead of the rotation. */
  override_id: string | null;
  problem: OnCallProblem | null;
}

export interface OnCallOverride {
  id: string;
  user_id: string;
  /** The place of the turn it covers. */
  seat: number;
  username: string | null;
  starts_at: string;
  ends_at: string;
}

/** A schedule of `GET /api/on-call/schedules`. */
export interface OnCallSchedule {
  id: string;
  name: string;
  /** The customer it covers, or the site's; null with `site_id` too: everything. */
  customer_id: string | null;
  customer_name: string | null;
  site_id: string | null;
  site_name: string | null;
  /** An IANA name, like Europe/Paris: hand-offs happen in it. */
  time_zone: string;
  rotation: "daily" | "weekly";
  /** "09:00", in `time_zone`. */
  handoff_time: string;
  /** 1 for Monday to 7 for Sunday; null when daily. */
  handoff_day: number | null;
  members: { user_id: string | null; username: string | null; problem: OnCallProblem | null }[];
  /** How many people are on call at once, 1 to 5. */
  people_per_turn: number;
  /** Told only when a follow-up step names it (ADR-0171). */
  follow_up_only: boolean;
  overrides: OnCallOverride[];
  now: OnCallTurn | null;
  next: OnCallTurn | null;
  /** Everyone on call now, one per place. */
  now_all: OnCallTurn[];
  /** Who takes each place next. */
  next_all: OnCallTurn[];
  /** From now to four weeks ahead. */
  turns: OnCallTurn[];
  /** Who is told during a gap; empty when nobody can be. Empty too for members who do not manage it. */
  fallback: string[];
  /** The mail server email to people goes through; null when none. */
  mail_host: string | null;
  can_manage: boolean;
}

export interface OnCallScheduleRequest {
  name: string;
  customer_id: string | null;
  site_id: string | null;
  time_zone: string;
  rotation: "daily" | "weekly";
  handoff_time: string;
  handoff_day: number | null;
  /** In order of their turns. */
  members: string[];
  /** 1 to 5, at most the number of members. */
  people_per_turn: number;
  /** Told only when a follow-up step names it. */
  follow_up_only: boolean;
}

/** Someone the user may put on a schedule. */
export interface OnCallPerson {
  id: string;
  username: string;
  has_contact: boolean;
}

/** The signed-in user's own contact; the chat URL never leaves the server. */
export interface Contact {
  email: string | null;
  kind: "webhook" | ChatKind | null;
  settings: { name?: string; host?: string; url?: string } | null;
  has_secret: boolean;
  last_sent_at: string | null;
  last_error: string | null;
  last_error_at: string | null;
}

export interface ContactRequest {
  email: string | null;
  kind: "webhook" | ChatKind | null;
  settings: { name?: string; url?: string } | null;
  /** Empty keeps the stored one. */
  secret?: string | null;
}

/** Result of "Send test": the error says why in plain words. */
export interface ChannelTestResult {
  ok: boolean;
  error: string | null;
}

/** A model offered for AI, the default first (ADR-0064). */
export interface AiModel {
  id: string;
  name: string;
  /** When to pick it, in English. */
  note: string;
  /** `note` as a stable code: `recommended`, `half_price` or `cheapest`; absent from older servers. */
  fit?: string;
}

/** Who answers (ADR-0096): Anthropic, OpenAI, or any OpenAI-compatible server, local models included. */
export type AiProvider = "anthropic" | "openai" | "openai_compatible";

/** `GET /api/settings/ai`, readable by every user; the key itself never leaves the server. */
export interface AiSettings {
  enabled: boolean;
  /** Set by `[ai]` in the server's configuration, which takes precedence and locks the page. */
  from_config: boolean;
  provider: AiProvider;
  has_key: boolean;
  /** The address of an OpenAI-compatible server. */
  base_url: string | null;
  model: string;
  /** Anthropic's short list. */
  models: AiModel[];
  /** The language administrators chose for the AI's diagnoses; null follows the people (ADR-0181). Absent from older servers. */
  language?: "en" | "fr" | null;
  /** The language of the next diagnosis. */
  effective_language?: "en" | "fr";
}

/** What the AI form holds; an empty key or address uses the stored one of the same provider. */
export interface AiForm {
  provider?: AiProvider;
  api_key: string | null;
  base_url?: string | null;
  model: string;
}

/**
 * What a write answers in the static demo (docs/demo.md): an `ApiError`
 * with this message, which `describeError` shows in the reader's language.
 */
export const DEMO_REFUSAL = "This is a demo: changes are not saved.";

/** What made something down for a while in an availability report (ADR-0155). */
export interface AvailabilityCause {
  kind: "check" | "offline" | "unreachable" | "member";
  server_id: string;
  server_name: string;
  check: string | null;
  instance: string;
  /** For `unreachable`: what it is reached through, like a relay. */
  via: string | null;
}

export interface AvailabilityOutage {
  start: string;
  end: string;
  seconds: number;
  ongoing: boolean;
  causes: AvailabilityCause[];
}

export interface AvailabilityNumbers {
  /** Percent of the counted time that was up; null when nothing was counted. */
  availability: number | null;
  counted_seconds: number;
  down_seconds: number;
  maintenance_seconds: number;
  /** Time the central server was not running, left out of the count (ADR-0176). */
  unobserved_seconds: number;
  outages: number;
  longest_seconds: number;
}

export interface AvailabilityService {
  id: string;
  name: string;
  customer_id: string;
  rule: "all" | "at_least";
  at_least: number | null;
  target: number | null;
  met: boolean | null;
  budget_seconds: number | null;
  members: { server_id: string; server_name: string; check: SilencedCheck | null }[];
  numbers: AvailabilityNumbers;
  outage_list: AvailabilityOutage[];
}

export interface AvailabilityMachine {
  id: string;
  name: string;
  device: boolean;
  site_id: string;
  site_name: string;
  customer_id: string;
  numbers: AvailabilityNumbers;
  outage_list: AvailabilityOutage[];
}

export interface AvailabilityCheck {
  server_id: string;
  server_name: string;
  name: string;
  instance: string;
  numbers: AvailabilityNumbers;
  outage_list: AvailabilityOutage[];
}

export interface AvailabilityMaintenance {
  start: string;
  end: string;
  reason: string;
  servers: string[];
  checks: SilencedCheck[];
  window: boolean;
}

/** `GET /api/availability`: availability over a period (ADR-0155). */
export interface AvailabilityReport {
  customer_id: string | null;
  customer_name: string | null;
  from: string;
  to: string;
  generated_at: string;
  summary: {
    services: number;
    services_met: number;
    services_missed: number;
    machines: number;
    outages: number;
    down_seconds: number;
    maintenance_seconds: number;
  };
  services: AvailabilityService[];
  machines: AvailabilityMachine[];
  checks: AvailabilityCheck[];
  maintenance: AvailabilityMaintenance[];
}

/** A business service: a group with one availability (ADR-0155). */
export interface BusinessService {
  id: string;
  customer_id: string;
  customer_name: string;
  name: string;
  description: string;
  rule: "all" | "at_least";
  at_least: number | null;
  target: number | null;
  members: { server_id: string; server_name: string; device: boolean; check: SilencedCheck | null }[];
  created_at: string;
  updated_at: string;
  can_change: boolean;
}

export interface BusinessServiceRequest {
  customer_id?: string | null;
  name: string;
  description: string;
  rule: "all" | "at_least";
  at_least: number | null;
  target: number | null;
  members: { server_id: string; check: SilencedCheck | null }[];
}

/** An availability report sent by email on a schedule (ADR-0155). */
export interface ReportSchedule {
  id: string;
  customer_id: string | null;
  customer_name: string | null;
  frequency: "monthly" | "weekly";
  time_zone: string;
  language: "en" | "fr";
  pdf: boolean;
  csv: boolean;
  channel_id: string;
  channel_host: string;
  recipients: string[];
  channel_recipients: string[];
  enabled: boolean;
  last_period_end: string | null;
  last_sent_at: string | null;
  last_error: string | null;
  last_error_at: string | null;
  next_from: string;
  next_to: string;
  next_send_at: string;
  created_at: string;
}

export interface ReportScheduleRequest {
  customer_id: string | null;
  frequency: "monthly" | "weekly";
  time_zone: string;
  language: "en" | "fr";
  pdf: boolean;
  csv: boolean;
  channel_id: string;
  recipients: string[];
  enabled: boolean;
}

/** Answer of sending a scheduled report now. */
export interface ReportSent {
  ok: boolean;
  error: string | null;
  from: string;
  to: string;
  recipients: string[];
}

/** One day of uptime (ADR-0172). */
export interface UptimeDay {
  /** Local date, "2026-11-22". */
  date: string;
  /** Percent; null when nothing was counted. */
  availability: number | null;
  down_seconds: number;
  maintenance_seconds: number;
}

/** One service, server, device or check with its windows and daily bars. */
export interface UptimeRow {
  kind: "service" | "server" | "device" | "check";
  id: string;
  name: string;
  check: string | null;
  site_name: string | null;
  /** Percent over the last 24 hours, 7, 30 and 90 days; null when not measured. */
  windows: { day: number | null; week: number | null; month: number | null; quarter: number | null };
  outages: number;
  /** 90 days, the oldest first, today so far last. */
  days: UptimeDay[];
}

/** `GET /api/availability/uptime`. */
export interface Uptime {
  at: string;
  time_zone: string;
  rows: UptimeRow[];
}

/** The query of an availability report: a period and, for an MSP, a customer. */
export interface AvailabilityQuery {
  from: string;
  to: string;
  customer_id?: string | null;
}

function availabilityQuery(query: AvailabilityQuery, extra: Record<string, string> = {}): string {
  const params = new URLSearchParams({ from: query.from, to: query.to, ...extra });
  if (query.customer_id) params.set("customer_id", query.customer_id);
  return params.toString();
}

/** Where the report's file is downloaded from, in `language` and `zone`. */
export function availabilityFileHref(query: AvailabilityQuery, format: "pdf" | "csv", language: string, zone: string): string {
  return `/api/availability/report?${availabilityQuery(query, { format, language, time_zone: zone })}`;
}

async function request<T>(
  path: string,
  options: RequestOptions = {},
): Promise<T> {
  // Replaced at build time: without the flag, the branch and the made-up
  // data it loads are left out of the bundle.
  if (process.env.NEXT_PUBLIC_MONITORFLOW_DEMO === "1") {
    const { demoRequest } = await import("./demo/handler");
    return demoRequest<T>(path, options);
  }
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
      body:
        options.form ??
        options.file ??
        (options.body === undefined ? undefined : JSON.stringify(options.body)),
      credentials: "same-origin",
      cache: "no-store",
      signal: options.signal,
      ...(options.keepalive && { keepalive: true }),
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
    if (
      body &&
      typeof body === "object" &&
      "error" in body &&
      typeof body.error === "string"
    ) {
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

/** The attempts of AI remediation an incident's page lists. */
export const MAX_ATTEMPTS_SHOWN = 20;

/** A short English sentence for showing `error` to the user. */
export function describeError(error: unknown): string {
  if (!(error instanceof ApiError)) {
    return t("common.error.unknown");
  }
  if (error.message === DEMO_REFUSAL) {
    return t("common.error.demo");
  }
  switch (error.status) {
    case 0:
      return t("common.error.unreachable");
    case 401:
      return t("common.error.signedOut");
    case 403:
      return t("common.error.forbidden");
    default:
      // The server's own message stays in English (ADR-0136).
      return error.status >= 500 ? t("common.error.server") : t("common.error.failed", { message: error.message });
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

  setupStatus: (signal?: AbortSignal) =>
    request<SetupStatus>("/api/setup", { signal, redirectOn401: false }),

  /** Creates the first administrator with the setup link's `token`, and signs it in. */
  setup: (token: string, username: string, password: string) =>
    request<Me>("/api/setup", {
      method: "POST",
      body: { token, username, password },
      redirectOn401: false,
    }),

  me: (signal?: AbortSignal) => request<Me>("/api/auth/me", { signal }),

  preferences: (signal?: AbortSignal) => request<Preferences>("/api/me/preferences", { signal }),

  /**
   * Changes the signed-in person's own password (ADR-0141). Ends their other
   * sessions and keeps this one; a wrong current password is a 400.
   */
  changePassword: (currentPassword: string, newPassword: string) =>
    request<void>("/api/me/password", {
      method: "POST",
      body: { current_password: currentPassword, new_password: newPassword },
    }),
  /** Replaces the signed-in person's preferences with `preferences`. */
  setPreferences: (preferences: Preferences, options: { keepalive?: boolean } = {}) =>
    request<Preferences>("/api/me/preferences", { method: "PUT", body: preferences, ...options }),

  /** One page of servers, from `cursor` when given. */
  serversPage: (cursor?: string, signal?: AbortSignal) =>
    request<ServerList>(
      cursor === undefined
        ? "/api/servers"
        : `/api/servers?cursor=${encodeURIComponent(cursor)}`,
      { signal },
    ),

  /** The first 500 servers of one site, by hostname. */
  siteServers: (siteId: string, signal?: AbortSignal) =>
    request<ServerList>(`/api/servers?site_id=${encodeURIComponent(siteId)}&limit=500`, { signal }),

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

  /** Servers per state in `scope`, for the status strip (ADR-0130). */
  serverSummary: (scope: { customerId: string | null; siteId: string | null }, signal?: AbortSignal) => {
    const query = scope.siteId
      ? `?site_id=${encodeURIComponent(scope.siteId)}`
      : scope.customerId
        ? `?customer_id=${encodeURIComponent(scope.customerId)}`
        : "";
    return request<ServerSummary>(`/api/servers/summary${query}`, { signal });
  },

  /** One server with its certificate; 404 when missing or outside the user's sites. */
  server: (id: string, signal?: AbortSignal) =>
    request<ServerDetail>(`/api/servers/${encodeURIComponent(id)}`, { signal }),

  /** The last run of each website check of the server (ADR-0150). */
  websiteChecks: (serverId: string, signal?: AbortSignal) =>
    request<WebsiteCheckList>(`/api/servers/${encodeURIComponent(serverId)}/website-checks`, { signal }),

  sensorValues: (serverId: string, signal?: AbortSignal) =>
    request<SensorValueList>(`/api/servers/${encodeURIComponent(serverId)}/sensor-values`, { signal }),

  serverSensors: (serverId: string, signal?: AbortSignal) =>
    request<EffectiveSensors>(
      `/api/servers/${encodeURIComponent(serverId)}/sensors`,
      { signal },
    ),

  /** The server's whole package report, following `next_cursor` across pages. */
  serverPackages: async (
    serverId: string,
    signal?: AbortSignal,
  ): Promise<PackageList> => {
    const base = `/api/servers/${encodeURIComponent(serverId)}/packages?limit=5000`;
    const packages: Package[] = [];
    let cursor: string | undefined;
    let page: PackageList;
    do {
      page = await request<PackageList>(
        cursor === undefined
          ? base
          : `${base}&cursor=${encodeURIComponent(cursor)}`,
        {
          signal,
        },
      );
      packages.push(...page.packages);
      cursor = page.next_cursor ?? undefined;
    } while (cursor !== undefined);
    return { ...page, packages, next_cursor: null };
  },

  /** The server's third-party managers and apps (ADR-0138), following `next_cursor` across pages. */
  serverThirdParty: async (serverId: string, signal?: AbortSignal): Promise<ThirdPartyList> => {
    const base = `/api/servers/${encodeURIComponent(serverId)}/third-party?limit=5000`;
    const packages: ThirdPartyPackage[] = [];
    let cursor: string | undefined;
    let page: ThirdPartyList;
    do {
      page = await request<ThirdPartyList>(
        cursor === undefined ? base : `${base}&cursor=${encodeURIComponent(cursor)}`,
        { signal },
      );
      packages.push(...page.packages);
      cursor = page.next_cursor ?? undefined;
    } while (cursor !== undefined);
    return { ...page, packages, next_cursor: null };
  },

  sites: (signal?: AbortSignal) => request<SiteList>("/api/sites", { signal }),

  customers: (signal?: AbortSignal) =>
    request<CustomerList>("/api/customers", { signal }),

  /** Overrides automatic updates of the site, or `null` to follow the server-wide setting; customer administrators only (ADR-0055). */
  setAgentAutoUpdate: (siteId: string, enabled: boolean | null) =>
    request<Site>(
      `/api/sites/${encodeURIComponent(siteId)}/agent-auto-update`,
      {
        method: "PUT",
        body: { enabled },
      },
    ),

  /** Creates the customer `name`; global administrators only. */
  createCustomer: (name: string) =>
    request<Customer>("/api/customers", { method: "POST", body: { name } }),

  /** Renames a customer; global administrators only. */
  renameCustomer: (customerId: string, name: string) =>
    request<Customer>(`/api/customers/${encodeURIComponent(customerId)}`, {
      method: "PUT",
      body: { name },
    }),

  /** Every account; global administrators only (ADR-0059). */
  users: (signal?: AbortSignal) => request<UserList>("/api/users", { signal }),

  /** The sign-in providers turned on, for the login page; no session needed (ADR-0145). */
  loginProviders: (signal?: AbortSignal) =>
    request<{ providers: LoginProvider[] }>("/api/auth/providers", { signal, redirectOn401: false }),

  /** Sign-in providers, without secrets; global administrators only (ADR-0145). */
  identityProviders: (signal?: AbortSignal) =>
    request<IdentityProviderList>("/api/identity-providers", { signal }),

  /** Adds a provider; turning it on tests it first. */
  createIdentityProvider: (body: IdentityProviderRequest) =>
    request<IdentityProvider>("/api/identity-providers", { method: "POST", body }),

  /** Replaces a provider; an empty secret keeps the stored one for the same address. */
  updateIdentityProvider: (id: string, body: IdentityProviderRequest) =>
    request<IdentityProvider>(`/api/identity-providers/${encodeURIComponent(id)}`, { method: "PUT", body }),

  removeIdentityProvider: (id: string) =>
    request<void>(`/api/identity-providers/${encodeURIComponent(id)}`, { method: "DELETE" }),

  /** Tests what the form holds, saving nothing. */
  testIdentityProvider: (body: IdentityProviderTest) =>
    request<IdentityProviderTestResult>("/api/identity-providers/test", { method: "POST", body }),

  /** Replaces the groups and roles of a provider; its accounts follow at once. */
  setIdentityProviderGroups: (id: string, mappings: GroupMapping[]) =>
    request<{ mappings: GroupMapping[] }>(`/api/identity-providers/${encodeURIComponent(id)}/groups`, {
      method: "PUT",
      body: { mappings },
    }),

  /** Creates `username` with one role, and returns their invitation link. */
  inviteUser: (username: string, grant: GrantRequest) =>
    request<{ user: UserAccount; link: PasswordLink }>("/api/users", {
      method: "POST",
      body: { username, ...grant },
    }),

  /** A new password link for the user, replacing their previous one. */
  passwordLink: (userId: string) =>
    request<PasswordLink>(`/api/users/${encodeURIComponent(userId)}/link`, {
      method: "POST",
    }),

  setUserDisabled: (userId: string, disabled: boolean) =>
    request<UserAccount>(`/api/users/${encodeURIComponent(userId)}/disabled`, {
      method: "PUT",
      body: { disabled },
    }),

  removeUser: (userId: string) =>
    request<void>(`/api/users/${encodeURIComponent(userId)}`, {
      method: "DELETE",
    }),

  grantRole: (userId: string, grant: GrantRequest) =>
    request<UserAccount>(`/api/users/${encodeURIComponent(userId)}/roles`, {
      method: "POST",
      body: grant,
    }),

  revokeRole: (userId: string, grant: GrantRequest) =>
    request<UserAccount>(`/api/users/${encodeURIComponent(userId)}/roles`, {
      method: "DELETE",
      body: grant,
    }),

  /** Whose password link `token` is; 410 when it is no longer valid. */
  inspectPasswordLink: (token: string) =>
    request<LinkOwner>("/api/password-link", {
      method: "POST",
      body: { token },
      redirectOn401: false,
    }),

  /** Sets the password with the link `token`, and signs the user in. */
  usePasswordLink: (token: string, password: string) =>
    request<Me>("/api/password-link/use", {
      method: "POST",
      body: { token, password },
      redirectOn401: false,
    }),

  /** Turns offline incidents on or off for a site, server or relay; operators of the site (ADR-0072). */
  setOfflineAlerts: (
    kind: "sites" | "servers" | "relays",
    id: string,
    enabled: boolean,
  ) =>
    request<{ enabled: boolean }>(
      `/api/${kind}/${encodeURIComponent(id)}/offline-alerts`,
      {
        method: "PUT",
        body: { enabled },
      },
    ),

  /** The server's Proxmox guest and whether it is snapshotted before changes (ADR-0044). */
  serverProxmox: (id: string, signal?: AbortSignal) =>
    request<ProxmoxView>(`/api/servers/${encodeURIComponent(id)}/proxmox`, { signal }),

  /** Maps the server to a VM or container by hand. */
  setServerProxmox: (id: string, guest: { cluster: string; node: string; vmid: number; kind: GuestKind }) =>
    request<ProxmoxView>(`/api/servers/${encodeURIComponent(id)}/proxmox`, { method: "PUT", body: guest }),

  /** Guests the server may be mapped to, for the picker. */
  serverProxmoxGuests: (id: string, signal?: AbortSignal) =>
    request<{ guests: ProxmoxGuest[] }>(`/api/servers/${encodeURIComponent(id)}/proxmox/guests`, { signal }),

  /** Unmaps the server; discovery never maps it to that guest again. */
  clearServerProxmox: (id: string) =>
    request<void>(`/api/servers/${encodeURIComponent(id)}/proxmox`, { method: "DELETE" }),

  /** Active and upcoming silences of what the user reads, or those covering one server, relay or site (ADR-0079). */
  silences: (filter: { server_id?: string; relay_id?: string; site_id?: string }, signal?: AbortSignal) => {
    const query = new URLSearchParams(filter).toString();
    return request<{ silences: Silence[] }>(`/api/silences${query ? `?${query}` : ""}`, { signal });
  },

  /** Silences something for a while; operators of it. */
  createSilence: (body: SilenceRequest) => request<Silence>("/api/silences", { method: "POST", body }),

  /** Ends a silence now, or calls it off before it starts. */
  endSilence: (id: string) => request<void>(`/api/silences/${encodeURIComponent(id)}/end`, { method: "POST" }),

  /** Maintenance windows of what the user reads, or those covering one site or server (ADR-0151). */
  maintenanceWindows: (filter: { site_id?: string; server_id?: string }, signal?: AbortSignal) => {
    const query = new URLSearchParams(filter).toString();
    return request<{ windows: MaintenanceWindow[] }>(`/api/maintenance-windows${query ? `?${query}` : ""}`, { signal });
  },

  /** Makes a maintenance window; operators of its site. */
  createMaintenanceWindow: (body: MaintenanceWindowRequest) =>
    request<MaintenanceWindow>("/api/maintenance-windows", { method: "POST", body }),

  /** Replaces a maintenance window's rule, place and reason. */
  updateMaintenanceWindow: (id: string, body: MaintenanceWindowRequest) =>
    request<MaintenanceWindow>(`/api/maintenance-windows/${encodeURIComponent(id)}`, { method: "PUT", body }),

  /** Removes a maintenance window; the running one ends now. */
  removeMaintenanceWindow: (id: string) =>
    request<void>(`/api/maintenance-windows/${encodeURIComponent(id)}`, { method: "DELETE" }),

  /** Availability over a period of what the user reads (ADR-0155). */
  availability: (query: AvailabilityQuery, signal?: AbortSignal) =>
    request<AvailabilityReport>(`/api/availability?${availabilityQuery(query)}`, { signal }),

  /** The last 24 hours, 7, 30 and 90 days and 90 daily bars, days cut in `time_zone` (ADR-0172). */
  uptime: (query: { customer_id?: string | null; time_zone: string }, signal?: AbortSignal) => {
    const params = new URLSearchParams({ time_zone: query.time_zone });
    if (query.customer_id) params.set("customer_id", query.customer_id);
    return request<Uptime>(`/api/availability/uptime?${params.toString()}`, { signal });
  },

  /** Business services of the customers the user is granted as a whole. */
  businessServices: (signal?: AbortSignal) => request<{ services: BusinessService[] }>("/api/business-services", { signal }),

  createBusinessService: (body: BusinessServiceRequest) =>
    request<BusinessService>("/api/business-services", { method: "POST", body }),

  updateBusinessService: (id: string, body: BusinessServiceRequest) =>
    request<BusinessService>(`/api/business-services/${encodeURIComponent(id)}`, { method: "PUT", body }),

  removeBusinessService: (id: string) =>
    request<void>(`/api/business-services/${encodeURIComponent(id)}`, { method: "DELETE" }),

  /** Report schedules the user manages: administrators only. */
  reportSchedules: (signal?: AbortSignal) => request<{ schedules: ReportSchedule[] }>("/api/report-schedules", { signal }),

  createReportSchedule: (body: ReportScheduleRequest) =>
    request<ReportSchedule>("/api/report-schedules", { method: "POST", body }),

  updateReportSchedule: (id: string, body: ReportScheduleRequest) =>
    request<ReportSchedule>(`/api/report-schedules/${encodeURIComponent(id)}`, { method: "PUT", body }),

  removeReportSchedule: (id: string) =>
    request<void>(`/api/report-schedules/${encodeURIComponent(id)}`, { method: "DELETE" }),

  /** Sends the latest period of a schedule now. */
  sendReportNow: (id: string) =>
    request<ReportSent>(`/api/report-schedules/${encodeURIComponent(id)}/send`, { method: "POST" }),

  /** What a server or device depends on, what is behind it, and why it is unreachable (ADR-0152). */
  serverDependencies: (id: string, signal?: AbortSignal) =>
    request<DependencyPage>(`/api/servers/${encodeURIComponent(id)}/dependencies`, { signal }),

  /** What is behind a relay. */
  relayDependencies: (id: string, signal?: AbortSignal) =>
    request<DependencyPage>(`/api/relays/${encodeURIComponent(id)}/dependencies`, { signal }),

  /** Puts `children` behind `parent`, all or none; operators of every end's site. */
  addDependencies: (parent: DependencyEndRequest, children: DependencyEndRequest[]) =>
    request<{ dependencies: Dependency[] }>("/api/dependencies", { method: "POST", body: { parent, children } }),

  /** Removes a dependency set by someone. */
  removeDependency: (id: string) => request<void>(`/api/dependencies/${encodeURIComponent(id)}`, { method: "DELETE" }),

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
      const path =
        cursor === undefined
          ? "/api/relays"
          : `/api/relays?cursor=${encodeURIComponent(cursor)}`;
      const page = await request<RelayList>(path, { signal });
      relays.push(...page.relays);
      cursor = page.next_cursor ?? undefined;
    } while (cursor !== undefined);
    return { relays, next_cursor: null };
  },

  relay: (id: string, signal?: AbortSignal) =>
    request<RelayDetail>(`/api/relays/${encodeURIComponent(id)}`, { signal }),

  /** The newest enrollment tokens of the customers the user administers. */
  tokens: (signal?: AbortSignal) =>
    request<TokenList>("/api/tokens", { signal }),

  createToken: (body: TokenRequest) =>
    request<CreatedToken>("/api/tokens", { method: "POST", body }),

  /** The default install source; customer administrators (ADR-0054). */
  agentInstall: (signal?: AbortSignal) =>
    request<AgentInstall>("/api/settings/agent-install", { signal }),
  publicAddress: (signal?: AbortSignal) =>
    request<PublicAddress>("/api/settings/public-address", { signal }),

  /** Changes the default install source; global administrators only, audited. */
  setAgentInstall: (body: {
    source: InstallSource;
    custom_url: string | null;
  }) =>
    request<AgentInstall>("/api/settings/agent-install", {
      method: "PUT",
      body,
    }),

  /** Changes the address in install commands; global administrators only, audited. `null` is the default. */
  setPublicAddress: (body: { address: string | null }) =>
    request<PublicAddress>("/api/settings/public-address", {
      method: "PUT",
      body,
    }),

  /** Whether AI is on, and its model; never the key (ADR-0064). */
  serverSettings: (signal?: AbortSignal) =>
    request<ServerSettings>("/api/settings/server", { signal }),
  setServerSettings: (body: {
    session_lifetime: number | null;
    session_idle_timeout: number | null;
    mirror_public_url: string | null;
  }) =>
    request<ServerSettings>("/api/settings/server", { method: "PUT", body }),
  setRetention: (body: {
    metrics_days: number | null;
    logs_days: number | null;
  }) =>
    request<ServerSettings>("/api/settings/retention", { method: "PUT", body }),
  aiSettings: (signal?: AbortSignal) =>
    request<AiSettings>("/api/settings/ai", { signal }),

  /** Saves the AI settings, testing them when they turn AI on; global administrators only, audited. An empty key keeps the stored one. */
  setAiSettings: (body: AiForm & { enabled: boolean }) =>
    request<AiSettings>("/api/settings/ai", { method: "PUT", body }),

  /** Forgets the API key, which turns AI off; global administrators only, audited. */
  removeAiKey: () =>
    request<AiSettings>("/api/settings/ai", { method: "DELETE" }),

  /** Sets the language of AI diagnoses, null to follow the people's; global administrators only, audited (ADR-0181). */
  setAiLanguage: (language: "en" | "fr" | null) =>
    request<AiSettings>("/api/settings/ai/language", { method: "PUT", body: { language } }),

  /** Tests a key and model without saving; an empty key tests the stored one. */
  testAiSettings: (body: AiForm) =>
    request<ChannelTestResult>("/api/settings/ai/test", {
      method: "POST",
      body,
    }),

  /** The models the form's provider offers, read from its server; `error` says why there is no list. */
  aiModels: (body: Omit<AiForm, "model">) =>
    request<{ models: string[]; error: string | null }>("/api/settings/ai/models", { method: "POST", body }),

  /** Schedules the user manages or is on, with who is on call now and next (ADR-0081). */
  onCallSchedules: (signal?: AbortSignal) => request<{ schedules: OnCallSchedule[] }>("/api/on-call/schedules", { signal }),

  createOnCallSchedule: (body: OnCallScheduleRequest) =>
    request<OnCallSchedule>("/api/on-call/schedules", { method: "POST", body }),

  updateOnCallSchedule: (id: string, body: OnCallScheduleRequest) =>
    request<OnCallSchedule>(`/api/on-call/schedules/${encodeURIComponent(id)}`, { method: "PUT", body }),

  deleteOnCallSchedule: (id: string) =>
    request<void>(`/api/on-call/schedules/${encodeURIComponent(id)}`, { method: "DELETE" }),

  /** Puts `user_id` on call instead of the rotation for a while. */
  addOnCallOverride: (id: string, body: { user_id: string; seat?: number; starts_at?: string; ends_at: string }) =>
    request<OnCallSchedule>(`/api/on-call/schedules/${encodeURIComponent(id)}/overrides`, { method: "POST", body }),

  deleteOnCallOverride: (id: string) => request<void>(`/api/on-call/overrides/${encodeURIComponent(id)}`, { method: "DELETE" }),

  /** Who the user may put on a schedule covering this. */
  onCallPeople: (scope: { customer_id: string | null; site_id: string | null }, signal?: AbortSignal) => {
    const query = new URLSearchParams();
    if (scope.customer_id) query.set("customer_id", scope.customer_id);
    if (scope.site_id) query.set("site_id", scope.site_id);
    const text = query.toString();
    return request<{ people: OnCallPerson[] }>(`/api/on-call/people${text ? `?${text}` : ""}`, { signal });
  },

  contact: (signal?: AbortSignal) => request<Contact>("/api/on-call/contact", { signal }),

  setContact: (body: ContactRequest) => request<Contact>("/api/on-call/contact", { method: "PUT", body }),

  /** Sends a test to each of the user's own destinations. */
  testContact: () =>
    request<{ results: { kind: string; ok: boolean; error: string | null }[] }>("/api/on-call/contact/test", { method: "POST" }),

  /** Notification channels of what the user administers, without secrets (ADR-0060, ADR-0078). */
  notificationChannels: (signal?: AbortSignal) =>
    request<NotificationChannelList>("/api/notifications/channels", { signal }),

  /** Installed plugins: global administrators (ADR-0103). */
  plugins: (signal?: AbortSignal) =>
    request<{ plugins: InstalledPlugin[] }>("/api/plugins", { signal }),

  /** What a plugin file is and asks for; installs nothing. */
  inspectPlugin: (file: Blob) =>
    request<PluginInspection>("/api/plugins/inspect", { method: "POST", file }),

  /** Installs or updates the plugin of `file`, which must be the one inspected. */
  installPlugin: (file: Blob, sha256: string) =>
    request<InstalledPlugin>(`/api/plugins?sha256=${encodeURIComponent(sha256)}`, {
      method: "POST",
      file,
    }),

  /** Where it may be used; `{}` is everything, none is nowhere. */
  setPluginPlaces: (id: string, places: { customer_id?: string; site_id?: string }[]) =>
    request<InstalledPlugin>(`/api/plugins/${encodeURIComponent(id)}/places`, {
      method: "PUT",
      body: { places },
    }),

  setPluginEnabled: (id: string, enabled: boolean) =>
    request<InstalledPlugin>(`/api/plugins/${encodeURIComponent(id)}/enabled`, {
      method: "PUT",
      body: { enabled },
    }),

  removePlugin: (id: string) =>
    request<void>(`/api/plugins/${encodeURIComponent(id)}`, { method: "DELETE" }),

  /** Plugins turned on at places the user sees. */
  checkUsage: (signal?: AbortSignal) => request<{ checks: CheckUsage[] }>("/api/checks/usage", { signal }),

  usablePlugins: (signal?: AbortSignal) =>
    request<{ plugins: UsablePlugin[] }>("/api/plugins/usable", { signal }),

  pluginPublishers: (signal?: AbortSignal) =>
    request<PluginPublisherList>("/api/plugins/publishers", { signal }),

  addPluginPublisher: (name: string, publicKey: string) =>
    request<PluginPublisher>("/api/plugins/publishers", {
      method: "POST",
      body: { name, public_key: publicKey },
    }),

  removePluginPublisher: (id: string) =>
    request<void>(`/api/plugins/publishers/${encodeURIComponent(id)}`, { method: "DELETE" }),

  createNotificationChannel: (body: NotificationChannelRequest) =>
    request<NotificationChannel>("/api/notifications/channels", {
      method: "POST",
      body,
    }),

  updateNotificationChannel: (id: string, body: NotificationChannelRequest) =>
    request<NotificationChannel>(
      `/api/notifications/channels/${encodeURIComponent(id)}`,
      { method: "PUT", body },
    ),

  deleteNotificationChannel: (id: string) =>
    request<void>(`/api/notifications/channels/${encodeURIComponent(id)}`, {
      method: "DELETE",
    }),

  /** Sends a test now; 200 with `ok` false and the reason when it does not go through. */
  testNotificationChannel: (id: string) =>
    request<ChannelTestResult>(
      `/api/notifications/channels/${encodeURIComponent(id)}/test`,
      { method: "POST" },
    ),
  /** Integrations of what the user administers, without secrets (ADR-0102). */
  integrations: (signal?: AbortSignal) => request<IntegrationList>("/api/integrations", { signal }),

  /** The answer carries the signing secret, the only time it is shown. */
  createIntegration: (body: IntegrationRequest) =>
    request<IntegrationWithSecret>("/api/integrations", { method: "POST", body }),

  /** A new URL gets a new signing secret; the answer carries it, the only time it is shown. */
  updateIntegration: (id: string, body: IntegrationRequest) =>
    request<IntegrationUpdated>(`/api/integrations/${encodeURIComponent(id)}`, { method: "PUT", body }),

  deleteIntegration: (id: string) =>
    request<void>(`/api/integrations/${encodeURIComponent(id)}`, { method: "DELETE" }),

  /** `on` also turns back on one turned off after failing. */
  setIntegrationState: (id: string, state: "on" | "paused") =>
    request<IntegrationEndpoint>(`/api/integrations/${encodeURIComponent(id)}/state`, {
      method: "POST",
      body: { state },
    }),

  /** Replaces the signing secret at once; the answer carries the new one. */
  newIntegrationSecret: (id: string) =>
    request<IntegrationWithSecret>(`/api/integrations/${encodeURIComponent(id)}/secret`, { method: "POST" }),

  testIntegration: (id: string) =>
    request<IntegrationTestResult>(`/api/integrations/${encodeURIComponent(id)}/test`, { method: "POST" }),

  integrationDeliveries: (id: string, before?: number, signal?: AbortSignal) =>
    request<IntegrationDeliveryList>(
      `/api/integrations/${encodeURIComponent(id)}/deliveries${before === undefined ? "" : `?before=${before}`}`,
      { signal },
    ),

  replayIntegration: (id: string, body: IntegrationReplay) =>
    request<{ queued: number }>(`/api/integrations/${encodeURIComponent(id)}/replay`, { method: "POST", body }),

  /** The PSA connection, global administrators only (ADR-0164). */
  psaConnection: (signal?: AbortSignal) => request<PsaConnectionState>("/api/psa/connection", { signal }),

  savePsaConnection: (body: PsaConnectionRequest) =>
    request<PsaConnection>("/api/psa/connection", { method: "PUT", body }),

  deletePsaConnection: () => request<void>("/api/psa/connection", { method: "DELETE" }),

  /** The PSA's active clients whose name holds `search`, read through the connection. */
  psaClients: (search: string, signal?: AbortSignal) =>
    request<PsaRecordList>(`/api/psa/clients?search=${encodeURIComponent(search)}`, { signal }),

  psaClientSites: (client: number, signal?: AbortSignal) =>
    request<PsaRecordList>(`/api/psa/clients/${client}/sites`, { signal }),

  /** Every site with the PSA client and site it is matched to. */
  psaSiteLinks: (signal?: AbortSignal) => request<PsaSiteLinkList>("/api/psa/site-links", { signal }),

  linkPsaSite: (site: string, body: { client_id: number; psa_site_id: number | null }) =>
    request<PsaSiteLink>(`/api/psa/site-links/${encodeURIComponent(site)}`, { method: "PUT", body }),

  unlinkPsaSite: (site: string) =>
    request<void>(`/api/psa/site-links/${encodeURIComponent(site)}`, { method: "DELETE" }),

  psaTicketTypes: (signal?: AbortSignal) => request<PsaRecordList>("/api/psa/ticket-types", { signal }),

  psaCategories: (signal?: AbortSignal) => request<{ categories: string[] }>("/api/psa/categories", { signal }),

  psaStatuses: (signal?: AbortSignal) => request<PsaRecordList>("/api/psa/statuses", { signal }),

  savePsaTicketSettings: (body: PsaTicketSettingsRequest) =>
    request<PsaConnection>("/api/psa/ticket-settings", { method: "PUT", body }),

  /** The latest tickets opened for incidents, closed, skipped or refused. */
  psaTickets: (signal?: AbortSignal) => request<{ tickets: PsaTicket[] }>("/api/psa/tickets", { signal }),

  /** Signs in to the PSA with the form's values, the saved ones filling the gaps; changes nothing there. */
  testPsaConnection: (body: PsaConnectionRequest) =>
    request<PsaConnectionTest>("/api/psa/connection/test", { method: "POST", body }),

  /** Install keys and hosts waiting for approval, of the customers the user administers (ADR-0073). */
  installKeys: (signal?: AbortSignal) =>
    request<InstallKeyList>("/api/install-keys", { signal }),

  createInstallKey: (body: InstallKeyRequest) =>
    request<CreatedInstallKey>("/api/install-keys", { method: "POST", body }),

  /** Stops a key from enrolling more hosts; hosts it enrolled keep working. */
  revokeInstallKey: (id: string) =>
    request<void>(`/api/install-keys/${encodeURIComponent(id)}`, {
      method: "DELETE",
    }),

  /** Lets a waiting host connect, or revokes its certificate. */
  decidePendingHost: (peerId: string, decision: "approve" | "reject") =>
    request<void>(
      `/api/pending-hosts/${encodeURIComponent(peerId)}/${decision}`,
      { method: "POST" },
    ),

  /** Revokes an unused token; 409 once used. */
  revokeToken: (id: string) =>
    request<void>(`/api/tokens/${encodeURIComponent(id)}`, {
      method: "DELETE",
    }),

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
    return request<IncidentList>(
      search ? `/api/incidents?${search}` : "/api/incidents",
      { signal },
    );
  },

  incident: (id: string, signal?: AbortSignal) =>
    request<IncidentDetail>(`/api/incidents/${encodeURIComponent(id)}`, {
      signal,
    }),

  /** Says the user is on these incidents, so nobody else is told next (ADR-0082). */
  acknowledgeIncidents: (ids: string[], note: string) =>
    request<{ acknowledged: string[] }>("/api/incidents/acknowledge", {
      method: "POST",
      body: { incident_ids: ids, note },
    }),

  /** Follow-ups of what the user administers, the default, and what steps may name. */
  followUpPolicies: (signal?: AbortSignal) => request<FollowUpPolicyList>("/api/follow-up/policies", { signal }),

  saveFollowUpPolicy: (body: FollowUpPolicyRequest) =>
    request<FollowUpPolicy>("/api/follow-up/policies", { method: "PUT", body }),

  /** The default applies again to that place. */
  deleteFollowUpPolicy: (id: string) =>
    request<void>(`/api/follow-up/policies/${encodeURIComponent(id)}`, { method: "DELETE" }),

  /** What an acknowledge link acknowledges; changes nothing. 410 once expired. */
  inspectAcknowledgeLink: (token: string) =>
    request<AcknowledgeLink>("/api/acknowledge-link", {
      method: "POST",
      body: { token },
      redirectOn401: false,
    }),

  /** Acknowledges with the link `token`, as its person. */
  useAcknowledgeLink: (token: string) =>
    request<AcknowledgeLink>("/api/acknowledge-link/use", {
      method: "POST",
      body: { token },
      redirectOn401: false,
    }),

  serverThresholds: (serverId: string, signal?: AbortSignal) =>
    request<EffectiveThresholds>(
      `/api/servers/${encodeURIComponent(serverId)}/thresholds/effective`,
      {
        signal,
      },
    ),

  siteThresholds: (siteId: string, signal?: AbortSignal) =>
    request<SiteThresholds>(
      `/api/sites/${encodeURIComponent(siteId)}/thresholds/effective`,
      { signal },
    ),

  siteSensors: (siteId: string, signal?: AbortSignal) =>
    request<SiteSensors>(`/api/sites/${encodeURIComponent(siteId)}/sensors`, {
      signal,
    }),

  /** The threshold rows of one site or server, not what it inherits. */
  thresholdRows: (owner: ConfigOwner, signal?: AbortSignal) =>
    request<{ entries: ThresholdEntry[] }>(
      `/api/${owner.kind}/${encodeURIComponent(owner.id)}/thresholds`,
      { signal },
    ),

  createThreshold: (owner: ConfigOwner, body: ThresholdRequest) =>
    request<ThresholdEntry>(
      `/api/${owner.kind}/${encodeURIComponent(owner.id)}/thresholds`,
      { method: "POST", body },
    ),

  updateThreshold: (id: string, body: ThresholdRequest) =>
    request<ThresholdEntry>(`/api/thresholds/${encodeURIComponent(id)}`, {
      method: "PUT",
      body,
    }),

  /** What a rule would have done over the last days (ADR-0153); writes nothing. */
  previewServerThreshold: (serverId: string, rule: ThresholdRequest, days: number, signal?: AbortSignal) =>
    request<ServerPreview>(`/api/servers/${encodeURIComponent(serverId)}/thresholds/preview`, {
      method: "POST",
      body: { rule, days },
      signal,
    }),

  previewSiteThreshold: (siteId: string, rule: ThresholdRequest, days: number, signal?: AbortSignal) =>
    request<SitePreview>(`/api/sites/${encodeURIComponent(siteId)}/thresholds/preview`, {
      method: "POST",
      body: { rule, days },
      signal,
    }),

  deleteThreshold: (id: string) =>
    request<void>(`/api/thresholds/${encodeURIComponent(id)}`, {
      method: "DELETE",
    }),

  /** The sensor rows of one site or server, disabled ones included. */
  sensorRows: (owner: ConfigOwner, signal?: AbortSignal) =>
    request<{ entries: SensorConfigEntry[] }>(
      `/api/${owner.kind}/${encodeURIComponent(owner.id)}/sensor_configs`,
      {
        signal,
      },
    ),

  /** Adds a device (ADR-0148): an address watched from a probe, pinged by default. */
  createDevice: (body: DeviceRequest) => request<Server>("/api/devices", { method: "POST", body }),

  updateDevice: (id: string, body: { name?: string; address?: string }) =>
    request<Server>(`/api/devices/${encodeURIComponent(id)}`, { method: "PUT", body }),

  deviceTemplates: (signal?: AbortSignal) => request<{ templates: SnmpTemplate[] }>("/api/devices/templates", { signal }),

  /** What `address` answers over SNMP, asked from its probe (ADR-0149). */
  detectDevice: (body: { site_id: string; address: string; probe?: string; snmp: SnmpCredentialsRequest }) =>
    request<Detected>("/api/devices/detect", { method: "POST", body }),

  deviceSnmp: (id: string, signal?: AbortSignal) =>
    request<SnmpCredentials>(`/api/devices/${encodeURIComponent(id)}/snmp`, { signal }),

  setDeviceSnmp: (id: string, body: SnmpCredentialsRequest) =>
    request<SnmpCredentials>(`/api/devices/${encodeURIComponent(id)}/snmp`, { method: "PUT", body }),

  deleteDeviceSnmp: (id: string) => request<void>(`/api/devices/${encodeURIComponent(id)}/snmp`, { method: "DELETE" }),

  /** A device's custom SNMP checks, their channels' limits and last values (ADR-0157). */
  snmpChecks: (id: string, signal?: AbortSignal) =>
    request<{ checks: SnmpCheck[] }>(`/api/devices/${encodeURIComponent(id)}/snmp-checks`, { signal }),

  createSnmpCheck: (id: string, body: SnmpCheckRequest) =>
    request<SnmpCheck>(`/api/devices/${encodeURIComponent(id)}/snmp-checks`, { method: "POST", body }),

  updateSnmpCheck: (id: string, name: string, body: SnmpCheckRequest) =>
    request<SnmpCheck>(`/api/devices/${encodeURIComponent(id)}/snmp-checks/${encodeURIComponent(name)}`, { method: "PUT", body }),

  deleteSnmpCheck: (id: string, name: string) =>
    request<void>(`/api/devices/${encodeURIComponent(id)}/snmp-checks/${encodeURIComponent(name)}`, { method: "DELETE" }),

  /** Test and Browse: reads OIDs or walks a subtree from the device's probe. */
  readSnmp: (id: string, body: { oids?: string[]; walk?: string; max?: number; probe?: string }) =>
    request<SnmpRead>(`/api/devices/${encodeURIComponent(id)}/snmp/read`, { method: "POST", body }),

  /** What each channel of a check would write, read once from its probe. */
  testSnmpCheck: (id: string, body: SnmpCheckRequest) =>
    request<SnmpTested>(`/api/devices/${encodeURIComponent(id)}/snmp/test`, { method: "POST", body }),

  snmpLookups: (signal?: AbortSignal) => request<{ lookups: SnmpLookup[] }>("/api/snmp-lookups", { signal }),

  createSnmpLookup: (body: SnmpLookupRequest) => request<SnmpLookup>("/api/snmp-lookups", { method: "POST", body }),

  updateSnmpLookup: (id: string, body: SnmpLookupRequest) =>
    request<SnmpLookup>(`/api/snmp-lookups/${encodeURIComponent(id)}`, { method: "PUT", body }),

  deleteSnmpLookup: (id: string) => request<void>(`/api/snmp-lookups/${encodeURIComponent(id)}`, { method: "DELETE" }),

  snmpMibs: (signal?: AbortSignal) => request<{ mibs: SnmpMib[] }>("/api/snmp-mibs", { signal }),

  uploadSnmpMibs: (body: { customer_id?: string; files: { name: string; text: string }[] }) =>
    request<{ mibs: SnmpMib[]; offered_lookups: SnmpOfferedLookup[] }>("/api/snmp-mibs", { method: "POST", body }),

  deleteSnmpMib: (id: string) => request<void>(`/api/snmp-mibs/${encodeURIComponent(id)}`, { method: "DELETE" }),

  /** Objects of the library and the MIBs whose name or OID matches `q`. */
  snmpObjects: (q: string, signal?: AbortSignal) =>
    request<{ objects: SnmpObject[]; names: Record<string, string> }>(`/api/snmp-objects?q=${encodeURIComponent(q)}`, { signal }),

  /** What a vCenter or ESXi host answers, from its probe; logs in only once its certificate is trusted (ADR-0156). */
  testVmware: (body: { site_id: string; address: string; probe?: string; vmware: VmwareCredentialsRequest }) =>
    request<VmwareTested>("/api/devices/vmware/test", { method: "POST", body }),

  deviceVmware: (id: string, signal?: AbortSignal) =>
    request<VmwareCredentials>(`/api/devices/${encodeURIComponent(id)}/vmware`, { signal }),

  setDeviceVmware: (id: string, body: VmwareCredentialsRequest) =>
    request<VmwareCredentials>(`/api/devices/${encodeURIComponent(id)}/vmware`, { method: "PUT", body }),

  /** A source's hosts, clusters, virtual machines and datastores, and the virtual machine a server is. */
  serverVirtualization: (id: string, signal?: AbortSignal) =>
    request<Virtualization>(`/api/servers/${encodeURIComponent(id)}/virtualization`, { signal }),

  /** Ties a virtual machine to a server by hand, to none, or back to matching. */
  linkVirtualObject: (id: string, body: { server_id?: string | null; automatic?: boolean }) =>
    request<VirtualObject>(`/api/virtual-objects/${encodeURIComponent(id)}/server`, { method: "PUT", body }),

  /** Adds a server waiting for its agent for a virtual machine. */
  installAgentOnVm: (id: string) =>
    request<Server>(`/api/virtual-objects/${encodeURIComponent(id)}/install-agent`, { method: "POST" }),

  /** Gives a Windows server the `hyperv` check and its alerts. */
  watchHyperv: (id: string) =>
    request<{ added: boolean; check: string }>(`/api/servers/${encodeURIComponent(id)}/hyperv`, { method: "POST" }),

  /** Where the site's network checks can run from: the central server, then its relays. */
  networkScans: (signal?: AbortSignal) => request<{ scans: NetworkScan[] }>("/api/network-scans", { signal }),

  networkScan: (id: string, signal?: AbortSignal) =>
    request<NetworkScan>(`/api/network-scans/${encodeURIComponent(id)}`, { signal }),

  createNetworkScan: (body: NetworkScanRequest) =>
    request<NetworkScan>("/api/network-scans", { method: "POST", body }),

  updateNetworkScan: (id: string, body: NetworkScanRequest) =>
    request<NetworkScan>(`/api/network-scans/${encodeURIComponent(id)}`, { method: "PUT", body }),

  deleteNetworkScan: (id: string) =>
    request<void>(`/api/network-scans/${encodeURIComponent(id)}`, { method: "DELETE" }),

  runNetworkScan: (id: string) =>
    request<NetworkScanRun>(`/api/network-scans/${encodeURIComponent(id)}/run`, { method: "POST" }),

  networkScanResults: (id: string, signal?: AbortSignal) =>
    request<NetworkScanResults>(`/api/network-scans/${encodeURIComponent(id)}/results`, { signal }),

  addFromNetworkScan: (
    id: string,
    body: { all?: boolean; devices?: { address: string; name?: string; template?: string }[] },
  ) => request<NetworkScanAddResult>(`/api/network-scans/${encodeURIComponent(id)}/add`, { method: "POST", body }),

  dismissScanAddress: (id: string, address: string) =>
    request<void>(`/api/network-scans/${encodeURIComponent(id)}/dismiss`, { method: "POST", body: { address } }),

  restoreScanAddress: (id: string, address: string) =>
    request<void>(`/api/network-scans/${encodeURIComponent(id)}/restore`, { method: "POST", body: { address } }),

  siteProbes: (siteId: string, signal?: AbortSignal) =>
    request<{ probes: Probe[] }>(`/api/sites/${encodeURIComponent(siteId)}/probes`, { signal }),

  createSensor: (owner: ConfigOwner, body: SensorConfigRequest) =>
    request<SensorConfigEntry>(
      `/api/${owner.kind}/${encodeURIComponent(owner.id)}/sensor_configs`,
      {
        method: "POST",
        body,
      },
    ),

  updateSensor: (id: string, body: SensorConfigRequest) =>
    request<SensorConfigEntry>(`/api/sensor_configs/${encodeURIComponent(id)}`, {
      method: "PUT",
      body,
    }),

  /** How often the site's built-in checks run; null for the built-in interval (ADR-0097). */
  setCheckInterval: (siteId: string, interval_ms: number | null) =>
    request<{ interval_ms: number | null }>(`/api/sites/${encodeURIComponent(siteId)}/check-interval`, {
      method: "PUT",
      body: { interval_ms },
    }),

  recommendations: (serverId: string, signal?: AbortSignal) =>
    request<RecommendationList>(`/api/servers/${encodeURIComponent(serverId)}/recommendations`, { signal }),

  dismissRecommendation: (serverId: string, key: string) =>
    request<void>(`/api/servers/${encodeURIComponent(serverId)}/recommendations/dismiss`, { method: "POST", body: { key } }),

  restoreRecommendation: (serverId: string, key: string) =>
    request<void>(`/api/servers/${encodeURIComponent(serverId)}/recommendations/restore`, { method: "POST", body: { key } }),

  deleteSensor: (id: string) =>
    request<void>(`/api/sensor_configs/${encodeURIComponent(id)}`, {
      method: "DELETE",
    }),

  serverMetrics: (serverId: string, signal?: AbortSignal) =>
    request<MetricList>(
      `/api/servers/${encodeURIComponent(serverId)}/metrics`,
      { signal },
    ),

  /** The newest 500 campaigns of the customers the user reads as a whole. */
  campaigns: (signal?: AbortSignal) =>
    request<CampaignList>("/api/campaigns?limit=500", { signal }),

  campaign: (id: string, signal?: AbortSignal) =>
    request<CampaignDetail>(`/api/campaigns/${encodeURIComponent(id)}`, {
      signal,
    }),

  /** Creates a draft campaign; customer administrators only. */
  createCampaign: (body: CampaignRequest) =>
    request<CampaignDetail>("/api/campaigns", { method: "POST", body }),

  /**
   * Edits a campaign (ADR-0166, ADR-0173): everything before its first batch, only its name, windows and
   * threshold while a run is under way, everything once finished, for its next run (it becomes a draft).
   */
  updateCampaign: (id: string, body: CampaignRequest) =>
    request<CampaignDetail>(`/api/campaigns/${encodeURIComponent(id)}`, { method: "PUT", body }),

  /** Deletes a draft or finished campaign (ADR-0166); 409 while commands are in flight. */
  deleteCampaign: (id: string) => request<void>(`/api/campaigns/${encodeURIComponent(id)}`, { method: "DELETE" }),

  /** Runs a finished campaign again, all its servers or the failed ones (ADR-0173). */
  relaunchCampaign: (id: string, servers: CampaignRelaunch) =>
    request<CampaignDetail>(`/api/campaigns/${encodeURIComponent(id)}/relaunch`, {
      method: "POST",
      body: { servers },
    }),

  /** The past runs of a campaign, newest first (ADR-0173). */
  campaignRuns: (id: string, signal?: AbortSignal) =>
    request<{ runs: CampaignRun[] }>(`/api/campaigns/${encodeURIComponent(id)}/runs`, { signal }),

  /** 409 when the campaign's state does not allow `action`. */
  campaignAction: (id: string, action: CampaignAction) =>
    request<CampaignDetail>(
      `/api/campaigns/${encodeURIComponent(id)}/${action}`,
      { method: "POST" },
    ),

  /** Every mirror, by name; global administrators only. */
  mirrors: (signal?: AbortSignal) =>
    request<MirrorList>("/api/mirrors", { signal }),

  /** Automatic updates: settings and rollouts; global administrators only (ADR-0055). */
  autoUpdate: (signal?: AbortSignal) =>
    request<AutoUpdateStatus>("/api/auto-update", { signal }),

  /** The agents or relays in one of the totals of `autoUpdate`, at most 500; global administrators only. */
  autoUpdatePeers: (
    kind: RolloutKind,
    state: AutoUpdatePeerState,
    siteId: string | null,
    signal?: AbortSignal,
  ) => {
    const query = new URLSearchParams({ state, limit: String(AUTO_UPDATE_PEERS_MAX) });
    if (siteId) query.set("site_id", siteId);
    return request<AutoUpdatePeerList>(`/api/auto-update/${kind}/peers?${query}`, { signal });
  },

  /** The agents or relays on `version`; `null` lists those that reported none. */
  autoUpdateVersionPeers: (kind: RolloutKind, version: string | null, signal?: AbortSignal) => {
    const query = new URLSearchParams({ limit: String(AUTO_UPDATE_PEERS_MAX) });
    if (version !== null) query.set("version", version);
    return request<AutoUpdateVersionPeerList>(`/api/auto-update/${kind}/versions/peers?${query}`, { signal });
  },

  /** Changes automatic update settings; global administrators only; audited. */
  setAutoUpdate: (change: AutoUpdateChange) =>
    request<AutoUpdateSettings>("/api/settings/auto-update", {
      method: "PUT",
      body: change,
    }),

  /** Resumes a halted rollout; global administrators only; audited. */
  resumeRollout: (kind: RolloutKind, version: string) =>
    request<Rollout>(
      `/api/auto-update/rollouts/${kind}/${encodeURIComponent(version)}/resume`,
      { method: "POST" },
    ),

  /** What can be added, with its size, the free disk and the tools' state. */
  mirrorCatalog: (signal?: AbortSignal) =>
    request<MirrorCatalog>("/api/mirrors/catalog", { signal }),

  /** Adds every mirror of a distribution; 409 says why not in a sentence. */
  addMirror: (distribution: string, mode: MirrorMode = "on_demand") =>
    request<MirrorList>("/api/mirrors", {
      method: "POST",
      body: { distribution, mode },
    }),

  /** Sets what a mirror's next sync downloads (ADR-0142). */
  setMirrorMode: (id: string, mode: MirrorMode) =>
    request<Mirror>(`/api/mirrors/${encodeURIComponent(id)}`, {
      method: "PATCH",
      body: { mode },
    }),

  /** The storage cap of on-demand mirrors and what they use. */
  mirrorCache: (signal?: AbortSignal) =>
    request<MirrorCache>("/api/mirrors/cache", { signal }),

  /** Sets the cap, `null` for the default; a lower cap evicts at once. */
  setMirrorCache: (capBytes: number | null) =>
    request<MirrorCache>("/api/mirrors/cache", {
      method: "PUT",
      body: { cap_bytes: capBytes },
    }),

  mirror: (id: string, signal?: AbortSignal) =>
    request<MirrorDetail>(`/api/mirrors/${encodeURIComponent(id)}`, { signal }),

  /** Starts removing a mirror, also to try again; 409 says what still uses it. */
  deleteMirror: (id: string) =>
    request<Mirror>(`/api/mirrors/${encodeURIComponent(id)}`, {
      method: "DELETE",
    }),

  /** Starts a sync; 409 while another run of the mirror is going. */
  syncMirror: (id: string) =>
    request<MirrorRunSummary>(`/api/mirrors/${encodeURIComponent(id)}/sync`, {
      method: "POST",
    }),

  /** Starts a snapshot named after the time; 409 while busy or before the first sync. */
  snapshotMirror: (id: string) =>
    request<MirrorRunSummary>(
      `/api/mirrors/${encodeURIComponent(id)}/snapshots`,
      { method: "POST", body: {} },
    ),

  /** What a change on the server gets first, and its local snapshots. */
  serverRollback: (id: string, signal?: AbortSignal) =>
    request<Rollback>(`/api/servers/${encodeURIComponent(id)}/rollback`, { signal }),

  /** Where the server's packages come from, and what can change that. */
  serverSources: (id: string, signal?: AbortSignal) =>
    request<SourcesStatus>(`/api/servers/${encodeURIComponent(id)}/sources`, {
      signal,
    }),

  /** Points the server at the mirrors of its distribution; 409 says why not. */
  useMirror: (id: string) =>
    request<CommandDetail>(
      `/api/servers/${encodeURIComponent(id)}/sources/use-mirror`,
      { method: "POST" },
    ),

  /** Puts the server's own sources back. */
  revertSources: (id: string) =>
    request<CommandDetail>(`/api/servers/${encodeURIComponent(id)}/sources`, {
      method: "DELETE",
    }),

  siteSources: (id: string, signal?: AbortSignal) =>
    request<SiteSourcesList>(`/api/sites/${encodeURIComponent(id)}/sources`, {
      signal,
    }),

  useMirrorOnSite: (id: string) =>
    request<SiteSourcesChange>(
      `/api/sites/${encodeURIComponent(id)}/sources/use-mirror`,
      { method: "POST" },
    ),

  revertSiteSources: (id: string) =>
    request<SiteSourcesChange>(`/api/sites/${encodeURIComponent(id)}/sources`, {
      method: "DELETE",
    }),

  /** One page of the server's commands, newest first. */
  serverCommands: (
    serverId: string,
    query: { limit?: number; cursor?: string } = {},
    signal?: AbortSignal,
  ) => {
    const params = new URLSearchParams();
    if (query.limit !== undefined) params.set("limit", String(query.limit));
    if (query.cursor !== undefined) params.set("cursor", query.cursor);
    const search = params.toString();
    return request<CommandList>(
      `/api/servers/${encodeURIComponent(serverId)}/commands${search ? `?${search}` : ""}`,
      { signal },
    );
  },

  /** The server's system logs, newest first (ADR-0091). */
  serverLogs: (serverId: string, query: LogQuery = {}, signal?: AbortSignal) => {
    const params = new URLSearchParams();
    if (query.start) params.set("start", query.start.toISOString());
    if (query.end) params.set("end", query.end.toISOString());
    if (query.level) params.set("level", query.level);
    if (query.unit) params.set("unit", query.unit);
    if (query.q) params.set("q", query.q);
    if (query.limit !== undefined) params.set("limit", String(query.limit));
    const search = params.toString();
    return request<LogList>(
      `/api/servers/${encodeURIComponent(serverId)}/logs${search ? `?${search}` : ""}`,
      { signal },
    );
  },

  agentReleases: (signal?: AbortSignal) =>
    request<AgentReleases>("/api/agent-releases", { signal }),

  /** Imports a release from its files; global administrators only (ADR-0061). */
  importRelease: (files: File[]) => {
    const form = new FormData();
    for (const file of files) form.append("files", file, file.name);
    return request<ReleaseImported>("/api/agent-releases/import", {
      method: "POST",
      form,
    });
  },
  importReleaseUrl: (url: string) =>
    request<ReleaseImported>("/api/agent-releases/import-url", {
      method: "POST",
      body: { url },
    }),

  /** Server updates; global administrators only (ADR-0056). */
  serverUpdate: (signal?: AbortSignal) =>
    request<ServerUpdateView>("/api/server-update", { signal }),

  /** The server's updates, newest first (ADR-0172). */
  serverUpdateHistory: (signal?: AbortSignal) =>
    request<{ updates: ServerUpdateHistoryEntry[] }>("/api/server-update/history", { signal }),

  license: (signal?: AbortSignal) => request<LicenseView>("/api/license", { signal }),

  /** Installs a license key in place of any other; global administrators only, audited. */
  installLicense: (text: string) =>
    request<LicenseView>("/api/license", { method: "PUT", body: { text } }),

  /** Removes the license key; central servers already added keep running. Audited. */
  removeLicense: () => request<void>("/api/license", { method: "DELETE" }),

  /** Frees the place of an added central server; audited. */
  removeAddedServer: (name: string) =>
    request<void>(`/api/license/servers/${encodeURIComponent(name)}`, { method: "DELETE" }),

  /** Moving from another monitoring tool (ADR-0146): the tools an import reads. */
  importSources: (signal?: AbortSignal) =>
    request<{ sources: ImportSource[] }>("/api/imports/sources", { signal }),
  /** The latest imports of the sites the user administers, without their items. */
  imports: (signal?: AbortSignal) => request<{ imports: ImportRun[] }>("/api/imports", { signal }),
  /** Reads files or connects, and stores a preview; creates nothing else. */
  previewImport: (body: ImportRequest) => request<ImportRun>("/api/imports", { method: "POST", body }),
  importRun: (id: string, signal?: AbortSignal) =>
    request<ImportRun>(`/api/imports/${encodeURIComponent(id)}`, { signal }),
  /** Creates the preview's items but `skip` and what needs them; audited. */
  confirmImport: (id: string, skip: string[]) =>
    request<ImportRun>(`/api/imports/${encodeURIComponent(id)}/confirm`, { method: "POST", body: { skip } }),
  /** Removes what the import created, but servers an agent took over; audited. */
  undoImport: (id: string) =>
    request<ImportRun>(`/api/imports/${encodeURIComponent(id)}/undo`, { method: "POST" }),

  backups: (signal?: AbortSignal) =>
    request<BackupsView>("/api/backups", { signal }),
  startBackup: () =>
    request<{ id: string }>("/api/backups", { method: "POST" }),
  checkServerUpdate: () =>
    request<ServerUpdateCheck>("/api/server-update/check", { method: "POST" }),

  /** The certificate authority and its changes; global administrators only (ADR-0088). */
  certificateAuthority: (signal?: AbortSignal) =>
    request<CertificateAuthorityView>("/api/certificate-authority", { signal }),
  startCertificateAuthorityChange: (emergency: boolean) =>
    request<CertificateAuthorityView>("/api/certificate-authority/rotation", {
      method: "POST",
      body: { emergency },
    }),
  nextCertificateAuthorityStep: () =>
    request<CertificateAuthorityView>("/api/certificate-authority/rotation/next", {
      method: "POST",
      body: {},
    }),

  startServerUpdate: (version: string) =>
    request<{ id: string }>("/api/server-update", {
      method: "POST",
      body: { version },
    }),

  /** Queues a `self_update` to `version`; customer administrators only (ADR-0053). */
  /** Confirms the names for a waiting relay's certificate; administrators of its customer (ADR-0066). */
  confirmRelayNames: (relayId: string, names: string[]) =>
    request<void>(`/api/relays/${encodeURIComponent(relayId)}/names`, {
      method: "POST",
      body: { names },
    }),

  /** Queues a relay self-update; administrators of its customer (ADR-0055). */
  updateRelay: (relayId: string, version: string) =>
    request<RelayCommand>(`/api/relays/${encodeURIComponent(relayId)}/update`, {
      method: "POST",
      body: { version },
    }),

  /** Turns AI commands on or off from the web interface; site administrators (ADR-0098). */
  switchAiCommands: (serverId: string, on: boolean) =>
    request<CommandDetail>(`/api/servers/${encodeURIComponent(serverId)}/ai-commands`, {
      method: "POST",
      body: { on },
    }),

  /** The same for every server of every site the user administers. */
  switchAllAiCommands: (on: boolean) =>
    request<SiteSourcesChange>("/api/remediation/ai-commands", { method: "POST", body: { on } }),

  /** The same for every server of a site that can take it. */
  switchSiteAiCommands: (siteId: string, on: boolean) =>
    request<SiteSourcesChange>(`/api/sites/${encodeURIComponent(siteId)}/ai-commands`, {
      method: "POST",
      body: { on },
    }),

  updateAgent: (serverId: string, version: string) =>
    request<CommandDetail>(
      `/api/servers/${encodeURIComponent(serverId)}/commands`,
      {
        method: "POST",
        body: { kind: "self_update", version },
      },
    ),

  command: (id: string, signal?: AbortSignal) =>
    request<CommandDetail>(`/api/commands/${encodeURIComponent(id)}`, {
      signal,
    }),

  /** The remediation incidents of a threshold incident, one per attempt, newest first. */
  remediationIncidents: (alertIncidentId: string, signal?: AbortSignal) =>
    request<RemediationIncidentList>(
      `/api/remediation/incidents?alert_incident_id=${encodeURIComponent(alertIncidentId)}&limit=${MAX_ATTEMPTS_SHOWN}`,
      { signal },
    ),

  /** Whether the user may run AI remediation again on an alert, and why not (ADR-0165). */
  remediationRunStatus: (alertIncidentId: string, signal?: AbortSignal) =>
    request<RemediationRunStatus>(`/api/remediation/alerts/${encodeURIComponent(alertIncidentId)}`, { signal }),

  /** Starts a new AI remediation of an open alert; 409 with the reason when it cannot. */
  runRemediation: (alertIncidentId: string) =>
    request<RemediationRunStarted>(`/api/remediation/alerts/${encodeURIComponent(alertIncidentId)}/run`, {
      method: "POST",
    }),

  /** Every command waiting for approval in the user's sites, nearest deadline first. */
  approvals: (signal?: AbortSignal) =>
    request<PendingApprovalList>("/api/remediation/approvals", { signal }),

  /** Runs the pending command, or `command` instead when given; 409 once decided or past the deadline. */
  approve: (id: string, command?: string) =>
    request<ApprovalDecision>(
      `/api/remediation/approvals/${encodeURIComponent(id)}/approve`,
      {
        method: "POST",
        body: command === undefined ? {} : { command },
      },
    ),

  /** Drops the pending command; the reason goes back to the AI. */
  reject: (id: string, reason: string) =>
    request<ApprovalDecision>(
      `/api/remediation/approvals/${encodeURIComponent(id)}/reject`,
      {
        method: "POST",
        body: { reason },
      },
    ),

  /** One remediation incident with its events and verified chain. */
  remediationIncident: (id: string, signal?: AbortSignal) =>
    request<RemediationIncidentDetail>(
      `/api/remediation/incidents/${encodeURIComponent(id)}`,
      { signal },
    ),

  /** Remediation events across incidents, newest first. */
  remediationEvents: (query: RemediationEventQuery, signal?: AbortSignal) => {
    const params = new URLSearchParams();
    for (const [key, value] of Object.entries(query)) {
      if (value !== undefined) params.set(key, String(value));
    }
    const search = params.toString();
    return request<RemediationEventList>(
      search ? `/api/remediation/events?${search}` : "/api/remediation/events",
      {
        signal,
      },
    );
  },

  /** Dashboards the user sees, by name (ADR-0095). */
  dashboards: (signal?: AbortSignal) =>
    request<{ dashboards: DashboardSummary[] }>("/api/dashboards", { signal }),

  dashboard: (id: string, signal?: AbortSignal) =>
    request<Dashboard>(`/api/dashboards/${encodeURIComponent(id)}`, { signal }),

  createDashboard: (body: DashboardRequest) =>
    request<Dashboard>("/api/dashboards", { method: "POST", body }),

  /** 409 when someone saved since `body.revision`. */
  updateDashboard: (id: string, body: DashboardRequest) =>
    request<Dashboard>(`/api/dashboards/${encodeURIComponent(id)}`, { method: "PUT", body }),

  deleteDashboard: (id: string) =>
    request<void>(`/api/dashboards/${encodeURIComponent(id)}`, { method: "DELETE" }),

  /** Every panel at once, for the servers the user reads within `within`. */
  dashboardData: (body: { range: TimeRangeLabel; within: Within; panels: Panel[] }, signal?: AbortSignal) =>
    request<DashboardData>("/api/dashboard-data", { method: "POST", body, signal }),

  /** The display links showing the dashboard `id` (ADR-0099). */
  displayLinks: (id: string, signal?: AbortSignal) =>
    request<{ links: DisplayLink[] }>(`/api/dashboards/${encodeURIComponent(id)}/display-links`, { signal }),

  /** A new display link; `url` holds its secret, shown only now. */
  createDisplayLink: (body: DisplayLinkRequest) =>
    request<DisplayLink & { url: string }>("/api/display-links", { method: "POST", body }),

  /** Turns a display link off; its screens stop at their next request. */
  revokeDisplayLink: (id: string) =>
    request<void>(`/api/display-links/${encodeURIComponent(id)}`, { method: "DELETE" }),

  /** The API keys the user could have made (ADR-0101). */
  apiKeys: (signal?: AbortSignal) => request<{ keys: ApiKey[] }>("/api/api-keys", { signal }),

  /** A new API key; `secret` is shown only now. */
  createApiKey: (body: ApiKeyRequest) =>
    request<{ key: ApiKey; secret: string }>("/api/api-keys", { method: "POST", body }),

  /** Turns an API key off; it stops at its next request. */
  revokeApiKey: (id: string) => request<void>(`/api/api-keys/${encodeURIComponent(id)}`, { method: "DELETE" }),

  /** The OpenAPI document of the public API; no session needed. */
  openApi: (signal?: AbortSignal) => request<OpenApiDocument>("/api/v1/openapi.json", { signal }),

  /** Trades a display link's secret for a screen session cookie. No session needed. */
  startDisplay: (token: string, signal?: AbortSignal) =>
    request<ScreenView>("/api/display/start", { method: "POST", body: { token }, signal, redirectOn401: false }),

  /** What the screen shows now, with its screen session. */
  display: (signal?: AbortSignal) => request<ScreenView>("/api/display", { signal, redirectOn401: false }),

  /** One of the link's dashboards, read as the link's maker. */
  displayData: (dashboardId: string, signal?: AbortSignal) =>
    request<DashboardData>("/api/display/data", {
      method: "POST",
      body: { dashboard_id: dashboardId },
      signal,
      redirectOn401: false,
    }),

  dashboardCatalog: (query: { siteId?: string; customerId?: string; metric?: string }, signal?: AbortSignal) => {
    const params = new URLSearchParams();
    if (query.siteId) params.set("site_id", query.siteId);
    if (query.customerId) params.set("customer_id", query.customerId);
    if (query.metric) params.set("metric", query.metric);
    const search = params.toString();
    return request<DashboardCatalog>(`/api/dashboard-metrics${search ? `?${search}` : ""}`, { signal });
  },

  /** The latest value of each series of `metric`, as of now. */
  queryInstant: (
    query: { serverId: string; metric: string; instance?: string; lookbackS?: number },
    signal?: AbortSignal,
  ) => {
    const params = new URLSearchParams({
      server_id: query.serverId,
      metric: query.metric,
    });
    if (query.instance !== undefined) params.set("instance", query.instance);
    if (query.lookbackS !== undefined) params.set("lookback", String(query.lookbackS));
    return request<QueryResult>(`/api/metrics/query?${params}`, { signal });
  },

  queryRange: (query: RangeQuery, signal?: AbortSignal) => {
    const params = new URLSearchParams({
      server_id: query.serverId,
      metric: query.metric,
    });
    if (query.instance !== undefined) params.set("instance", query.instance);
    params.set("start", query.start.toISOString());
    params.set("end", query.end.toISOString());
    params.set("step", String(query.step));
    if (query.agg) params.set("agg", query.agg);
    if (query.rollup) params.set("rollup", query.rollup);
    return request<QueryResult>(`/api/metrics/query_range?${params}`, {
      signal,
    });
  },
};

/** The parts of an OpenAPI 3.1 document the documentation page reads (ADR-0101). */
export interface OpenApiDocument {
  info: { title: string; version: string; description?: string };
  paths: Record<string, Partial<Record<"get" | "post" | "put" | "patch" | "delete", OpenApiOperation>>>;
  components?: { schemas?: Record<string, OpenApiSchema> };
  tags?: { name: string; description?: string }[];
}

export interface OpenApiOperation {
  tags?: string[];
  summary?: string;
  description?: string;
  parameters?: { name: string; in: string; required?: boolean; description?: string; schema?: OpenApiSchema }[];
  requestBody?: { content?: Record<string, { schema?: OpenApiSchema }> };
  responses?: Record<string, { description?: string; content?: Record<string, { schema?: OpenApiSchema }> }>;
}

export interface OpenApiSchema {
  $ref?: string;
  type?: string | string[];
  format?: string;
  description?: string;
  enum?: unknown[];
  items?: OpenApiSchema;
  properties?: Record<string, OpenApiSchema>;
  required?: string[];
  oneOf?: OpenApiSchema[];
  allOf?: OpenApiSchema[];
}
