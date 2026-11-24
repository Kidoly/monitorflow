"use client";

import Link from "next/link";
import { useSearchParams } from "next/navigation";
import { useCallback, useEffect, useState } from "react";

import { t } from "@/i18n";
import { ApiError, type ScriptRun, type ScriptRunDetail, type ScriptRunServer, api, describeError, isAbortError } from "@/lib/api";
import { formatDateTime } from "@/lib/format";
import { riskWord } from "@/lib/remediation";
import { APPROVALS_PATH, SCRIPTS_PATH, scriptHref, serverHref } from "@/lib/routes";
import { runProgress, runStatus } from "@/lib/scripts";
import { usePolling } from "@/lib/usePolling";
import { UUID } from "@/lib/uuid";
import { Breadcrumb } from "@/components/ui/Breadcrumb";
import { Button, ButtonLink, fieldClass } from "@/components/ui/Button";
import { type Column, DataTable } from "@/components/ui/DataTable";
import { FactsBox } from "@/components/ui/FactsBox";
import { Page } from "@/components/ui/Page";
import { Banner, LoadingBlock, NotFound, PageHeader } from "@/components/ui/PageState";
import { StatusLabel, type StatusValue } from "@/components/ui/StatusLabel";

import admin from "./AdminPages.module.css";
import { requester } from "./ScriptLibrary";
import styles from "./Scripts.module.css";

const REFRESH_MS = 5_000;

/** Nothing changes any more: decided against, or every server done. */
function settled(detail: ScriptRunDetail | null | undefined): boolean {
  if (!detail) return detail === null;
  const { run } = detail;
  return ["rejected", "expired", "cancelled", "refused"].includes(run.state) || (run.state === "dispatched" && run.counts.pending === 0);
}

/** `/remediation/runs/view/?id=…` (ADR-0020 routing). */
export function ScriptRunView() {
  const id = useSearchParams().get("id");
  return <ScriptRunPage key={id} id={id} />;
}

export function ScriptRunPage({ id }: { id: string | null }) {
  const valid = id !== null && UUID.test(id);
  const [detail, setDetail] = useState<ScriptRunDetail | null | undefined>(undefined);
  const [error, setError] = useState<string | null>(null);
  const load = useCallback(
    async (signal?: AbortSignal) => {
      try {
        setDetail(await api.scriptRun(id ?? "", signal));
        setError(null);
      } catch (e) {
        if (isAbortError(e)) return;
        if (e instanceof ApiError && e.status === 404) setDetail(null);
        else setError(describeError(e));
      }
    },
    [id],
  );
  usePolling(load, REFRESH_MS, valid && !settled(detail));
  useEffect(() => {
    if (detail) document.title = `${detail.run.script_name} · MonitorFlow`;
  }, [detail]);

  if (!valid || detail === null) {
    return (
      <NotFound title={t("scripts.runPage.notFound")} actions={<ButtonLink href={SCRIPTS_PATH}>{t("scripts.back")}</ButtonLink>}>
        {t("scripts.runPage.notFoundText")}
      </NotFound>
    );
  }
  if (!detail) return error ? <Banner tone="bad">{error}</Banner> : <LoadingBlock label={t("scripts.loading")} />;
  const { run } = detail;
  const parameters = Object.entries(run.parameters);
  const facts = [
    {
      label: t("scripts.runPage.script"),
      value: <Link href={scriptHref(run.script_id)}>{t("scripts.runs.of", { name: run.script_name, version: run.version })}</Link>,
    },
    { label: t("scripts.runPage.requestedBy"), value: requester(run) },
    ...(run.trigger?.metric ? [{ label: t("scripts.runPage.trigger"), value: run.trigger.metric, mono: true }] : []),
    {
      label: t("scripts.runPage.parameters"),
      value: parameters.length ? parameters.map(([k, v]) => `${k}=${v}`).join(" ") : t("scripts.runPage.noParameters"),
      mono: parameters.length > 0,
    },
    { label: t("scripts.runPage.risk"), value: <StatusLabel kind="risk" value={riskWord(run.tier)} /> },
    { label: t("scripts.runPage.why"), value: run.reason },
    { label: t("scripts.runPage.scheduled"), value: formatDateTime(run.scheduled_at) },
    ...(run.decided_at
      ? [{ label: t("scripts.runPage.decided"), value: t("scripts.runPage.decidedBy", { name: run.decided_by_name ?? t("scripts.runs.deletedUser"), at: formatDateTime(run.decided_at) }) }]
      : []),
    ...(run.decision_reason ? [{ label: t("scripts.runPage.decisionReason"), value: run.decision_reason }] : []),
    ...(run.expires_at ? [{ label: t("scripts.runPage.expires"), value: formatDateTime(run.expires_at) }] : []),
    { label: t("scripts.runPage.servers"), value: runProgress(run) },
  ];
  const columns: Column<ScriptRunServer>[] = [
    { key: "server", header: t("scripts.runPage.server"), cell: (s) => <Link href={serverHref(s.server_id)}>{s.hostname}</Link> },
    { key: "status", header: t("scripts.runPage.status"), cell: (s) => <StatusLabel kind="scriptServer" value={s.status as StatusValue<"scriptServer">} /> },
    { key: "exit", header: t("scripts.runPage.exitCode"), cell: (s) => (s.exit_code === null ? t("scripts.runPage.noExit") : String(s.exit_code)) },
    { key: "finished", header: t("scripts.runPage.finished"), cell: (s) => (s.finished_at ? formatDateTime(s.finished_at) : t("scripts.runPage.notYet")) },
  ];
  return (
    <Page>
      <Breadcrumb parents={[{ label: t("scripts.title"), href: SCRIPTS_PATH }, { label: run.script_name, href: scriptHref(run.script_id) }]} />
      <PageHeader
        detail
        title={t("scripts.runPage.title", { name: run.script_name })}
        status={<StatusLabel kind="scriptRun" value={runStatus(run) as StatusValue<"scriptRun">} />}
        actions={<Decision run={run} onDone={setDetail} />}
      />
      {error && <Banner tone="bad">{error}</Banner>}
      {run.state === "awaiting_approval" && !run.can_approve && (
        <Banner tone="warn">
          {t("scripts.runPage.waiting")} <Link href={APPROVALS_PATH}>{t("scripts.runPage.queue")}</Link>
        </Banner>
      )}
      <FactsBox facts={facts} />
      <section className={admin.section} aria-label={t("scripts.runPage.serversTitle")}>
        <div className={admin.sectionHead}>
          <h2 className={admin.sectionTitle}>{t("scripts.runPage.serversTitle")}</h2>
        </div>
        <DataTable
          bare
          label={t("scripts.runPage.serversTitle")}
          columns={columns}
          rows={detail.servers}
          rowKey={(s) => s.server_id}
          detailInset
          rowDetail={(s) => <ServerOutput server={s} />}
        />
      </section>
      <section className={admin.section} aria-label={t("scripts.runPage.body")}>
        <div className={admin.sectionHead}>
          <h2 className={admin.sectionTitle}>{t("scripts.runPage.body")}</h2>
        </div>
        <div className={styles.sectionBody}>
          <pre className={styles.pre}>{detail.body}</pre>
        </div>
      </section>
    </Page>
  );
}

/** A server's output, or why it has none. Wraps, never scrolls on its own. */
function ServerOutput({ server }: { server: ScriptRunServer }) {
  if (server.status === "waiting" || server.status === "queued" || server.status === "sent") return null;
  const parts = [
    { label: t("scripts.runPage.error"), text: server.error },
    { label: t("scripts.runPage.stdout"), text: server.stdout },
    { label: t("scripts.runPage.stderr"), text: server.stderr },
  ].filter((p) => p.text);
  if (parts.length === 0) return <p className={styles.muted}>{t("scripts.runPage.noOutput")}</p>;
  return (
    <div className={styles.output}>
      {parts.map((p) => (
        <div key={p.label}>
          <span className={styles.outputLabel}>{p.label}</span>
          <pre className={styles.pre}>{p.text}</pre>
        </div>
      ))}
    </div>
  );
}

/** Approve, reject or cancel, as the caller may. */
export function Decision({ run, onDone, compact }: { run: ScriptRun; onDone: (detail: ScriptRunDetail) => void; compact?: boolean }) {
  const [rejecting, setRejecting] = useState(false);
  const [reason, setReason] = useState("");
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const act = async (action: () => Promise<ScriptRunDetail>) => {
    setBusy(true);
    setError(null);
    try {
      onDone(await action());
      setRejecting(false);
    } catch (e) {
      setError(describeError(e));
    } finally {
      setBusy(false);
    }
  };
  if (!run.can_approve && !run.can_reject && !run.can_cancel) return null;
  const size = compact ? "small" : undefined;
  return (
    <div className={styles.inline}>
      {rejecting ? (
        <>
          <input
            className={fieldClass}
            aria-label={t("scripts.runPage.rejectReason")}
            placeholder={t("scripts.runPage.rejectReason")}
            maxLength={1000}
            value={reason}
            onChange={(e) => setReason(e.target.value)}
          />
          <Button size={size} variant="danger" disabled={busy || !reason.trim()} onClick={() => act(() => api.rejectScriptRun(run.id, reason.trim()))}>
            {t("scripts.runPage.reject")}
          </Button>
          <Button size={size} disabled={busy} onClick={() => setRejecting(false)}>
            {t("scripts.form.cancel")}
          </Button>
        </>
      ) : (
        <>
          {run.can_approve && (
            <Button size={size} variant="k" disabled={busy} onClick={() => act(() => api.approveScriptRun(run.id))}>
              {t("scripts.runPage.approve")}
            </Button>
          )}
          {run.can_reject && (
            <Button size={size} disabled={busy} onClick={() => setRejecting(true)}>
              {t("scripts.runPage.rejectOpen")}
            </Button>
          )}
          {run.can_cancel && !compact && (
            <Button size={size} disabled={busy} onClick={() => act(() => api.cancelScriptRun(run.id))}>
              {t("scripts.runPage.cancelRun")}
            </Button>
          )}
        </>
      )}
      {error && (
        <span className={admin.formError} role="alert">
          {error}
        </span>
      )}
    </div>
  );
}
