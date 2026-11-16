import type {
  ActorKind,
  ChainReport,
  GuestKind,
  SnapshotSupport,
  RemediationEvent,
  RemediationEventKind,
  RemediationEventQuery,
  RemediationState,
  RiskTier,
  SnapshotUnsupported,
} from "./api";
import { t } from "@/i18n";
import { activeLocale } from "@/i18n/locale";
import { formatBytes } from "@/lib/format";

/** The `risk` status word of a tier (`StatusLabel kind="risk"`). */
export function riskWord(tier: RiskTier): "read-only" | "low-impact" | "high-impact" | "forbidden" {
  return tier.replace("_", "-") as "read-only" | "low-impact" | "high-impact" | "forbidden";
}

const TIERS: readonly RiskTier[] = ["read_only", "low_impact", "high_impact", "forbidden"];
const STATES: readonly RemediationState[] = ["open", "running", "awaiting_approval", "resolved", "escalated", "failed"];

/** A tier's name: "Low-impact". */
export function tierName(tier: RiskTier): string {
  return t(`remediation.tier.${tier}`);
}

function stateName(state: unknown): string | null {
  return STATES.includes(state as RemediationState) ? t(`remediation.state.${state as RemediationState}`) : null;
}

/** Event kinds in the order of a step, for the filter. */
export const EVENT_KINDS: readonly RemediationEventKind[] = [
  "context_sent",
  "ai_proposal",
  "classification",
  "approval",
  "edit",
  "rejection",
  "snapshot",
  "command_queued",
  "command_result",
  "escalation",
  "state_change",
  "note",
];

export const ACTOR_KINDS: readonly ActorKind[] = ["user", "system", "ai", "peer"];

/** The filter's choices of event kinds, in the active language. */
export const eventKindOptions = () => EVENT_KINDS.map((value) => ({ value, label: eventKindLabel(value) }));

/** The filter's choices of actors, in the active language. */
export const actorKindOptions = () => ACTOR_KINDS.map((value) => ({ value, label: t(`remediation.actors.${value}`) }));

export function eventKindLabel(kind: RemediationEventKind): string {
  return EVENT_KINDS.includes(kind) ? t(`remediation.kind.${kind}`) : kind;
}

/** Who did it, as the audit log shows it. */
export function actorLabel(event: Pick<RemediationEvent, "actor_kind" | "actor_id" | "actor_name">): string {
  switch (event.actor_kind) {
    case "user":
      return event.actor_name ?? t("remediation.actor.deletedUser");
    case "system":
      return t("remediation.actor.system");
    case "ai":
      return t("remediation.actor.ai", { id: event.actor_id });
    case "peer":
      return t("remediation.actor.agent", { id: event.actor_id });
  }
}

function text(payload: Record<string, unknown>, key: string): string {
  const value = payload[key];
  return typeof value === "string" ? value : "";
}

/** How long a command took: "20 ms", "1.5 s". */
function duration(ms: number): string {
  if (ms < 1000) return `${ms} ${t("format.unit.millisecond")}`;
  const seconds = (ms / 1000).toLocaleString(activeLocale(), { minimumFractionDigits: 1, maximumFractionDigits: 1 });
  return `${seconds} ${t("format.unit.second")}`;
}

/**
 * One line for an event of the audit log, from its payload. AI text and
 * commands are shown as given: React escapes them, nothing interprets them.
 */
export function eventSummary(event: Pick<RemediationEvent, "kind" | "actor_kind" | "payload">): string {
  const p = event.payload;
  switch (event.kind) {
    case "context_sent": {
      const redactions = p.context && typeof p.context === "object" ? (p.context as Record<string, unknown>).redactions : null;
      const count =
        redactions && typeof redactions === "object"
          ? Object.values(redactions).reduce<number>((sum, n) => sum + (typeof n === "number" ? n : 0), 0)
          : 0;
      const sent = t("remediation.summary.contextSent", { model: text(p, "model") || t("remediation.summary.theAi") });
      return count ? sent + t("remediation.summary.redacted", { count }) : sent;
    }
    case "ai_proposal":
      return typeof p.command === "string"
        ? t("remediation.summary.proposed", { command: p.command, diagnosis: text(p, "diagnosis") })
        : text(p, "stop") === "resolved"
          ? t("remediation.summary.stoppedFixed", { reason: text(p, "reason") })
          : t("remediation.summary.stoppedEscalates", { reason: text(p, "reason") });
    case "classification": {
      const tier = TIERS.includes(p.tier as RiskTier) ? tierName(p.tier as RiskTier) : text(p, "tier");
      const rule = p.rule && typeof p.rule === "object" ? text(p.rule as Record<string, unknown>, "id") : "";
      return rule
        ? t("remediation.summary.classifiedByRule", { tier, rule, reason: text(p, "reason") })
        : t("remediation.summary.classified", { tier, reason: text(p, "reason") });
    }
    case "approval":
      return event.actor_kind === "system"
        ? t("remediation.summary.approvedByPolicy", { command: text(p, "command") })
        : t("remediation.summary.approved", { command: text(p, "command") });
    case "edit":
      return t("remediation.summary.edited", { original: text(p, "original"), edited: text(p, "edited") });
    case "rejection":
      return text(p, "reason")
        ? t("remediation.summary.rejectedBecause", { command: text(p, "command"), reason: text(p, "reason") })
        : t("remediation.summary.rejected", { command: text(p, "command") });
    case "snapshot":
      return text(p, "method") === "none"
        ? t("remediation.summary.noSnapshot", { detail: text(p, "detail") })
        : t("remediation.summary.snapshot", { name: text(p, "name"), method: text(p, "method") });
    case "command_queued":
      return t("remediation.summary.queued", { command: text(p, "command") });
    case "command_result": {
      const ms = typeof p.duration_ms === "number" ? p.duration_ms : null;
      const took = ms === null ? "" : `, ${duration(ms)}`;
      return typeof p.exit_code === "number"
        ? t("remediation.summary.exitCode", { code: String(p.exit_code), took })
        : t("remediation.summary.notCompleted", { error: text(p, "error") || t("remediation.summary.noResult"), took });
    }
    case "escalation":
      return t("remediation.summary.escalated", { reason: text(p, "reason") });
    case "state_change":
      return t("remediation.summary.stateChange", {
        from: stateName(p.from) ?? text(p, "from"),
        to: stateName(p.to) ?? text(p, "to"),
      });
    case "note":
      return text(p, "text");
  }
}

/** Why rollback is limited on a server, for the approval detail. */
export function snapshotReason(reason: SnapshotUnsupported | null, kind?: GuestKind | null): string {
  switch (reason) {
    case "cannot_snapshot":
      return kind === "lxc" ? t("remediation.snapshot.cannotContainer") : t("remediation.snapshot.cannotVm");
    case "disabled":
      return t("remediation.snapshot.disabled");
    case "not_mapped":
      return t("remediation.snapshot.notMapped");
    case "unknown_cluster":
      return t("remediation.snapshot.unknownCluster");
    default:
      return t("remediation.snapshot.none");
  }
}

/**
 * Why `snapshots` has no local snapshot, in the active language, from the
 * API's `local_reason`; its English `detail` from an older server; null
 * when it says neither.
 */
export function localReasonText(snapshots: Pick<SnapshotSupport, "detail" | "local_reason">): string | null {
  const why = snapshots.local_reason;
  if (!why) return snapshots.detail ?? null;
  const bytes = (n: number | undefined) => formatBytes(n ?? 0);
  switch (why.code) {
    case "inspect":
      return t("remediation.local.inspect", { error: why.error ?? "" });
    case "root_not_covered":
      if (why.fs_type === "btrfs") return t("remediation.local.root_not_covered.btrfs");
      if (why.fs_type === "zfs") return t("remediation.local.root_not_covered.zfs");
      return t("remediation.local.root_not_covered", { fs: why.fs_type ?? "" });
    case "no_space":
      return t("remediation.local.no_space", {
        group: why.group ?? "",
        free: bytes(why.free_bytes),
        mount: why.mount_point ?? "",
        needed: bytes(why.needed_bytes),
      });
    case "pool_full":
      return t("remediation.local.pool_full", { mount: why.mount_point ?? "", free: bytes(why.free_bytes), size: bytes(why.pool_bytes) });
    case "windows":
    case "not_reported":
    case "sudo_rules":
    case "no_root":
      return t(`remediation.local.${why.code}`);
    default:
      return snapshots.detail ?? null;
  }
}

/**
 * The rollback line of the approval detail: which snapshot is taken before
 * the command, or why none is, then a quieter note (ADR-0044, ADR-0093,
 * ADR-0094).
 */
export function rollbackText(snapshots: SnapshotSupport): { text: string; note: string | null } {
  const { reason, detail, kind } = snapshots;
  if (!snapshots.available) {
    // A Proxmox guest: Proxmox's reason, then its cause.
    if (reason === "cannot_snapshot" || reason === "unknown_cluster" || !detail) {
      return {
        text: t("remediation.rollback.none", { reason: snapshotReason(reason, kind) }),
        note: detail && reason === "cannot_snapshot" ? `(${detail}.)` : null,
      };
    }
    // Not a Proxmox guest: why there is no local snapshot.
    return { text: t("remediation.rollback.noneDetail", { detail: localReasonText(snapshots) ?? detail }), note: null };
  }
  if ((snapshots.method === "lvm" || snapshots.method === "zfs") && detail) {
    return { text: t("remediation.rollback.local", { detail }), note: null };
  }
  return {
    text: kind === "lxc" ? t("remediation.rollback.proxmoxContainer") : t("remediation.rollback.proxmoxVm"),
    note: snapshots.left_out ? t("remediation.rollback.leftOut", { what: snapshots.left_out }) : null,
  };
}

/** "12 min 30 s left", or null once `deadline` passed. */
export function timeLeft(deadline: string, now: Date): string | null {
  const seconds = Math.floor((Date.parse(deadline) - now.getTime()) / 1000);
  if (seconds <= 0) return null;
  if (seconds < 60) return t("remediation.left.seconds", { seconds });
  const minutes = Math.floor(seconds / 60);
  if (minutes < 60) return t("remediation.left.minutes", { minutes, seconds: seconds % 60 });
  return t("remediation.left.hours", { hours: Math.floor(minutes / 60), minutes: minutes % 60 });
}

/** One line on an incident's chain report. */
export function chainSummary(chain: ChainReport): string {
  if (chain.valid) {
    return t("remediation.chain.verified", {
      count: chain.events,
      head: chain.head_hash?.slice(0, 12) ?? t("remediation.chain.noHead"),
    });
  }
  const why = {
    gap: "remediation.chain.gap",
    prev_hash: "remediation.chain.prevHash",
    hash: "remediation.chain.hash",
    head: "remediation.chain.head",
  } as const satisfies Record<NonNullable<ChainReport["reason"]>, string>;
  return t("remediation.chain.broken", {
    seq: String(chain.broken_at),
    why: t(chain.reason ? why[chain.reason] : "remediation.chain.unknown"),
  });
}

/** Filters of the audit log, from its query string. */
export interface AuditFilters {
  incident: string | null;
  server: string | null;
  kind: RemediationEventKind | null;
  actor: ActorKind | null;
}

export function auditFiltersFromQuery(params: URLSearchParams): AuditFilters {
  const kind = params.get("kind");
  const actor = params.get("actor");
  return {
    incident: params.get("incident"),
    server: params.get("server"),
    kind: EVENT_KINDS.includes(kind as RemediationEventKind) ? (kind as RemediationEventKind) : null,
    actor: ACTOR_KINDS.includes(actor as ActorKind) ? (actor as ActorKind) : null,
  };
}

export function auditQuery(filters: AuditFilters): RemediationEventQuery {
  return {
    incident_id: filters.incident ?? undefined,
    server_id: filters.server ?? undefined,
    kind: filters.kind ?? undefined,
    actor_kind: filters.actor ?? undefined,
  };
}
