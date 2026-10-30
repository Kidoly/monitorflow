"use client";

import Link from "next/link";
import { useCallback, useEffect, useId, useState } from "react";

import {
  type ApprovalDecision,
  ApiError,
  type PendingApproval,
  type RiskTier,
  api,
  describeError,
  isUnauthorized,
} from "@/lib/api";
import { riskWord, snapshotReason, timeLeft } from "@/lib/remediation";
import { administersEverything, useMe } from "@/lib/me";
import { AI_SETTINGS_PATH, AUDIT_PATH, auditHref, serverHref } from "@/lib/routes";
import { scopedHref, useScope } from "@/lib/scope";
import { seriesTitle } from "@/lib/sensors";
import { usePolledData } from "@/lib/usePolledData";
import { useUrlParams } from "@/lib/urlState";
import { Button, ButtonLink } from "@/components/ui/Button";
import { cx } from "@/components/ui/cx";
import { type Column, DataTable, PrimaryCell } from "@/components/ui/DataTable";
import { FilterBar } from "@/components/ui/FilterBar";
import { Page } from "@/components/ui/Page";
import { Banner, EmptyState, ErrorState, PageHeader, RefreshLine, StaleBanner } from "@/components/ui/PageState";
import { Label, StatusLabel } from "@/components/ui/StatusLabel";
import { formatExact } from "@/components/ui/time";

import styles from "./Remediation.module.css";

/** A clock that moves every second, for deadlines counting down between refreshes. */
function useNow(): Date {
  const [now, setNow] = useState(() => new Date());
  useEffect(() => {
    const timer = setInterval(() => setNow(new Date()), 1000);
    return () => clearInterval(timer);
  }, []);
  return now;
}

const TIERS: RiskTier[] = ["low_impact", "high_impact"];

/** The approval queue (ADR-0045): commands the risk rules parked for a human. */
export function ApprovalQueue() {
  const scope = useScope();
  const [params] = useUrlParams();
  const tier = TIERS.find((t) => t === params.get("tier")) ?? null;
  const now = useNow();
  const [open, setOpen] = useState<string | null>(null);
  const [notice, setNotice] = useState<Notice | null>(null);

  const fetcher = useCallback((signal: AbortSignal) => api.approvals(signal), []);
  const { data, error, stale, failures, updatedAt, refresh } = usePolledData(fetcher);
  const aiFetcher = useCallback((signal: AbortSignal) => api.aiSettings(signal), []);
  const ai = usePolledData(aiFetcher);

  const inScope = (p: PendingApproval) =>
    (!scope.customerId || p.customer_id === scope.customerId) && (!scope.siteId || p.site_id === scope.siteId);
  const all = data?.approvals.filter(inScope) ?? [];
  const shown = all.filter((p) => !tier || p.tier === tier);

  const columns: Column<PendingApproval>[] = [
    {
      key: "incident",
      header: "Incident",
      cell: (p) => (
        <PrimaryCell
          title={`${seriesTitle(undefined, p.metric, p.instance)} on ${p.hostname}`}
          href={scopedHref(serverHref(p.server_id), scope)}
          sub={`Step ${p.commands_run + 1} of ${p.max_commands} · ${p.site_name} · ${p.customer_name}`}
        />
      ),
    },
    { key: "tier", header: "Tier", cell: (p) => <StatusLabel kind="risk" value={riskWord(p.tier)} /> },
    {
      key: "command",
      header: "Command",
      cell: (p) => <code className={styles.command}>{p.command}</code>,
      className: styles.wrap,
    },
    {
      key: "deadline",
      header: "Deadline",
      cell: (p) => {
        const left = timeLeft(p.deadline, now);
        return (
          <time dateTime={p.deadline} title={formatExact(new Date(p.deadline))} className={cx(!left && styles.expired)}>
            {left ?? "Passed"}
          </time>
        );
      },
    },
  ];

  function done(next: Notice) {
    setNotice(next);
    setOpen(null);
    refresh();
  }

  return (
    <Page>
      <PageHeader
        title="Approvals"
        meta={
          <>
            {data && `${all.length} waiting · `}
            <RefreshLine updatedAt={updatedAt} stale={stale} />
          </>
        }
        actions={<ButtonLink href={scopedHref(AUDIT_PATH, scope)}>Audit log</ButtonLink>}
      />
      <FilterBar
        filters={[
          {
            name: "tier",
            label: "Tier",
            options: [
              { value: "low_impact", label: "Low-impact" },
              { value: "high_impact", label: "High-impact" },
            ],
          },
        ]}
      />
      {ai.data && !ai.data.enabled && <AiOff />}
      {notice && (
        <Banner tone={notice.tone} role="status">
          {notice.text}
        </Banner>
      )}
      {stale && updatedAt && (
        <StaleBanner error={describeError(error)} updatedAt={updatedAt} failures={failures} onRetry={refresh} />
      )}
      {!data && error !== null && !isUnauthorized(error) ? (
        <ErrorState what="the approval queue" error={describeError(error)} onRetry={refresh} />
      ) : data && shown.length === 0 ? (
        <EmptyState title="Nothing waiting for approval">
          {tier ? "No command of this tier is waiting." : "Commands the risk rules hold for a human appear here."}
        </EmptyState>
      ) : (
        <div className={styles.page}>
          <DataTable
            label="Commands waiting for approval"
            columns={columns}
            rows={shown}
            rowKey={(p) => p.id}
            loading={!data}
            rowActions={(p) => (
              <Button
                size="small"
                aria-expanded={open === p.id}
                aria-label={`${open === p.id ? "Close" : "Review"} the command for ${p.hostname}`}
                onClick={() => setOpen(open === p.id ? null : p.id)}
              >
                {open === p.id ? "Close" : "Review"}
              </Button>
            )}
            rowDetail={(p) =>
              open === p.id ? <ApprovalDetail item={p} now={now} onDone={done} /> : null
            }
          />
        </div>
      )}
      <p className={styles.note}>
        Approved commands run once on the agent, as its user. Every decision is recorded in the incident&apos;s audit log.
      </p>
    </Page>
  );
}

/** Says that nothing new will arrive here, and who can change that. */
function AiOff() {
  const me = useMe();
  return (
    <Banner tone="warn">
      <strong>AI is off.</strong> No new commands are proposed, and any waiting here stay on hold until it is back on.{" "}
      {administersEverything(me) ? (
        <Link href={AI_SETTINGS_PATH}>Turn on AI</Link>
      ) : (
        "A global administrator can turn it on under Administration › AI."
      )}
    </Banner>
  );
}

/** The outcome of the last decision, shown above the table once its row is gone. */
interface Notice {
  text: string;
  tone: "ok" | "warn";
}

type Mode = "view" | "edit" | "reject";

/** What an approver needs to decide on one command, and the decision. */
function ApprovalDetail({
  item,
  now,
  onDone,
}: {
  item: PendingApproval;
  now: Date;
  /** After a decision, or a conflict that means the row is out of date. */
  onDone: (notice: Notice) => void;
}) {
  const [mode, setMode] = useState<Mode>("view");
  const [command, setCommand] = useState(item.command);
  const [reason, setReason] = useState("");
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const commandId = useId();
  const reasonId = useId();
  const left = timeLeft(item.deadline, now);
  const rollbackLimited = item.tier !== "read_only" && !item.snapshots.available;

  async function decide(action: () => Promise<ApprovalDecision>, message: (d: ApprovalDecision) => string) {
    setBusy(true);
    setError(null);
    try {
      const decision = await action();
      onDone({ text: message(decision), tone: "ok" });
    } catch (e) {
      if (e instanceof ApiError && e.status === 409) {
        onDone({ text: `Not recorded for ${item.hostname}: ${e.message}. The queue has been refreshed.`, tone: "warn" });
      } else {
        setError(describeError(e));
        setBusy(false);
      }
    }
  }

  const approved = (d: ApprovalDecision) =>
    `Approved ${d.edited ? "the edited command " : ""}${d.command} on ${item.hostname}. The incident resumes within a few seconds.`;

  return (
    <div className={styles.detail}>
      <dl className={styles.facts}>
        <dt>Diagnosis</dt>
        <dd className={styles.untrusted}>{item.diagnosis}</dd>
        <dt>AI reasoning</dt>
        <dd>
          <span className={styles.untrusted}>{item.reasoning}</span>{" "}
          <span className={styles.dim}>Generated text, untrusted, never executed as-is.</span>
        </dd>
        <dt>Command</dt>
        <dd>
          <code className={styles.command}>{item.command}</code>
        </dd>
        <dt>Rule</dt>
        <dd>
          {item.rule.id} · {item.reason} <span className={styles.dim}>Tier decided by the rules, not by the AI.</span>
        </dd>
        <dt>Rollback</dt>
        <dd>
          {rollbackLimited ? (
            <>
              <Label tone="warn" small>
                Rollback limited
              </Label>{" "}
              No snapshot before this change: {snapshotReason(item.snapshots.reason)}
            </>
          ) : (
            "A Proxmox snapshot of the VM is taken before the command runs."
          )}
        </dd>
        <dt>Deadline</dt>
        <dd>
          {left ? `${left}, at ${formatExact(new Date(item.deadline))}` : "Passed: the incident escalates to a human."}
        </dd>
        <dt>History</dt>
        <dd>
          <Link href={auditHref({ incident: item.incident_id })}>Audit log of this incident</Link>
        </dd>
      </dl>

      {!left ? null : mode === "edit" ? (
        <form
          className={styles.form}
          onSubmit={(e) => {
            e.preventDefault();
            void decide(() => api.approve(item.id, command), approved);
          }}
        >
          <label htmlFor={commandId}>Edited command</label>
          <textarea
            id={commandId}
            className={cx(styles.textarea, styles.mono)}
            value={command}
            onChange={(e) => setCommand(e.target.value)}
            disabled={busy}
            spellCheck={false}
          />
          <p className={styles.note}>
            The rules classify the edited command again: a forbidden command is refused, and a higher tier may need an
            administrator.
          </p>
          <div className={styles.actions}>
            <Button type="submit" variant="k" disabled={busy || command.trim() === ""}>
              {busy ? "Approving…" : "Approve edited command"}
            </Button>
            <Button onClick={() => setMode("view")} disabled={busy}>
              Cancel
            </Button>
          </div>
        </form>
      ) : mode === "reject" ? (
        <form
          className={styles.form}
          onSubmit={(e) => {
            e.preventDefault();
            void decide(
              () => api.reject(item.id, reason.trim()),
              () => `Rejected ${item.command} on ${item.hostname}. The AI gets the reason and may propose another step.`,
            );
          }}
        >
          <label htmlFor={reasonId}>Reason</label>
          <textarea
            id={reasonId}
            className={styles.textarea}
            value={reason}
            maxLength={1000}
            onChange={(e) => setReason(e.target.value)}
            disabled={busy}
          />
          <p className={styles.note}>The reason is recorded and sent to the AI provider. Do not include secrets.</p>
          <div className={styles.actions}>
            <Button type="submit" variant="danger" disabled={busy || reason.trim() === ""}>
              {busy ? "Rejecting…" : "Reject command"}
            </Button>
            <Button onClick={() => setMode("view")} disabled={busy}>
              Cancel
            </Button>
          </div>
        </form>
      ) : (
        <div className={styles.actions}>
          {item.can_approve && (
            <>
              <Button
                variant="k"
                disabled={busy}
                onClick={() => void decide(() => api.approve(item.id), approved)}
              >
                {busy ? "Approving…" : "Approve"}
              </Button>
              <Button disabled={busy} onClick={() => setMode("edit")}>
                Edit
              </Button>
            </>
          )}
          {item.can_reject && (
            <Button variant="danger" disabled={busy} onClick={() => setMode("reject")}>
              Reject…
            </Button>
          )}
          {!item.can_approve && (
            <p className={styles.note}>
              {item.can_reject
                ? "Only an administrator can approve a high-impact command."
                : "Your role can read this command but not decide on it."}
            </p>
          )}
        </div>
      )}
      {error && (
        <p className={styles.error} role="alert">
          {error}
        </p>
      )}
    </div>
  );
}
