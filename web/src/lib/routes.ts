import { type IncidentFilters, NO_FILTERS, filtersToQuery } from "./incidents";
import { shortMetric } from "./metrics";

/** Link to the page of the server `id` (a query parameter, ADR-0020), on `tab` when given. */
export function serverHref(id: string, tab?: "sensors" | "updates" | "commands" | "logs"): string {
  return `/servers/view/?id=${encodeURIComponent(id)}${tab ? `&tab=${tab}` : ""}`;
}

/** Margin around an incident in its logs link: 15 minutes. */
const LOG_MARGIN_MS = 15 * 60 * 1000;

/**
 * Link to the logs of the server `id` around an incident: from 15 minutes
 * before it opened to 15 minutes after it resolved, or to now while open.
 */
export function incidentLogsHref(id: string, openedAt: string, resolvedAt: string | null): string {
  const params = new URLSearchParams({ id, tab: "logs" });
  params.set("from", new Date(Date.parse(openedAt) - LOG_MARGIN_MS).toISOString());
  if (resolvedAt) params.set("to", new Date(Date.parse(resolvedAt) + LOG_MARGIN_MS).toISOString());
  return `/servers/view/?${params}`;
}

/** Link to the page of the site `id`: its alerts and checks. */
export function siteHref(id: string): string {
  return `/sites/view/?id=${encodeURIComponent(id)}`;
}

/**
 * Link to the page of one sensor's metric on the server `id`; `instance`
 * "" is none. `edit` opens its Configuration card's form.
 */
export function sensorHref(id: string, metric: string, instance = "", edit = false): string {
  const params = new URLSearchParams({ id, sensor: shortMetric(metric) });
  if (instance) params.set("instance", instance);
  if (edit) params.set("edit", "1");
  return `/servers/sensor/?${params}`;
}

/** Link to the server or relay of an incident. */
export function incidentPeerHref(incident: { server_id: string | null; relay_id?: string | null }): string {
  return incident.server_id ? serverHref(incident.server_id) : relayHref(incident.relay_id ?? "");
}

/** Link to the incidents list with `filters` (see `lib/incidents.ts`). */
export function incidentsHref(filters: Partial<IncidentFilters> = {}): string {
  const query = filtersToQuery({ ...NO_FILTERS, ...filters });
  return query ? `/incidents/?${query}` : "/incidents/";
}

/** Link to the page of the incident `id` (a query parameter, like servers). */
export function incidentHref(id: string): string {
  return `/incidents/view/?id=${encodeURIComponent(id)}`;
}

/** Dashboards (ADR-0095). */
export const DASHBOARDS_PATH = "/dashboards/";

/** Link to the dashboard `id`. */
export function dashboardHref(id: string): string {
  return `/dashboards/view/?id=${encodeURIComponent(id)}`;
}

/** Link to a ready-made dashboard, like `overview`. */
export function readyMadeHref(id: string): string {
  return `/dashboards/view/?ready=${encodeURIComponent(id)}`;
}

/** Active and upcoming silences (ADR-0079). */
export const SILENCES_PATH = "/silences/";

/** Who is on call, and one's own contact (ADR-0081). */
export const ON_CALL_PATH = "/on-call/";

/**
 * Settings: one page listing the others, which live under it. They were
 * under `/admin/` before; `LEGACY_SETTINGS_PATHS` redirects those.
 */
export const SETTINGS_PATH = "/settings/";
/** The signed-in person's own preferences, for everyone (ADR-0137). */
export const PREFERENCES_PATH = "/settings/preferences/";
/** The sidebar part of it, which the rail links to. */
export const SIDEBAR_PREFERENCES_PATH = "/settings/preferences/#sidebar";
/** How this server runs: address, sign-in, packages, retention (ADR-0069). */
export const GENERAL_SETTINGS_PATH = "/settings/general/";

/** Settings pages (ADR-0029). */
export const RELAYS_PATH = "/settings/relays/";
export const SITES_PATH = "/settings/sites/";
/** Install commands and single-use tokens. */
export const TOKENS_PATH = "/settings/add-servers/";
export const USERS_PATH = "/settings/users/";
/** API keys and the public API's documentation (ADR-0101). */
export const API_KEYS_PATH = "/settings/api-keys/";
export const API_DOCS_PATH = "/settings/api-keys/docs/";

/** Email, chat and webhook notifications (ADR-0060, ADR-0077). */
export const NOTIFICATIONS_PATH = "/settings/notifications/";
/** Signed events for other tools, like a PSA (ADR-0102). */
export const INTEGRATIONS_PATH = "/settings/integrations/";
/** AI setup, global administrators only (ADR-0064). */
export const AI_SETTINGS_PATH = "/settings/ai/";

/** Updating the central server (ADR-0056). */
export const SERVER_UPDATE_PATH = "/settings/server-update/";
/** Backups of the server (ADR-0058). */
export const BACKUPS_PATH = "/settings/backups/";
/** Plugins: installing them and trusting their publishers (ADR-0103). */
export const PLUGINS_PATH = "/settings/plugins/";
/** Changing the certificate authority of the server (ADR-0088). */
export const CERTIFICATE_AUTHORITY_PATH = "/settings/certificate-authority/";
/** The Enterprise license key (ADR-0100). */
export const LICENSE_PATH = "/settings/license/";

/** Package mirrors and automatic updates stay under `/admin/`: they belong to Updates, not Settings. */
export const MIRRORS_PATH = "/admin/mirrors/";
/** Automatic agent and relay updates (ADR-0055). */
export const AUTO_UPDATE_PATH = "/admin/updates/";

/**
 * Where each settings page was before it moved under `/settings/`: links in
 * old notifications, bookmarks and guides still lead there, and a static
 * export has no server redirects, so each keeps a page that redirects.
 */
export const LEGACY_SETTINGS_PATHS: Record<string, string> = {
  "/admin/settings/": GENERAL_SETTINGS_PATH,
  "/admin/relays/": RELAYS_PATH,
  "/admin/relays/view/": "/settings/relays/view/",
  "/admin/sites/": SITES_PATH,
  "/admin/tokens/": TOKENS_PATH,
  "/admin/users/": USERS_PATH,
  "/admin/api-keys/": API_KEYS_PATH,
  "/admin/api-keys/docs/": API_DOCS_PATH,
  "/admin/notifications/": NOTIFICATIONS_PATH,
  "/admin/integrations/": INTEGRATIONS_PATH,
  "/admin/ai/": AI_SETTINGS_PATH,
  "/admin/server-update/": SERVER_UPDATE_PATH,
  "/admin/backups/": BACKUPS_PATH,
  "/admin/plugins/": PLUGINS_PATH,
  "/admin/certificate-authority/": CERTIFICATE_AUTHORITY_PATH,
  "/admin/license/": LICENSE_PATH,
};

/** The new place of a moved settings `pathname`, its query and hash kept; null when it did not move. */
export function movedSettingsHref(pathname: string, search = "", hash = ""): string | null {
  const path = pathname.endsWith("/") ? pathname : `${pathname}/`;
  const to = LEGACY_SETTINGS_PATHS[path];
  return to ? `${to}${search}${hash}` : null;
}

/** Where the browser downloads the backup `name`: a plain GET with the session cookie. */
export function backupDownloadHref(name: string): string {
  return `/api/backups/${encodeURIComponent(name)}`;
}

/** Link to the page of the mirror `id`. */
export function mirrorHref(id: string): string {
  return `/admin/mirrors/view/?id=${encodeURIComponent(id)}`;
}

/** Link to the page of the relay `id`. */
export function relayHref(id: string): string {
  return `/settings/relays/view/?id=${encodeURIComponent(id)}`;
}

/** Update campaigns (ADR-0033). */
export const CAMPAIGNS_PATH = "/campaigns/";
export const NEW_CAMPAIGN_PATH = "/campaigns/new/";

/** Link to the page of the campaign `id`. */
export function campaignHref(id: string): string {
  return `/campaigns/view/?id=${encodeURIComponent(id)}`;
}

/** The approval queue and the remediation audit log (ADR-0045). */
export const APPROVALS_PATH = "/remediation/approvals/";
export const AUDIT_PATH = "/remediation/audit/";

/** Link to the remediation audit log, filtered to one incident when given. */
export function auditHref(filters: { incident?: string } = {}): string {
  return filters.incident ? `${AUDIT_PATH}?incident=${encodeURIComponent(filters.incident)}` : AUDIT_PATH;
}
