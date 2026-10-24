import type {
  ActorKind,
  ChainReport,
  RemediationEvent,
  RemediationEventKind,
  RemediationEventQuery,
  RemediationState,
  RiskTier,
  SnapshotUnsupported,
} from "./api";

/** The `risk` status word of a tier (`StatusLabel kind="risk"`). */
export function riskWord(tier: RiskTier): "read-only" | "low-impact" | "high-impact" | "forbidden" {
  return tier.replace("_", "-") as "read-only" | "low-impact" | "high-impact" | "forbidden";
}

const TIER_NAMES: Record<RiskTier, string> = {
  read_only: "Read-only",
  low_impact: "Low-impact",
  high_impact: "High-impact",
  forbidden: "Forbidden",
};

const STATE_NAMES: Record<RemediationState, string> = {
  open: "open",
  running: "running",
  awaiting_approval: "awaiting approval",
  resolved: "resolved",
  escalated: "escalated",
  failed: "failed",
};

/** Event kinds in the order of a step, for the filter. */
export const EVENT_KINDS: { value: RemediationEventKind; label: string }[] = [
  { value: "context_sent", label: "Context sent" },
  { value: "ai_proposal", label: "AI proposal" },
  { value: "classification", label: "Classification" },
  { value: "approval", label: "Approval" },
  { value: "edit", label: "Edit" },
  { value: "rejection", label: "Rejection" },
  { value: "snapshot", label: "Snapshot" },
  { value: "command_queued", label: "Command queued" },
  { value: "command_result", label: "Command result" },
  { value: "escalation", label: "Escalation" },
  { value: "state_change", label: "State change" },
  { value: "note", label: "Note" },
];

export const ACTOR_KINDS: { value: ActorKind; label: string }[] = [
  { value: "user", label: "Users" },
  { value: "system", label: "System" },
  { value: "ai", label: "AI" },
  { value: "peer", label: "Agents" },
];

export function eventKindLabel(kind: RemediationEventKind): string {
  return EVENT_KINDS.find((k) => k.value === kind)?.label ?? kind;
}

/** Who did it, as the audit log shows it. */
export function actorLabel(event: Pick<RemediationEvent, "actor_kind" | "actor_id" | "actor_name">): string {
  switch (event.actor_kind) {
    case "user":
      return event.actor_name ?? "deleted user";
    case "system":
      return "system";
    case "ai":
      return `AI · ${event.actor_id}`;
    case "peer":
      return `agent ${event.actor_id}`;
  }
}

function text(payload: Record<string, unknown>, key: string): string {
  const value = payload[key];
  return typeof value === "string" ? value : "";
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
      return `Context sent to ${text(p, "model") || "the AI"}${count ? `, ${count} ${count === 1 ? "secret" : "secrets"} redacted` : ""}`;
    }
    case "ai_proposal":
      return typeof p.command === "string"
        ? `Proposed ${p.command} · ${text(p, "diagnosis")}`
        : `Stopped (${text(p, "stop") === "resolved" ? "considers it fixed" : "escalates"}): ${text(p, "reason")}`;
    case "classification": {
      const tier = TIER_NAMES[p.tier as RiskTier] ?? text(p, "tier");
      const rule = p.rule && typeof p.rule === "object" ? text(p.rule as Record<string, unknown>, "id") : "";
      return `${tier}${rule ? ` by rule ${rule}` : ""}: ${text(p, "reason")}`;
    }
    case "approval":
      return event.actor_kind === "system"
        ? `Approved by policy: ${text(p, "command")}`
        : `Approved: ${text(p, "command")}`;
    case "edit":
      return `Edited ${text(p, "original")} → ${text(p, "edited")}`;
    case "rejection":
      return `Rejected ${text(p, "command")}${text(p, "reason") ? ` · ${text(p, "reason")}` : ""}`;
    case "snapshot":
      return text(p, "method") === "none"
        ? `No snapshot: ${text(p, "detail")}`
        : `Snapshot ${text(p, "name")} (${text(p, "method")})`;
    case "command_queued":
      return `Queued ${text(p, "command")}`;
    case "command_result": {
      const ms = typeof p.duration_ms === "number" ? p.duration_ms : null;
      const took = ms === null ? "" : `, ${ms < 1000 ? `${ms} ms` : `${(ms / 1000).toFixed(1)} s`}`;
      return typeof p.exit_code === "number"
        ? `Exit code ${p.exit_code}${took}`
        : `Did not complete: ${text(p, "error") || "no result"}${took}`;
    }
    case "escalation":
      return `Escalated: ${text(p, "reason")}`;
    case "state_change":
      return `State ${STATE_NAMES[p.from as RemediationState] ?? text(p, "from")} → ${STATE_NAMES[p.to as RemediationState] ?? text(p, "to")}`;
    case "note":
      return text(p, "text");
  }
}

/** Why rollback is limited on a server, for the approval detail. */
export function snapshotReason(reason: SnapshotUnsupported | null): string {
  switch (reason) {
    case "disabled":
      return "Proxmox snapshots are not enabled on this MonitorFlow server.";
    case "not_mapped":
      return "the server is not mapped to a Proxmox VM.";
    case "unknown_cluster":
      return "the server's Proxmox cluster is not configured.";
    default:
      return "no snapshot support on this server.";
  }
}

/** "12 min 30 s left", or null once `deadline` passed. */
export function timeLeft(deadline: string, now: Date): string | null {
  const seconds = Math.floor((Date.parse(deadline) - now.getTime()) / 1000);
  if (seconds <= 0) return null;
  if (seconds < 60) return `${seconds} s left`;
  const minutes = Math.floor(seconds / 60);
  if (minutes < 60) return `${minutes} min ${seconds % 60} s left`;
  return `${Math.floor(minutes / 60)} h ${minutes % 60} min left`;
}

/** One line on an incident's chain report. */
export function chainSummary(chain: ChainReport): string {
  if (chain.valid) {
    return `Hash chain verified: ${chain.events} ${chain.events === 1 ? "event" : "events"}, head ${chain.head_hash?.slice(0, 12) ?? "none"}.`;
  }
  const why: Record<NonNullable<ChainReport["reason"]>, string> = {
    gap: "a missing event",
    prev_hash: "a link to the previous event that does not match",
    hash: "an event whose content does not match its hash",
    head: "a head that does not match the last event",
  };
  return `Hash chain broken at event ${chain.broken_at}: ${chain.reason ? why[chain.reason] : "unknown reason"}. The history was changed outside MonitorFlow.`;
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
    kind: EVENT_KINDS.some((k) => k.value === kind) ? (kind as RemediationEventKind) : null,
    actor: ACTOR_KINDS.some((a) => a.value === actor) ? (actor as ActorKind) : null,
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
