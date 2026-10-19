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

/** A role granted on one site, or on every site when `site_id` is null. */
export interface RoleGrant {
  role: Role;
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
  method?: "GET" | "POST";
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

  serverSensors: (serverId: string, signal?: AbortSignal) =>
    request<EffectiveSensors>(`/api/servers/${encodeURIComponent(serverId)}/sensors`, { signal }),

  sites: (signal?: AbortSignal) => request<SiteList>("/api/sites", { signal }),

  incidents: (query: IncidentQuery, signal?: AbortSignal) => {
    const params = new URLSearchParams();
    for (const [key, value] of Object.entries(query)) {
      if (value !== undefined) params.set(key, String(value));
    }
    const search = params.toString();
    return request<IncidentList>(search ? `/api/incidents?${search}` : "/api/incidents", { signal });
  },

  serverThresholds: (serverId: string, signal?: AbortSignal) =>
    request<EffectiveThresholds>(`/api/servers/${encodeURIComponent(serverId)}/thresholds/effective`, {
      signal,
    }),

  serverMetrics: (serverId: string, signal?: AbortSignal) =>
    request<MetricList>(`/api/servers/${encodeURIComponent(serverId)}/metrics`, { signal }),

  queryRange: (query: RangeQuery, signal?: AbortSignal) => {
    const params = new URLSearchParams({ server_id: query.serverId, metric: query.metric });
    if (query.instance !== undefined) params.set("instance", query.instance);
    params.set("start", query.start.toISOString());
    params.set("end", query.end.toISOString());
    params.set("step", String(query.step));
    if (query.agg) params.set("agg", query.agg);
    return request<QueryResult>(`/api/metrics/query_range?${params}`, { signal });
  },
};
