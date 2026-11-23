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
import { t } from "@/i18n";
import { acceptance } from "@/lib/commandKinds";
import { riskWord, rollbackText, ruleReason, tierName, timeLeft } from "@/lib/remediation";
import { administersEverything, useMe, useMultiCustomer } from "@/lib/me";
import { AI_COMMANDS_HREF, AI_SETTINGS_PATH, AUDIT_PATH, auditHref, incidentsHref, serverHref } from "@/lib/routes";
import { scopedHref, useScope } from "@/lib/scope";
import { seriesTitle } from "@/lib/sensors";
import { usePolledData } from "@/lib/usePolledData";
import { useUrlParams } from "@/lib/urlState";
import { Button, ButtonLink } from "@/components/ui/Button";
import { cx } from "@/components/ui/cx";
import { type Column, DataTable, PrimaryCell } from "@/components/ui/DataTable";
import { FilterBar, NoMatches } from "@/components/ui/FilterBar";
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
  const multiCustomer = useMultiCustomer();
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
      header: t("remediation.queue.incident"),
      cell: (p) => (
        <PrimaryCell
          title={t("remediation.queue.on", { title: seriesTitle(undefined, p.metric, p.instance), hostname: p.hostname })}
          href={scopedHref(serverHref(p.server_id), scope)}
          sub={[t("remediation.queue.step", { step: p.commands_run + 1, max: p.max_commands }), p.site_name, multiCustomer && p.customer_name].filter(Boolean).join(" · ")}
        />
      ),
    },
    { key: "tier", header: t("remediation.queue.tier"), cell: (p) => <StatusLabel kind="risk" value={riskWord(p.tier)} /> },
    {
      key: "command",
      header: t("remediation.queue.command"),
      cell: (p) => <code className={styles.command}>{p.command}</code>,
      className: styles.wrap,
    },
    {
      key: "deadline",
      header: t("remediation.queue.deadline"),
      cell: (p) => {
        const left = timeLeft(p.deadline, now);
        return (
          <time dateTime={p.deadline} title={formatExact(new Date(p.deadline))} className={cx(!left && styles.expired)}>
            {left ?? t("remediation.queue.passed")}
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
        title={t("remediation.queue.title")}
        meta={<RefreshLine updatedAt={updatedAt} stale={stale} />}
      />
      <FilterBar
        filters={[
          {
            name: "tier",
            label: t("remediation.queue.tier"),
            options: TIERS.map((value) => ({ value, label: tierName(value) })),
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
        <ErrorState what={t("remediation.queue.what")} error={describeError(error)} onRetry={refresh} />
      ) : data && shown.length === 0 ? (
        tier ? (
          <NoMatches names={["tier"]} title={t("remediation.queue.nothing")}>
            {t("remediation.queue.noneOfTier")}
          </NoMatches>
        ) : (
          <EmptyState
            title={t("remediation.queue.nothing")}
            actions={
              <>
                <ButtonLink href={AUDIT_PATH}>{t("remediation.queue.seeAudit")}</ButtonLink>
                <ButtonLink href={scopedHref(incidentsHref(), scope)}>{t("remediation.queue.openIncidents")}</ButtonLink>
              </>
            }
          >
            <HowItArrives />
          </EmptyState>
        )
      ) : (
        <div className={styles.page}>
          <DataTable
            label={t("remediation.queue.label")}
            columns={columns}
            rows={shown}
            rowKey={(p) => p.id}
            loading={!data}
            rowActions={(p) => (
              <Button
                size="small"
                aria-expanded={open === p.id}
                aria-label={
                  open === p.id
                    ? t("remediation.queue.closeFor", { hostname: p.hostname })
                    : t("remediation.queue.reviewFor", { hostname: p.hostname })
                }
                onClick={() => setOpen(open === p.id ? null : p.id)}
              >
                {open === p.id ? t("remediation.queue.close") : t("remediation.queue.review")}
              </Button>
            )}
            rowDetail={(p) =>
              open === p.id ? <ApprovalDetail item={p} now={now} onDone={done} /> : null
            }
          />
        </div>
      )}
      <p className={styles.note}>
        {t("remediation.queue.note")}
      </p>
    </Page>
  );
}

/**
 * What an empty queue means: the AI parks a command here only when it
 * proposes a change the risk rules keep for a person, so most days there
 * is none. Says how one gets here, so an empty page is not read as broken.
 */
function HowItArrives() {
  const me = useMe();
  return (
    <>
      <p className={styles.emptyText}>{t("remediation.queue.empty")}</p>
      <p className={styles.emptyText}>
        <strong>{t("remediation.queue.howTitle")}</strong>
      </p>
      <ol className={styles.howList}>
        <li>{t("remediation.queue.howAi")}</li>
        <li>
          {t("remediation.queue.howServer")}
          {/* Settings › AI is for global administrators; others switch per site or server. */}
          {administersEverything(me) && (
            <>
              {" "}
              <Link href={AI_COMMANDS_HREF}>{t("remediation.queue.howServerAll")}</Link>
            </>
          )}
        </li>
        <li>{t("remediation.queue.howAlert")}</li>
        <li>{t("remediation.queue.howRules")}</li>
      </ol>
    </>
  );
}

/** Says that nothing new will arrive here, and who can change that. */
function AiOff() {
  const me = useMe();
  return (
    <Banner tone="warn">
      <strong>{t("remediation.aiOff.title")}</strong> {t("remediation.aiOff.body")}{" "}
      {administersEverything(me) ? (
        <Link href={AI_SETTINGS_PATH}>{t("remediation.aiOff.turnOn")}</Link>
      ) : (
        t("remediation.aiOff.askAdmin")
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
  const rollback = rollbackText(item.snapshots);
  const scope = useScope();
  const shell = acceptance(item.shell_acceptance);
  const refused = shell === "off" || shell === "unsupported";

  async function decide(action: () => Promise<ApprovalDecision>, message: (d: ApprovalDecision) => string) {
    setBusy(true);
    setError(null);
    try {
      const decision = await action();
      onDone({ text: message(decision), tone: "ok" });
    } catch (e) {
      if (e instanceof ApiError && e.status === 409) {
        onDone({ text: t("remediation.detail.notRecorded", { hostname: item.hostname, message: e.message }), tone: "warn" });
      } else {
        setError(describeError(e));
        setBusy(false);
      }
    }
  }

  const approved = (d: ApprovalDecision) =>
    d.edited
      ? t("remediation.detail.approvedEdited", { command: d.command, hostname: item.hostname })
      : t("remediation.detail.approved", { command: d.command, hostname: item.hostname });

  return (
    <div className={styles.detail}>
      <dl className={styles.facts}>
        <dt>{t("remediation.detail.diagnosis")}</dt>
        <dd className={styles.untrusted}>{item.diagnosis}</dd>
        <dt>{t("remediation.detail.reasoning")}</dt>
        <dd>
          <span className={styles.untrusted}>{item.reasoning}</span>{" "}
          <span className={styles.dim}>{t("remediation.detail.untrusted")}</span>
        </dd>
        <dt>{t("remediation.detail.command")}</dt>
        <dd>
          <code className={styles.command}>{item.command}</code>{" "}
          <span className={styles.dim}>
            {item.shell === "powershell"
              ? t("remediation.detail.powershell")
              : t("remediation.detail.sh")}
          </span>
        </dd>
        <dt>{t("remediation.detail.rule")}</dt>
        <dd>
          {item.rule.id} · {ruleReason(item.rule, item.reason)} <span className={styles.dim}>{t("remediation.detail.byRules")}</span>
        </dd>
        <dt>{t("remediation.detail.rollback")}</dt>
        <dd>
          {rollbackLimited ? (
            <>
              <Label tone="warn" small>
                {t("remediation.detail.rollbackLimited")}
              </Label>{" "}
              {rollback.text}
              {rollback.note && <span className={styles.dim}> {rollback.note}</span>}
            </>
          ) : (
            <>
              {rollback.text}
              {rollback.note && <span className={styles.dim}> {rollback.note}</span>}
            </>
          )}
        </dd>
        <dt>{t("remediation.detail.deadline")}</dt>
        <dd>
          {left
            ? t("remediation.detail.leftAt", { left, at: formatExact(new Date(item.deadline)) })
            : t("remediation.detail.passed")}
        </dd>
        <dt>{t("remediation.detail.history")}</dt>
        <dd>
          <Link href={auditHref({ incident: item.incident_id })}>{t("remediation.detail.incidentAudit")}</Link>
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
          <label htmlFor={commandId}>{t("remediation.detail.editedCommand")}</label>
          <textarea
            id={commandId}
            className={cx(styles.textarea, styles.mono)}
            value={command}
            onChange={(e) => setCommand(e.target.value)}
            disabled={busy}
            spellCheck={false}
          />
          <p className={styles.note}>
            {t("remediation.detail.editNote")}
          </p>
          <div className={styles.actions}>
            <Button type="submit" variant="k" disabled={busy || command.trim() === ""}>
              {busy ? t("remediation.detail.approving") : t("remediation.detail.approveEdited")}
            </Button>
            <Button onClick={() => setMode("view")} disabled={busy}>
              {t("remediation.detail.cancel")}
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
              () => t("remediation.detail.rejected", { command: item.command, hostname: item.hostname }),
            );
          }}
        >
          <label htmlFor={reasonId}>{t("remediation.detail.reason")}</label>
          <textarea
            id={reasonId}
            className={styles.textarea}
            value={reason}
            maxLength={1000}
            onChange={(e) => setReason(e.target.value)}
            disabled={busy}
          />
          <p className={styles.note}>{t("remediation.detail.reasonNote")}</p>
          <div className={styles.actions}>
            <Button type="submit" variant="danger" disabled={busy || reason.trim() === ""}>
              {busy ? t("remediation.detail.rejecting") : t("remediation.detail.reject")}
            </Button>
            <Button onClick={() => setMode("view")} disabled={busy}>
              {t("remediation.detail.cancel")}
            </Button>
          </div>
        </form>
      ) : (
        <div className={styles.actions}>
          {refused && (
            <p className={styles.note}>
              {shell === "off" ? (
                <>
                  {t("remediation.detail.commandsOff", { hostname: item.hostname })}{" "}
                  <Link href={scopedHref(serverHref(item.server_id, "commands"), scope)}>
                    {t("remediation.detail.howToTurnOn")}
                  </Link>
                </>
              ) : item.shell === "powershell" ? (
                t("remediation.detail.refusesWindows", { hostname: item.hostname })
              ) : (
                t("remediation.detail.refusesSystem", { hostname: item.hostname })
              )}
            </p>
          )}
          {item.can_approve && !refused && (
            <>
              <Button
                variant="k"
                disabled={busy}
                onClick={() => void decide(() => api.approve(item.id), approved)}
              >
                {busy ? t("remediation.detail.approving") : t("remediation.detail.approve")}
              </Button>
              <Button disabled={busy} onClick={() => setMode("edit")}>
                {t("remediation.detail.edit")}
              </Button>
            </>
          )}
          {item.can_reject && (
            <Button variant="danger" disabled={busy} onClick={() => setMode("reject")}>
              {t("remediation.detail.rejectOpen")}
            </Button>
          )}
          {!item.can_approve && (
            <p className={styles.note}>
              {item.can_reject
                ? t("remediation.detail.adminOnly")
                : t("remediation.detail.readOnly")}
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
