// Campaign helpers without React (ADR-0033): the creation form's model and
// the checks the API makes, so most mistakes show before a request; the
// state machine for the action buttons; and display strings.

import { compareVersions } from "./agentVersion";
import { browserTimeZone, localToIso, toLocalInput } from "./clock";
import type { Tone } from "@/components/ui/tone";
import { type PlainKey, t } from "@/i18n";
import { activeLocale } from "@/i18n/locale";

import type {
  Campaign,
  CampaignAction,
  CampaignDetail,
  CampaignServer,
  CampaignServerStatus,
  CampaignRequest,
  CampaignSoftware,
  CampaignState,
  CampaignWindow,
  FailureThreshold,
  MajorUpgradeSpec,
  Server,
  SoftwareAction,
  SourcesPin,
  ThirdPartyManagerName,
  UpdateSpec,
  WaveSpec,
} from "./api";
import { formatDateTime, formatOs } from "./format";
import { shortTime } from "./mirrors";

/** Limits of `crates/server/src/campaign/model.rs` and the API. */
export const MAX_NAME_LEN = 64;
/** Servers of a batch upgrading at once in a major upgrade (ADR-0182). */
export const MAX_PARALLEL = 50;
export const MAX_TARGETS = 10_000;
export const MAX_WAVES = 100;
export const MAX_WINDOWS = 32;
export const MAX_PACKAGES = 1000;
export const MIN_TTL_S = 60;
export const MAX_TTL_S = 7 * 86_400;
/** Of `crates/common/src/software.rs` (ADR-0188). */
export const MAX_HOLDS = 1024;
export const MAX_SOFTWARE = 100;

/** The Windows Update classifications an update can approve, by the names Windows reports (ADR-0188). */
export const WINDOWS_CLASSIFICATIONS = [
  "Security Updates",
  "Critical Updates",
  "Definition Updates",
  "Update Rollups",
  "Updates",
  "Feature Packs",
  "Service Packs",
  "Tools",
  "Upgrades",
] as const;

/** ISO weekdays, Monday 1 to Sunday 7. */
export const WEEKDAYS = [1, 2, 3, 4, 5, 6, 7] as const;

/** Months, January 1 to December 12. */
export const MONTHS = [1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12] as const;

/** Weeks of a month: the first to the fourth, and 5 for the last. */
export const MONTH_WEEKS = [1, 2, 3, 4, 5] as const;

const locale = () => (activeLocale() === "en" ? "en-US" : activeLocale());

/** The long name of ISO weekday `day`: "Tuesday", "mardi". */
export function weekdayLong(day: number): string {
  return new Date(2026, 0, 4 + day).toLocaleString(locale(), { weekday: "long" });
}

/** The name of month `month` (1 is January): "January", or "Jan" when `short`. */
export function monthName(month: number, short = false): string {
  return new Date(2026, month - 1, 1).toLocaleString(locale(), { month: short ? "short" : "long" });
}

/** The short name of ISO weekday `day` in the active language: "Mon", "lun.". */
export function weekdayName(day: number): string {
  if (!(Number.isInteger(day) && day >= 1 && day <= 7)) return String(day);
  // 5 January 2026 is a Monday. American English says "Tue", where British English may say "Tues".
  return new Date(2026, 0, 4 + day).toLocaleString(locale(), { weekday: "short" });
}

export interface WaveDraft {
  kind: "size" | "percent" | "servers";
  /** Size or percentage, as typed. */
  value: string;
  servers: string[];
}

export interface WindowDraft {
  /** `monthly` and `yearly` are both monthly windows of the API; yearly has one month. */
  kind: "weekly" | "monthly" | "yearly" | "once";
  weekdays: number[];
  /** Monthly and yearly: on a day of the month, or on a weekday of a week of it. */
  monthMode: "day" | "weekday";
  /** As typed. */
  monthDay: string;
  monthWeek: number;
  weekday: number;
  /** The months it opens in; the first one for a yearly window. */
  months: number[];
  start: string;
  end: string;
  timezone: string;
  /** `datetime-local` values, in the browser's time zone. */
  startsAt: string;
  endsAt: string;
}


/** The major upgrades this version knows, one release at a time, as `common::major_upgrade::KNOWN` (ADR-0182). */
const DEBIAN_13 = { from: "debian 12", to: "debian 13", fromLabel: "Debian 12 (bookworm)", toLabel: "Debian 13 (trixie)" };
const leapp = (id: string, name: string, from: string, to: string): typeof DEBIAN_13 => ({
  from: `${id} ${from}`,
  to: `${id} ${to}`,
  fromLabel: `${name} ${from}`,
  toLabel: `${name} ${to}`,
});
export const MAJOR_UPGRADES: (typeof DEBIAN_13)[] = [
  DEBIAN_13,
  { from: "ubuntu 22.04", to: "ubuntu 24.04", fromLabel: "Ubuntu 22.04 (jammy)", toLabel: "Ubuntu 24.04 (noble)" },
  { from: "ubuntu 24.04", to: "ubuntu 26.04", fromLabel: "Ubuntu 24.04 (noble)", toLabel: "Ubuntu 26.04 (resolute)" },
  leapp("rhel", "Red Hat Enterprise Linux", "8", "9"),
  leapp("rhel", "Red Hat Enterprise Linux", "9", "10"),
  leapp("rocky", "Rocky Linux", "8", "9"),
  leapp("almalinux", "AlmaLinux", "8", "9"),
  leapp("almalinux", "AlmaLinux", "9", "10"),
];

/** "Debian 13 (trixie)" for "debian 13", else the release as given. */
export function releaseLabel(release: string): string {
  const from = MAJOR_UPGRADES.find((u) => u.from === release);
  if (from) return from.fromLabel;
  return MAJOR_UPGRADES.find((u) => u.to === release)?.toLabel ?? release;
}

/** Whether `osVersion` ("debian 12", "debian 12.8") is the release `release`. */
export const runsRelease = (osVersion: string | null | undefined, release: string): boolean =>
  !!osVersion && (osVersion === release || osVersion.startsWith(`${release}.`));

/** The confirmation before starting a major upgrade campaign: releases and number of servers. */
export function majorConfirmText(spec: MajorUpgradeSpec, servers: number): string {
  return t("campaigns.major.confirm", { count: servers, from: releaseLabel(spec.from), to: releaseLabel(spec.to) });
}

/** The words of each Windows Update classification (ADR-0188). */
export const CLASSIFICATION_KEYS: Record<(typeof WINDOWS_CLASSIFICATIONS)[number], PlainKey> = {
  "Security Updates": "campaigns.classification.security",
  "Critical Updates": "campaigns.classification.critical",
  "Definition Updates": "campaigns.classification.definition",
  "Update Rollups": "campaigns.classification.rollups",
  Updates: "campaigns.classification.updates",
  "Feature Packs": "campaigns.classification.featurePacks",
  "Service Packs": "campaigns.classification.servicePacks",
  Tools: "campaigns.classification.tools",
  Upgrades: "campaigns.classification.upgrades",
};

/** A classification in the active language; a name the interface does not know as Windows gave it. */
export function classificationName(name: string): string {
  const key = CLASSIFICATION_KEYS[name as keyof typeof CLASSIFICATION_KEYS];
  return key ? t(key) : name;
}

/**
 * `agent` updates the agents themselves (ADR-0053); `major` moves servers to the next release (ADR-0182);
 * `software` installs or removes catalogue entries (ADR-0188).
 */
export type UpdateKind = "all" | "security" | "packages" | "agent" | "major" | "software";

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
  /** For `major`: the release servers run, the next one, whether servers without a snapshot are upgraded too, and how many at once. */
  majorFrom: string;
  acceptLimited: boolean;
  parallel: string;
  /** For `software`: install or remove, and the catalogue entries. */
  softwareAction: SoftwareAction;
  softwareIds: string[];
  /** Windows Update classifications approved; empty is every one but Upgrades. */
  classifications: string[];
  /** Windows updates held back, KB numbers or update IDs, as typed. */
  hold: string;
  /** winget apps held back, as typed. */
  appHold: string;
  allowReboot: boolean;
  ttl: number;
  thresholdValue: string;
  thresholdUnit: FailureThreshold["unit"];
  windows: WindowDraft[];
  /** Pinned snapshots of the campaign edited or copied, kept as they are: the form does not edit them. */
  pin?: SourcesPin;
}

export function newWindow(kind: WindowDraft["kind"] = "weekly"): WindowDraft {
  return {
    kind,
    weekdays: [6, 7],
    monthMode: "day",
    monthDay: "1",
    monthWeek: 1,
    weekday: 6,
    months: kind === "yearly" ? [1] : [...MONTHS],
    start: "22:00",
    end: "04:00",
    timezone: browserTimeZone(),
    startsAt: "",
    endsAt: "",
  };
}

/** `window` as another kind, keeping its hours, zone and day. */
export function changeKind(window: WindowDraft, kind: WindowDraft["kind"]): WindowDraft {
  const months = kind === "yearly" ? [window.months[0] ?? 1] : kind === "monthly" ? [...MONTHS] : window.months;
  return { ...window, kind, months: window.kind === kind ? window.months : months };
}

function windowRequest(w: WindowDraft): CampaignWindow {
  if (w.kind === "once") {
    return { kind: "once", starts_at: localToIso(w.startsAt) ?? "", ends_at: localToIso(w.endsAt) ?? "" };
  }
  const hours = { start: w.start, end: w.end, timezone: w.timezone.trim() };
  if (w.kind === "weekly") return { kind: "weekly", weekdays: [...w.weekdays].sort((a, b) => a - b), ...hours };
  const day =
    w.monthMode === "day" ? { month_day: Number(w.monthDay) } : { month_week: w.monthWeek, weekday: w.weekday };
  const months = w.kind === "yearly" ? w.months.slice(0, 1) : [...w.months].sort((a, b) => a - b);
  return { kind: "monthly", ...day, months, ...hours };
}

function windowDraft(w: CampaignWindow): WindowDraft {
  if (w.kind === "once") return { ...newWindow("once"), startsAt: isoToLocal(w.starts_at), endsAt: isoToLocal(w.ends_at) };
  const hours = { start: w.start, end: w.end, timezone: w.timezone };
  if (w.kind === "weekly") return { ...newWindow("weekly"), weekdays: w.weekdays, ...hours };
  const byDay = w.month_day !== null && w.month_day !== undefined;
  return {
    ...newWindow(w.months.length === 1 ? "yearly" : "monthly"),
    monthMode: byDay ? "day" : "weekday",
    monthDay: byDay ? String(w.month_day) : "1",
    monthWeek: w.month_week ?? 1,
    weekday: w.weekday ?? 6,
    months: w.months,
    ...hours,
  };
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
    majorFrom: DEBIAN_13.from,
    acceptLimited: false,
    parallel: "1",
    softwareAction: "install",
    softwareIds: [],
    classifications: [],
    hold: "",
    appHold: "",
    allowReboot: false,
    ttl: 3600,
    thresholdValue: "0",
    thresholdUnit: "count",
    windows: [],
  };
}

/** Names typed separated by spaces, commas or lines. */
export const splitNames = (text: string): string[] => text.split(/[\s,]+/).filter(Boolean);

export function toRequest(draft: CampaignDraft): CampaignRequest {
  const waves: WaveSpec[] = draft.waves.map((wave) =>
    wave.kind === "servers"
      ? { servers: wave.servers }
      : wave.kind === "size"
        ? { size: Number(wave.value) }
        : { percent: Number(wave.value) },
  );
  const windows = draft.windows.map(windowRequest);
  const common = {
    customer_id: draft.customerId,
    name: draft.name,
    ...(draft.siteId && { site_id: draft.siteId }),
    server_ids: draft.serverIds,
    waves,
  };
  const timing = {
    command_ttl_s: draft.ttl,
    failure_threshold: { value: Number(draft.thresholdValue), unit: draft.thresholdUnit },
    windows,
  };
  if (draft.update === "software") {
    // A software campaign takes no packages, apps, approvals nor snapshots (ADR-0188).
    return {
      ...common,
      packages: [],
      security_only: false,
      allow_reboot: draft.allowReboot,
      software: { action: draft.softwareAction, catalog_ids: draft.softwareIds },
      ...timing,
    };
  }
  const major = draft.update === "major";
  const agent = draft.update === "agent" || major;
  const os = agent || includesOs(draft.sources);
  const apps = !agent && includesApps(draft.sources);
  const upgrade = MAJOR_UPGRADES.find((u) => u.from === draft.majorFrom) ?? DEBIAN_13;
  // Approvals and holds narrow the OS's Windows updates; classifications only all or security ones.
  const approvals = os && !agent;
  const classifications =
    approvals && draft.update !== "packages" ? WINDOWS_CLASSIFICATIONS.filter((c) => draft.classifications.includes(c)) : [];
  const hold = approvals ? splitNames(draft.hold) : [];
  const appHold = splitNames(draft.appHold);
  return {
    ...common,
    packages: os && draft.update === "packages" ? splitNames(draft.packages) : [],
    security_only: os && draft.update === "security",
    allow_reboot: !agent && draft.allowReboot,
    ...(draft.update === "agent" && { agent_version: draft.agentVersion }),
    ...(major && {
      major_upgrade: {
        from: upgrade.from,
        to: upgrade.to,
        accept_limited_rollback: draft.acceptLimited,
        parallel: Number(draft.parallel),
      },
    }),
    ...(!os && { include_os: false }),
    ...(apps && {
      third_party: draft.managers.map((manager) => ({
        manager,
        packages: [],
        ...(manager === "winget" && appHold.length > 0 && { hold: appHold }),
      })),
    }),
    ...(classifications.length > 0 && { classifications }),
    ...(hold.length > 0 && { hold }),
    ...timing,
    ...(draft.pin && os && !agent && { sources: draft.pin }),
  };
}

/**
 * How the API edits `campaign` now (ADR-0179, ADR-0173): `full` before its first batch is queued;
 * `running` while a run is under way, when only its name, windows and failure threshold change;
 * `next` once over, when saving prepares its next run; `busy` while commands of a finished run
 * still run, when it waits for them.
 */
export type EditMode = "full" | "running" | "next" | "busy";

export function editMode(campaign: Pick<Campaign, "state" | "current_wave" | "progress">): EditMode {
  if (isFinal(campaign.state)) return campaign.progress.in_progress > 0 ? "busy" : "next";
  return campaign.current_wave === null ? "full" : "running";
}

/** Whether the API runs `campaign` again: a finished one whose commands all ended (ADR-0173). */
export function canRelaunch(campaign: Pick<Campaign, "state" | "progress">): boolean {
  return isFinal(campaign.state) && campaign.progress.in_progress === 0;
}

/** Servers of the last run that `{"servers": "failed"}` runs again. */
export function failedServers(campaign: Pick<Campaign, "progress">): number {
  return campaign.progress.failed + campaign.progress.cancelled;
}

/**
 * The request `campaign` holds, field for field, so that a run under way can be edited without
 * the API seeing a change of what it holds fixed. `draft` gives the servers of a campaign made
 * before its request was kept, which the API does not compare.
 */
export function requestFromCampaign(campaign: CampaignDetail, draft: CampaignDraft): CampaignRequest {
  const fallback = toRequest(draft);
  const { spec } = campaign;
  return {
    customer_id: campaign.customer_id,
    name: campaign.name,
    ...(campaign.site_id && { site_id: campaign.site_id }),
    server_ids: campaign.server_ids ?? fallback.server_ids,
    waves: campaign.wave_specs ?? fallback.waves,
    packages: spec.packages,
    security_only: spec.security_only,
    allow_reboot: spec.allow_reboot,
    ...(campaign.agent_version && { agent_version: campaign.agent_version }),
    ...(campaign.major_upgrade && { major_upgrade: campaign.major_upgrade }),
    ...(spec.include_os === false && { include_os: false }),
    ...(spec.third_party?.length && { third_party: spec.third_party }),
    command_ttl_s: campaign.command_ttl_s,
    failure_threshold: campaign.failure_threshold,
    windows: campaign.windows,
    ...(campaign.sources && { sources: campaign.sources }),
    ...(spec.classifications?.length && { classifications: spec.classifications }),
    ...(spec.hold?.length && { hold: spec.hold }),
    ...(campaign.software && { software: softwareSelection(campaign.software) }),
  };
}

/** The request's `software` of a saved software campaign: its entries by catalogue ID. */
export function softwareSelection(software: CampaignSoftware): { action: SoftwareAction; catalog_ids: string[] } {
  return { action: software.action, catalog_ids: software.packages.map((p) => p.catalog_id) };
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
 * the request it was made from (before ADR-0179), its servers outside its
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
  const software = campaign.software ?? null;
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
    update: campaign.major_upgrade
      ? "major"
      : software
        ? "software"
        : campaign.agent_version
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
    majorFrom: campaign.major_upgrade?.from ?? DEBIAN_13.from,
    acceptLimited: campaign.major_upgrade?.accept_limited_rollback ?? false,
    parallel: String(campaign.major_upgrade?.parallel ?? 1),
    softwareAction: software?.action ?? "install",
    softwareIds: software ? softwareSelection(software).catalog_ids : [],
    classifications: spec.classifications ?? [],
    hold: (spec.hold ?? []).join(" "),
    appHold: (third.find((s) => s.manager === "winget")?.hold ?? []).join(" "),
    allowReboot: spec.allow_reboot,
    ttl: campaign.command_ttl_s,
    thresholdValue: String(campaign.failure_threshold.value),
    thresholdUnit: campaign.failure_threshold.unit,
    windows: campaign.windows.map(windowDraft),
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
 * platform one of them is for. A software campaign (`software`) targets
 * Windows servers only, whether they take package updates or not.
 */
export function campaignTargets(
  servers: Server[],
  customerId: string,
  siteId: string,
  serverIds: string[],
  agentUpdate = false,
  appsOnly: ThirdPartyManagerName[] | null = null,
  software = false,
): Server[] {
  const named = new Set(serverIds);
  return servers
    .filter(
      (s) =>
        s.customer_id === customerId &&
        s.revoked_at === null &&
        (software
          ? s.os_family === "windows"
          : appsOnly
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
// As `valid_windows_update_name`: a KB number or an update ID.
const WINDOWS_UPDATE = /^KB\d{1,10}$|^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/i;
// As `valid_winget_id`: 2 to 8 segments joined by `.`, at most 128 bytes.
const WINGET_SEGMENT = /^[A-Za-z0-9][A-Za-z0-9_+-]{0,31}$/;

/** Whether `id` is a winget package ID the API takes. */
export function validWingetId(id: string): boolean {
  const segments = id.split(".");
  return (
    new TextEncoder().encode(id).length <= 128 &&
    segments.length >= 2 &&
    segments.length <= 8 &&
    segments.every((s) => WINGET_SEGMENT.test(s))
  );
}

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
  const parallel = request.major_upgrade?.parallel;
  if (parallel !== undefined && !(Number.isInteger(parallel) && parallel >= 1 && parallel <= MAX_PARALLEL)) {
    errors.push(t("campaigns.check.parallel", { max: String(MAX_PARALLEL) }));
  }
  if (request.include_os === false && request.third_party === undefined) errors.push(t("campaigns.check.sources"));
  if (request.third_party?.length === 0) errors.push(t("campaigns.check.managers"));
  if (request.packages.length > MAX_PACKAGES) errors.push(t("campaigns.check.packageCount", { max: String(MAX_PACKAGES) }));
  // On Windows servers, named packages are KB numbers, which follow the same grammar as these.
  const badPackage = request.packages.find((p) => p.length > 128 || !PACKAGE.test(p));
  if (badPackage !== undefined) errors.push(t("campaigns.check.packageName", { name: badPackage }));
  const hold = request.hold ?? [];
  if (hold.length > MAX_HOLDS) errors.push(t("campaigns.check.holdCount", { max: String(MAX_HOLDS) }));
  const badHold = hold.find((h) => !WINDOWS_UPDATE.test(h));
  if (badHold !== undefined) errors.push(t("campaigns.check.holdName", { name: badHold }));
  const heldAndNamed = hold.find((h) => request.packages.some((p) => p.toLowerCase() === h.toLowerCase()));
  if (heldAndNamed !== undefined) errors.push(t("campaigns.check.heldAndNamed", { name: heldAndNamed }));
  const appHold = (request.third_party ?? []).flatMap((s) => s.hold ?? []);
  const badApp = appHold.find((id) => !validWingetId(id));
  if (badApp !== undefined) errors.push(t("campaigns.check.appHoldName", { name: badApp }));
  if (request.software) {
    if (request.software.catalog_ids.length === 0) errors.push(t("campaigns.check.software"));
    else if (request.software.catalog_ids.length > MAX_SOFTWARE) {
      errors.push(t("campaigns.check.softwareCount", { max: String(MAX_SOFTWARE) }));
    }
  }
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
    if (w.kind !== "once") {
      if (w.kind === "weekly") {
        if (w.weekdays.length === 0 || w.weekdays.some((d) => !(Number.isInteger(d) && d >= 1 && d <= 7))) {
          errors.push(t("campaigns.check.weekday", { window: windowName }));
        }
      } else {
        const day = w.month_day;
        if (day !== undefined && day !== null && !(Number.isInteger(day) && day >= 1 && day <= 31)) {
          errors.push(t("campaigns.check.monthDay", { window: windowName }));
        }
        if (w.months.length === 0) errors.push(t("campaigns.check.months", { window: windowName }));
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

/** "7-Zip, Firefox": the software of a software campaign. */
export function describeSoftware(software: CampaignSoftware): string {
  return software.packages.map((p) => p.name).join(", ");
}

export function describeUpdate(
  spec: UpdateSpec,
  agentVersion: string | null = null,
  major: MajorUpgradeSpec | null = null,
  software: CampaignSoftware | null = null,
): string {
  if (major) return t("campaigns.update.major", { from: releaseLabel(major.from), to: releaseLabel(major.to) });
  if (agentVersion) return t("campaigns.update.agent", { version: agentVersion });
  if (software) {
    const what = t(software.action === "install" ? "campaigns.update.install" : "campaigns.update.remove", {
      software: describeSoftware(software),
    });
    return spec.allow_reboot ? t("campaigns.update.reboot", { update: what }) : what;
  }
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

/** When a recurring window opens: "Sat, Sun", "Second Tuesday of each month", "Every year on December 31". */
export function recurrenceText(window: Exclude<CampaignWindow, { kind: "once" }>): string {
  if (window.kind === "weekly") return weekdaysText(window.weekdays);
  const { months } = window;
  const day = window.month_day ?? null;
  // French writes the first of a month "1er".
  const nth = (d: number) => (activeLocale() === "fr" && d === 1 ? "1er" : String(d));
  if (months.length === 1 && day !== null) {
    const date =
      activeLocale() === "fr"
        ? `${nth(day)} ${monthName(months[0]!)}`
        : new Date(2026, months[0]! - 1, day).toLocaleString(locale(), { day: "numeric", month: "long" });
    return t("campaigns.window.yearDay", { date });
  }
  const when =
    months.length === 12
      ? t("campaigns.window.eachMonth")
      : months.map((m) => monthName(m, months.length > 1)).join(", ");
  if (day !== null) return t("campaigns.window.monthDay", { day: nth(day), months: when });
  return t("campaigns.window.monthWeek", {
    week: t(`campaigns.window.week${window.month_week ?? 1}` as "campaigns.window.week1"),
    weekday: weekdayLong(window.weekday ?? 1),
    months: when,
  });
}

export function describeWindow(window: CampaignWindow): string {
  if (window.kind === "once") {
    return t("campaigns.window.once", { start: formatDateTime(window.starts_at), end: formatDateTime(window.ends_at) });
  }
  return `${recurrenceText(window)} ${window.start}–${window.end} (${window.timezone})`;
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
  upgrading: "info",
};

/** What a server of a major upgrade is doing, from its agent's phase and its Proxmox snapshot (ADR-0182). */
export function upgradeStepText(server: Pick<CampaignServer, "status" | "upgrade_phase" | "vm_snapshot">): string | null {
  if (server.status === "pending" && server.vm_snapshot === "taking") return t("campaigns.major.phase.snapshot");
  if (server.status !== "upgrading" || !server.upgrade_phase) return null;
  return t(`campaigns.major.phase.${server.upgrade_phase}`);
}

export const serverStatusWord = (status: CampaignServerStatus): string => t(`campaigns.server.${status}`);

/** "debian-12/20261022T143000Z, rocky-9/…". */
export function describeSources(pin: SourcesPin): string {
  return pin.mirrors.map((m) => `${m.name}/${m.snapshot}`).join(", ");
}

/** The grey line under a campaign's name: "2 batches · snapshot debian-12/s1 · security only · reboot allowed". */
export function campaignSubline(
  campaign: Pick<Campaign, "wave_count" | "sources" | "spec" | "agent_version" | "major_upgrade" | "software">,
): string {
  const { spec } = campaign;
  const batches = t("campaigns.batches", { count: campaign.wave_count });
  if (campaign.major_upgrade) {
    const { from, to } = campaign.major_upgrade;
    return `${batches} · ${t("campaigns.subline.major", { from: releaseLabel(from), to: releaseLabel(to) })}`;
  }
  if (campaign.agent_version) {
    return `${batches} · ${t("campaigns.subline.agent", { version: campaign.agent_version })}`;
  }
  if (campaign.software) {
    const count = campaign.software.packages.length;
    return [
      batches,
      t(campaign.software.action === "install" ? "campaigns.subline.install" : "campaigns.subline.remove", { count }),
      spec.allow_reboot && t("campaigns.subline.reboot"),
    ]
      .filter(Boolean)
      .join(" · ");
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
  if (window.kind !== "once") {
    return `${recurrenceText(window)} ${window.start}–${window.end}`;
  }
  const start = new Date(window.starts_at);
  const end = new Date(window.ends_at);
  // shortTime writes the date for any other day than its second argument.
  const dated = (date: Date) => shortTime(date, new Date(0));
  return `${dated(start)}–${shortTime(end, start)}`;
}
