// Campaign helpers without React (ADR-0033): the creation form's model and
// the checks the API makes, so most mistakes show before a request; the
// state machine for the action buttons; and display strings.

import type { Tone } from "@/components/ui/tone";

import type {
  Campaign,
  CampaignAction,
  CampaignServerStatus,
  CampaignRequest,
  CampaignState,
  CampaignWindow,
  FailureThreshold,
  Server,
  SourcesPin,
  UpdateSpec,
  WaveSpec,
} from "./api";
import { formatDateTime } from "./format";
import { shortTime } from "./mirrors";

/** Limits of `crates/server/src/campaign/model.rs` and the API. */
export const MAX_NAME_LEN = 64;
export const MAX_TARGETS = 10_000;
export const MAX_WAVES = 100;
export const MAX_WINDOWS = 32;
export const MAX_PACKAGES = 1000;
export const MIN_TTL_S = 60;
export const MAX_TTL_S = 7 * 86_400;

export const WEEKDAYS = ["Mon", "Tue", "Wed", "Thu", "Fri", "Sat", "Sun"] as const;

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

export type UpdateKind = "all" | "security" | "packages";

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
  allowReboot: boolean;
  ttl: number;
  thresholdValue: string;
  thresholdUnit: FailureThreshold["unit"];
  windows: WindowDraft[];
}

export function browserTimeZone(): string {
  return Intl.DateTimeFormat().resolvedOptions().timeZone || "UTC";
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
    allowReboot: false,
    ttl: 3600,
    thresholdValue: "0",
    thresholdUnit: "count",
    windows: [],
  };
}

function localToIso(local: string): string {
  const date = new Date(local);
  return local && !Number.isNaN(date.getTime()) ? date.toISOString() : "";
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
      : { kind: "once", starts_at: localToIso(w.startsAt), ends_at: localToIso(w.endsAt) },
  );
  return {
    customer_id: draft.customerId,
    name: draft.name,
    ...(draft.siteId && { site_id: draft.siteId }),
    server_ids: draft.serverIds,
    waves,
    packages: draft.update === "packages" ? draft.packages.split(/[\s,]+/).filter(Boolean) : [],
    security_only: draft.update === "security",
    allow_reboot: draft.allowReboot,
    command_ttl_s: draft.ttl,
    failure_threshold: { value: Number(draft.thresholdValue), unit: draft.thresholdUnit },
    windows,
  };
}

/** The servers a campaign would target, in the API's order: hostname, then peer ID. */
export function campaignTargets(servers: Server[], customerId: string, siteId: string, serverIds: string[]): Server[] {
  const named = new Set(serverIds);
  return servers
    .filter(
      (s) => s.customer_id === customerId && s.revoked_at === null && ((siteId && s.site_id === siteId) || named.has(s.id)),
    )
    .sort((a, b) => cmp(a.hostname, b.hostname) || cmp(a.peer_id, b.peer_id));
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
      if (!known.has(server)) return { error: `a server of wave ${index + 1} is not a target of the campaign` };
      if (placed.has(server)) return { error: "a server is in more than one wave" };
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
      if (!(spec.percent >= 1 && spec.percent <= 100)) return { error: "a wave percentage is 1 to 100" };
      wave = pool.splice(0, Math.ceil((targets.length * spec.percent) / 100));
    }
    if (wave.length === 0) return { error: `wave ${index + 1} would be empty` };
    waves.push(wave);
  }
  if (pool.length > 0) waves.push(pool);
  if (waves.length > MAX_WAVES) return { error: `at most ${MAX_WAVES} waves` };
  return { waves };
}

const PACKAGE = /^[A-Za-z0-9][A-Za-z0-9._+:-]*$/;
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
    errors.push(`The name must be 1 to ${MAX_NAME_LEN} characters, without control characters or surrounding spaces.`);
  }
  if (!request.customer_id) errors.push("Choose a customer.");
  if (!(request.command_ttl_s >= MIN_TTL_S && request.command_ttl_s <= MAX_TTL_S)) {
    errors.push(`Commands must stay valid between ${MIN_TTL_S} and ${MAX_TTL_S} seconds.`);
  }
  if (request.packages.length > MAX_PACKAGES) errors.push(`At most ${MAX_PACKAGES} packages.`);
  const badPackage = request.packages.find((p) => p.length > 128 || !PACKAGE.test(p));
  if (badPackage !== undefined) errors.push(`Invalid package name "${badPackage}".`);
  const { value, unit } = request.failure_threshold;
  if (!isCount(value, unit === "percent" ? 100 : MAX_TARGETS)) {
    errors.push(
      unit === "percent"
        ? "The failure threshold is a whole percentage from 0 to 100."
        : `The failure threshold is a whole number of servers from 0 to ${MAX_TARGETS}.`,
    );
  }
  if (request.windows.length > MAX_WINDOWS) errors.push(`At most ${MAX_WINDOWS} windows.`);
  request.windows.forEach((w, i) => {
    const label = `Window ${i + 1}`;
    if (w.kind === "weekly") {
      if (w.weekdays.length === 0 || w.weekdays.some((d) => !(Number.isInteger(d) && d >= 1 && d <= 7))) {
        errors.push(`${label}: choose at least one weekday.`);
      }
      if (!TIME.test(w.start) || w.start === "24:00") errors.push(`${label}: the start is a time before 24:00.`);
      if (!TIME.test(w.end)) errors.push(`${label}: the end is a time such as 04:00.`);
      // Midnight as an end is the end of the day.
      else if (TIME.test(w.start) && minutes(w.start) === (minutes(w.end) || 24 * 60)) {
        errors.push(`${label}: it must end at another time than it starts.`);
      }
      if (!w.timezone || w.timezone.length > 64) errors.push(`${label}: give a time zone such as Europe/Paris.`);
    } else if (!w.starts_at || !w.ends_at) {
      errors.push(`${label}: give a start and an end.`);
    } else if (new Date(w.starts_at) >= new Date(w.ends_at)) {
      errors.push(`${label}: it must end after it starts.`);
    }
  });
  if (!request.site_id && request.server_ids.length === 0) {
    errors.push("Choose a site or servers to update.");
  } else if (targets.length === 0) {
    errors.push("The campaign targets no server.");
  } else if (targets.length > MAX_TARGETS) {
    errors.push(`At most ${MAX_TARGETS} servers.`);
  }
  if (request.waves.some((w) => "size" in w && !isCount(w.size, MAX_TARGETS))) {
    errors.push("A wave size is a whole number of servers.");
  } else if (request.waves.some((w) => "percent" in w && !Number.isInteger(w.percent))) {
    errors.push("A wave percentage is a whole number from 1 to 100.");
  } else if (targets.length > 0) {
    const plan = planWaves(targets, request.waves);
    if ("error" in plan) errors.push(`Waves: ${plan.error}.`);
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

export function describeUpdate(spec: UpdateSpec): string {
  const what =
    spec.packages.length > 0
      ? `Packages: ${spec.packages.join(", ")}`
      : spec.security_only
        ? "Security updates"
        : "All upgrades";
  return spec.allow_reboot ? `${what}, reboot allowed` : what;
}

export function describeThreshold({ value, unit }: FailureThreshold): string {
  if (unit === "percent") return `more than ${value}% of a wave fails`;
  if (value === 0) return "first failure";
  return `more than ${value} server${value === 1 ? "" : "s"} of a wave fail`;
}

export function describeWindow(window: CampaignWindow): string {
  if (window.kind === "once") {
    return `${formatDateTime(window.starts_at)} to ${formatDateTime(window.ends_at)}`;
  }
  const days = window.weekdays.map((d) => WEEKDAYS[d - 1] ?? String(d)).join(", ");
  return `${days} ${window.start}–${window.end} (${window.timezone})`;
}

/** Status words of campaigns and their servers (the handoff says Succeeded for completed). */
export const CAMPAIGN_STATES: Record<CampaignState, readonly [string, Tone]> = {
  draft: ["Draft", "neutral"],
  scheduled: ["Scheduled", "neutral"],
  running: ["Running", "info"],
  paused: ["Paused", "warn"],
  stopped: ["Stopped", "neutral"],
  completed: ["Succeeded", "ok"],
  failed: ["Failed", "bad"],
};

export const SERVER_STATUSES: Record<CampaignServerStatus, readonly [string, Tone]> = {
  pending: ["Pending", "neutral"],
  queued: ["Queued", "neutral"],
  sent: ["Sent", "info"],
  running: ["Running", "info"],
  succeeded: ["Succeeded", "ok"],
  failed: ["Failed", "bad"],
  expired: ["Expired", "bad"],
  cancelled: ["Cancelled", "neutral"],
  skipped: ["Skipped", "bad"],
};

/** "debian-12/20261022T143000Z, rocky-9/…". */
export function describeSources(pin: SourcesPin): string {
  return pin.mirrors.map((m) => `${m.name}/${m.snapshot}`).join(", ");
}

/** The grey line under a campaign's name: "2 waves · snapshot debian-12/s1 · security only · reboot allowed". */
export function campaignSubline(campaign: Pick<Campaign, "wave_count" | "sources" | "spec">): string {
  const { spec } = campaign;
  return [
    `${campaign.wave_count} wave${campaign.wave_count === 1 ? "" : "s"}`,
    campaign.sources && `snapshot${campaign.sources.mirrors.length === 1 ? "" : "s"} ${describeSources(campaign.sources)}`,
    spec.packages.length > 0
      ? `${spec.packages.length} package${spec.packages.length === 1 ? "" : "s"}`
      : spec.security_only
        ? "security only"
        : "all packages",
    spec.allow_reboot && "reboot allowed",
  ]
    .filter(Boolean)
    .join(" · ");
}

/** A window in a table cell: "Sat, Sun 22:00–04:00", "23 Oct 02:00–05:00". */
export function shortWindow(window: CampaignWindow): string {
  if (window.kind === "weekly") {
    return `${window.weekdays.map((d) => WEEKDAYS[d - 1] ?? String(d)).join(", ")} ${window.start}–${window.end}`;
  }
  const start = new Date(window.starts_at);
  const end = new Date(window.ends_at);
  // shortTime writes the date for any other day than its second argument.
  const dated = (date: Date) => shortTime(date, new Date(0));
  return `${dated(start)}–${shortTime(end, start)}`;
}
