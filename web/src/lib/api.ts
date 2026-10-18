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
}

export interface ServerList {
  servers: Server[];
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

  servers: (signal?: AbortSignal) => request<ServerList>("/api/servers", { signal }),
};
