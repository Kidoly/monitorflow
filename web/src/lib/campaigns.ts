// Campaign helpers without React (ADR-0033): the creation form's model and
// the checks the API makes, so most mistakes show before a request; the
// state machine for the action buttons; and display strings.

import { compareVersions } from "./agentVersion";
import { browserTimeZone, localToIso, toLocalInput } from "./clock";
import type { Tone } from "@/components/ui/tone";
import { t } from "@/i18n";
import { activeLocale } from "@/i18n/locale";

import type {
  Campaign,
  CampaignAction,
  CampaignDetail,
  CampaignServerStatus,
  CampaignRequest,
  CampaignState,
  CampaignWindow,
  FailureThreshold,
  Server,
  SourcesPin,
  ThirdPartyManagerName,
  UpdateSpec,
  WaveSpec,
} from "./api";
import { formatDateTime, formatOs } from "./format";
import { shortTime } from "./mirrors";

/** Limits of `crates/server/src/campaign/model.rs` and the API. */
export const MAX_NAME_LEN = 64;
export const MAX_TARGETS = 10_000;
export const MAX_WAVES = 100;
export const MAX_WINDOWS = 32;
export const MAX_PACKAGES = 1000;
export const MIN_TTL_S = 60;
export const MAX_TTL_S = 7 * 86_400;

/** ISO weekdays, Monday 1 to Sunday 7. */
export const WEEKDAYS = [1, 2, 3, 4, 5, 6, 7] as const;

/** The short name of ISO weekday `day` in the active language: "Mon", "lun.". */
export function weekdayName(day: number): string {
  if (!(Number.isInteger(day) && day >= 1 && day <= 7)) return String(day);
  // 5 January 2026 is a Monday. American English says "Tue", where British English may say "Tues".
  const locale = activeLocale() === "en" ? "en-US" : activeLocale();
  return new Date(2026, 0, 4 + day).toLocaleString(locale, { weekday: "short" });
}

export interface WaveDraft {
  kind: "size" | "percent" | "servers";
  /** Size or percentage, as typed. */
  value: string;
  servers: string[];
}

export interface WindowDraft {
  kind: "weekly" | "once";
  weekdays: number[];
  start: string;
  end: string;
  timezone: string;
  /** `datetime-local` values, in the browser's time zone. */
  startsAt: string;
  endsAt: string;
}

/** `agent` updates the agents themselves (ADR-0053). */
export type UpdateKind = "all" | "security" | "packages" | "agent";

/** Which updates a package campaign installs (ADR-0138): the OS's, third-party apps', both, or not yet chosen. */
export type UpdateSources = "os" | "apps" | "both" | "none";

/** The sources of two independent choices: OS updates, third-party apps. */
export function updateSources(os: boolean, apps: boolean): UpdateSources {
  return os ? (apps ? "both" : "os") : apps ? "apps" : "none";
}

export const includesOs = (sources: UpdateSources): boolean => sources === "os" || sources === "both";
export const includesApps = (sources: UpdateSources): boolean => sources === "apps" || sources === "both";

/** Third-party managers, Windows first. */
export const THIRD_PARTY_MANAGERS: ThirdPartyManagerName[] = ["winget", "flatpak", "snap"];

/** A manager's name on screen. */
export const MANAGER_NAMES: Record<ThirdPartyManagerName, string> = { winget: "winget", flatpak: "Flatpak", snap: "Snap" };

/** Whether `manager` runs on servers of `osFamily`: winget on Windows, Flatpak and Snap on Linux. */
export const managerFits = (manager: ThirdPartyManagerName, osFamily: string): boolean =>
  (manager === "winget") === (osFamily === "windows");

/** What the creation form edits. */
export interface CampaignDraft {
  customerId: string;
  name: string;
  /** Empty: no site, only `serverIds`. */
  siteId: string;
  serverIds: string[];
  waves: WaveDraft[];
  update: UpdateKind;
  /** Package names separated by spaces, commas or lines. */
  packages: string;
  /** OS updates by default; third-party apps on request. */
  sources: UpdateSources;
  /** The third-party managers updated, unless `sources` is `os`. */
  managers: ThirdPartyManagerName[];
  /** For `agent`: the version to update to. */
  agentVersion: string;
  allowReboot: boolean;
  ttl: number;
  thresholdValue: string;
  thresholdUnit: FailureThreshold["unit"];
  windows: WindowDraft[];
  /** Pinned snapshots of the campaign edited or copied, kept as they are: the form does not edit them. */
  pin?: SourcesPin;
}

export function newWindow(kind: WindowDraft["kind"] = "weekly"): WindowDraft {
  return { kind, weekdays: [6, 7], start: "22:00", end: "04:00", timezone: browserTimeZone(), startsAt: "", endsAt: "" };
}

export function emptyDraft(customerId = ""): CampaignDraft {
  return {
    customerId,
    name: "",
    siteId: "",
    serverIds: [],
    waves: [],
    update: "all",
    packages: "",
    sources: "os",
    managers: [...THIRD_PARTY_MANAGERS],
    agentVersion: "",
    allowReboot: false,
    ttl: 3600,
    thresholdValue: "0",
    thresholdUnit: "count",
    windows: [],
  };
}

export function toRequest(draft: CampaignDraft): CampaignRequest {
  const waves: WaveSpec[] = draft.waves.map((wave) =>
    wave.kind === "servers"
      ? { servers: wave.servers }
      : wave.kind === "size"
        ? { size: Number(wave.value) }
        : { percent: Number(wave.value) },
  );
  const windows: CampaignWindow[] = draft.windows.map((w) =>
    w.kind === "weekly"
      ? { kind: "weekly", weekdays: [...w.weekdays].sort((a, b) => a - b), start: w.start, end: w.end, timezone: w.timezone.trim() }
      : { kind: "once", starts_at: localToIso(w.startsAt) ?? "", ends_at: localToIso(w.endsAt) ?? "" },
  );
  const agent = draft.update === "agent";
  const os = agent || includesOs(draft.sources);
  const apps = !agent && includesApps(draft.sources);
  return {
    customer_id: draft.customerId,
    name: draft.name,
    ...(draft.siteId && { site_id: draft.siteId }),
    server_ids: draft.serverIds,
    waves,
    packages: os && draft.update === "packages" ? draft.packages.split(/[\s,]+/).filter(Boolean) : [],
    security_only: os && draft.update === "security",
    allow_reboot: draft.update !== "agent" && draft.allowReboot,
    ...(draft.update === "agent" && { agent_version: draft.agentVersion }),
    ...(!os && { include_os: false }),
    ...(apps && { third_party: draft.managers.map((manager) => ({ manager, packages: [] })) }),
    command_ttl_s: draft.ttl,
    failure_threshold: { value: Number(draft.thresholdValue), unit: draft.thresholdUnit },
    windows,
    ...(draft.pin && os && !agent && { sources: draft.pin }),
  };
}

/** Whether the API lets `campaign` be edited: nothing queued yet, not finished (ADR-0166). */
export function canEdit(campaign: Pick<Campaign, "state" | "current_wave">): boolean {
  return !isFinal(campaign.state) && campaign.current_wave === null;
}

/** Whether the API may delete `campaign`: a draft or a finished one, once its commands ended. */
export function canDelete(campaign: Pick<Campaign, "state" | "progress">): boolean {
  return campaign.state === "draft" || (isFinal(campaign.state) && campaign.progress.in_progress === 0);
}

/** A `datetime-local` value of an ISO time, in the browser's time zone; "" when invalid. */
function isoToLocal(iso: string): string {
  const date = new Date(iso);
  return Number.isNaN(date.getTime()) ? "" : toLocalInput(date);
}

/**
 * The form's draft for `campaign`, to edit it or to start a copy. Without
 * the request it was made from (before ADR-0166), its servers outside its
 * site are named and its batches become sizes.
 */
export function draftFromCampaign(campaign: CampaignDetail): CampaignDraft {
  const { spec } = campaign;
  const targets = campaign.waves.flatMap((w) => w.servers);
  const serverIds =
    campaign.server_ids ?? targets.filter((s) => s.site_id !== campaign.site_id).map((s) => s.server_id);
  const specs: WaveSpec[] =
    campaign.wave_specs ?? campaign.waves.slice(0, -1).map((w) => ({ size: w.servers.length }));
  const third = spec.third_party ?? [];
  return {
    customerId: campaign.customer_id,
    name: campaign.name,
    siteId: campaign.site_id ?? "",
    serverIds,
    waves: specs.map((w): WaveDraft =>
      "servers" in w
        ? { kind: "servers", value: "1", servers: w.servers }
        : "size" in w
          ? { kind: "size", value: String(w.size), servers: [] }
          : { kind: "percent", value: String(w.percent), servers: [] },
    ),
    update: campaign.agent_version
      ? "agent"
      : spec.packages.length > 0
        ? "packages"
        : spec.security_only
          ? "security"
          : "all",
    packages: spec.packages.join(" "),
    sources: spec.include_os === false ? "apps" : third.length > 0 ? "both" : "os",
    managers:
      third.length > 0
        ? THIRD_PARTY_MANAGERS.filter((m) => third.some((s) => s.manager === m))
        : [...THIRD_PARTY_MANAGERS],
    agentVersion: campaign.agent_version ?? "",
    allowReboot: spec.allow_reboot,
    ttl: campaign.command_ttl_s,
    thresholdValue: String(campaign.failure_threshold.value),
    thresholdUnit: campaign.failure_threshold.unit,
    windows: campaign.windows.map((w): WindowDraft =>
      w.kind === "weekly"
        ? { ...newWindow("weekly"), weekdays: w.weekdays, start: w.start, end: w.end, timezone: w.timezone }
        : { ...newWindow("once"), startsAt: isoToLocal(w.starts_at), endsAt: isoToLocal(w.ends_at) },
    ),
    ...(campaign.sources && { pin: campaign.sources }),
  };
}

/** The name of a copy: "Copy of November updates", cut to the longest name the API takes. */
export function copyName(name: string): string {
  return [...t("campaigns.copyName", { name })].slice(0, MAX_NAME_LEN).join("").trim();
}

/**
 * The servers a campaign would target, in the API's order: hostname, then
 * peer ID. Only an agent update reaches hosts that take no package updates.
 * Without OS updates (`appsOnly`, the managers chosen), only servers of a
 * platform one of them is for.
 */
export function campaignTargets(
  servers: Server[],
  customerId: string,
  siteId: string,
  serverIds: string[],
  agentUpdate = false,
  appsOnly: ThirdPartyManagerName[] | null = null,
): Server[] {
  const named = new Set(serverIds);
  return servers
    .filter(
      (s) =>
        s.customer_id === customerId &&
        s.revoked_at === null &&
        (appsOnly
          ? appsOnly.some((m) => managerFits(m, s.os_family))
          : agentUpdate || s.package_updates !== false) &&
        ((siteId && s.site_id === siteId) || named.has(s.id)),
    )
    .sort((a, b) => cmp(a.hostname, b.hostname) || cmp(a.peer_id, b.peer_id));
}

/**
 * The servers matching `query`: every word of it, in any case, is part of
 * the host name, the OS or the site name. An empty query matches all.
 */
export function searchServers(servers: Server[], query: string): Server[] {
  const words = query.toLowerCase().split(/\s+/).filter(Boolean);
  if (words.length === 0) return servers;
  return servers.filter((s) => {
    const text = [s.hostname, formatOs(s.os_family, s.os_version), s.os_family, s.site_name].join(" ").toLowerCase();
    return words.every((w) => text.includes(w));
  });
}

function cmp(a: string, b: string): number {
  return a < b ? -1 : a > b ? 1 : 0;
}

/** `plan_waves` of the server: explicit servers first, then sizes and percentages, the rest last. */
export function planWaves(targets: string[], specs: WaveSpec[]): { waves: string[][] } | { error: string } {
  const known = new Set(targets);
  const placed = new Set<string>();
  for (const [index, spec] of specs.entries()) {
    if (!("servers" in spec)) continue;
    for (const server of spec.servers) {
      if (!known.has(server)) return { error: t("campaigns.plan.notTarget", { batch: index + 1 }) };
      if (placed.has(server)) return { error: t("campaigns.plan.twice") };
      placed.add(server);
    }
  }
  const pool = targets.filter((t) => !placed.has(t));
  const waves: string[][] = [];
  for (const [index, spec] of specs.entries()) {
    let wave: string[];
    if ("servers" in spec) {
      wave = spec.servers;
    } else if ("size" in spec) {
      wave = pool.splice(0, spec.size);
    } else {
      if (!(spec.percent >= 1 && spec.percent <= 100)) return { error: t("campaigns.plan.percent") };
      wave = pool.splice(0, Math.ceil((targets.length * spec.percent) / 100));
    }
    if (wave.length === 0) return { error: t("campaigns.plan.empty", { batch: index + 1 }) };
    waves.push(wave);
  }
  if (pool.length > 0) waves.push(pool);
  if (waves.length > MAX_WAVES) return { error: t("campaigns.plan.tooMany", { max: MAX_WAVES }) };
  return { waves };
}

// As `valid_package_name` on the server: a trailing `-` would make apt remove the package.
const PACKAGE = /^[A-Za-z0-9](?:[A-Za-z0-9._+:-]*[A-Za-z0-9])?\+*$/;
const TIME = /^(?:[01]\d|2[0-3]):[0-5]\d$|^24:00$/;

function isCount(n: number, max: number): boolean {
  return Number.isInteger(n) && n >= 0 && n <= max;
}

function minutes(time: string): number {
  const [h, m] = time.split(":").map(Number);
  return h! * 60 + m!;
}

/**
 * The errors the API would answer for `request`, given the servers it
 * would target (from {@link campaignTargets}); empty when it looks valid.
 * The server still decides: time zone names and servers the user cannot
 * see are only checked there.
 */
export function validateCampaign(request: CampaignRequest, targets: string[]): string[] {
  const errors: string[] = [];
  const name = request.name;
  if ([...name].length < 1 || [...name].length > MAX_NAME_LEN || name.trim() !== name || /\p{Cc}/u.test(name)) {
    errors.push(t("campaigns.check.name", { max: MAX_NAME_LEN }));
  }
  if (!request.customer_id) errors.push(t("campaigns.check.customer"));
  if (!(request.command_ttl_s >= MIN_TTL_S && request.command_ttl_s <= MAX_TTL_S)) {
    errors.push(t("campaigns.check.ttl", { min: String(MIN_TTL_S), max: String(MAX_TTL_S) }));
  }
  if (request.agent_version !== undefined && compareVersions(request.agent_version, request.agent_version) === null) {
    errors.push(t("campaigns.check.agentVersion"));
  }
  if (request.include_os === false && request.third_party === undefined) errors.push(t("campaigns.check.sources"));
  if (request.third_party?.length === 0) errors.push(t("campaigns.check.managers"));
  if (request.packages.length > MAX_PACKAGES) errors.push(t("campaigns.check.packageCount", { max: String(MAX_PACKAGES) }));
  const badPackage = request.packages.find((p) => p.length > 128 || !PACKAGE.test(p));
  if (badPackage !== undefined) errors.push(t("campaigns.check.packageName", { name: badPackage }));
  const { value, unit } = request.failure_threshold;
  if (!isCount(value, unit === "percent" ? 100 : MAX_TARGETS)) {
    errors.push(
      unit === "percent"
        ? t("campaigns.check.thresholdPercent")
        : t("campaigns.check.thresholdCount", { max: String(MAX_TARGETS) }),
    );
  }
  if (request.windows.length > MAX_WINDOWS) errors.push(t("campaigns.check.windowCount", { max: MAX_WINDOWS }));
  request.windows.forEach((w, i) => {
    const windowName = t("campaigns.check.window", { number: i + 1 });
    if (w.kind === "weekly") {
      if (w.weekdays.length === 0 || w.weekdays.some((d) => !(Number.isInteger(d) && d >= 1 && d <= 7))) {
        errors.push(t("campaigns.check.weekday", { window: windowName }));
      }
      if (!TIME.test(w.start) || w.start === "24:00") errors.push(t("campaigns.check.start", { window: windowName }));
      if (!TIME.test(w.end)) errors.push(t("campaigns.check.end", { window: windowName }));
      // Midnight as an end is the end of the day.
      else if (TIME.test(w.start) && minutes(w.start) === (minutes(w.end) || 24 * 60)) {
        errors.push(t("campaigns.check.sameTime", { window: windowName }));
      }
      if (!w.timezone || w.timezone.length > 64) errors.push(t("campaigns.check.timezone", { window: windowName }));
    } else if (!w.starts_at || !w.ends_at) {
      errors.push(t("campaigns.check.startEnd", { window: windowName }));
    } else if (new Date(w.starts_at) >= new Date(w.ends_at)) {
      errors.push(t("campaigns.check.endAfter", { window: windowName }));
    }
  });
  if (!request.site_id && request.server_ids.length === 0) {
    errors.push(t("campaigns.check.target"));
  } else if (targets.length === 0) {
    errors.push(t("campaigns.check.noTarget"));
  } else if (targets.length > MAX_TARGETS) {
    errors.push(t("campaigns.check.serverCount", { max: String(MAX_TARGETS) }));
  }
  if (request.waves.some((w) => "size" in w && !isCount(w.size, MAX_TARGETS))) {
    errors.push(t("campaigns.check.batchSize"));
  } else if (request.waves.some((w) => "percent" in w && !Number.isInteger(w.percent))) {
    errors.push(t("campaigns.check.batchPercent"));
  } else if (targets.length > 0) {
    const plan = planWaves(targets, request.waves);
    if ("error" in plan) errors.push(t("campaigns.check.batches", { error: plan.error }));
  }
  return errors;
}

const ALLOWED: Record<CampaignAction, CampaignState[]> = {
  start: ["draft"],
  pause: ["scheduled", "running"],
  resume: ["paused"],
  stop: ["draft", "scheduled", "running", "paused"],
};

/** Whether the API accepts `action` on a campaign in `state` (`Action::next`). */
export function canAct(action: CampaignAction, state: CampaignState): boolean {
  return ALLOWED[action].includes(state);
}

export function isFinal(state: CampaignState): boolean {
  return state === "stopped" || state === "completed" || state === "failed";
}

/** "winget, Flatpak": the third-party managers of `spec`, or "" for none. */
export function describeManagers(spec: UpdateSpec): string {
  return (spec.third_party ?? []).map((s) => MANAGER_NAMES[s.manager] ?? s.manager).join(", ");
}

export function describeUpdate(spec: UpdateSpec, agentVersion: string | null = null): string {
  if (agentVersion) return t("campaigns.update.agent", { version: agentVersion });
  const os =
    spec.packages.length > 0
      ? t("campaigns.update.packages", { packages: spec.packages.join(", ") })
      : spec.security_only
        ? t("campaigns.update.security")
        : t("campaigns.update.all");
  const managers = describeManagers(spec);
  const what =
    spec.include_os === false
      ? t("campaigns.update.apps", { managers })
      : managers
        ? t("campaigns.update.osAndApps", { update: os, managers })
        : os;
  return spec.allow_reboot ? t("campaigns.update.reboot", { update: what }) : what;
}

export function describeThreshold({ value, unit }: FailureThreshold): string {
  if (unit === "percent") return t("campaigns.threshold.percent", { value });
  if (value === 0) return t("campaigns.threshold.first");
  return t("campaigns.threshold.count", { count: value });
}

/** "Sat, Sun", or "Every day" rather than the seven names. */
export function weekdaysText(weekdays: number[]): string {
  if (new Set(weekdays).size === 7) return t("campaigns.everyDay");
  return weekdays.map(weekdayName).join(", ");
}

export function describeWindow(window: CampaignWindow): string {
  if (window.kind === "once") {
    return t("campaigns.window.once", { start: formatDateTime(window.starts_at), end: formatDateTime(window.ends_at) });
  }
  return `${weekdaysText(window.weekdays)} ${window.start}–${window.end} (${window.timezone})`;
}

/** Tints of campaign states; their words come from `campaignStateWord`. */
export const CAMPAIGN_STATES: Record<CampaignState, Tone> = {
  draft: "neutral",
  scheduled: "neutral",
  running: "info",
  paused: "warn",
  stopped: "neutral",
  completed: "ok",
  failed: "bad",
};

/** A campaign state's word (the handoff says Succeeded for completed). */
export const campaignStateWord = (state: CampaignState): string => t(`campaigns.state.${state}`);

/** Tints of a server's status in a campaign; their words come from `serverStatusWord`. */
export const SERVER_STATUSES: Record<CampaignServerStatus, Tone> = {
  pending: "neutral",
  queued: "neutral",
  sent: "info",
  running: "info",
  succeeded: "ok",
  failed: "bad",
  expired: "bad",
  cancelled: "neutral",
  skipped: "bad",
};

export const serverStatusWord = (status: CampaignServerStatus): string => t(`campaigns.server.${status}`);

/** "debian-12/20261022T143000Z, rocky-9/…". */
export function describeSources(pin: SourcesPin): string {
  return pin.mirrors.map((m) => `${m.name}/${m.snapshot}`).join(", ");
}

/** The grey line under a campaign's name: "2 batches · snapshot debian-12/s1 · security only · reboot allowed". */
export function campaignSubline(campaign: Pick<Campaign, "wave_count" | "sources" | "spec" | "agent_version">): string {
  const { spec } = campaign;
  const batches = t("campaigns.batches", { count: campaign.wave_count });
  if (campaign.agent_version) {
    return `${batches} · ${t("campaigns.subline.agent", { version: campaign.agent_version })}`;
  }
  return [
    batches,
    campaign.sources &&
      t("campaigns.subline.snapshots", { count: campaign.sources.mirrors.length, sources: describeSources(campaign.sources) }),
    spec.include_os !== false &&
      (spec.packages.length > 0
        ? t("campaigns.subline.packages", { count: spec.packages.length })
        : spec.security_only
          ? t("campaigns.subline.security")
          : t("campaigns.subline.all")),
    describeManagers(spec) && t("campaigns.subline.apps", { managers: describeManagers(spec) }),
    spec.allow_reboot && t("campaigns.subline.reboot"),
  ]
    .filter(Boolean)
    .join(" · ");
}

/** A window in a table cell: "Sat, Sun 22:00–04:00", "23 Oct 02:00–05:00". */
export function shortWindow(window: CampaignWindow): string {
  if (window.kind === "weekly") {
    return `${weekdaysText(window.weekdays)} ${window.start}–${window.end}`;
  }
  const start = new Date(window.starts_at);
  const end = new Date(window.ends_at);
  // shortTime writes the date for any other day than its second argument.
  const dated = (date: Date) => shortTime(date, new Date(0));
  return `${dated(start)}–${shortTime(end, start)}`;
}
