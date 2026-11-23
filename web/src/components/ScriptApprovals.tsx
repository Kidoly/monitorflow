"use client";

import { useCallback } from "react";

import { t } from "@/i18n";
import { type ScriptRun, api } from "@/lib/api";
import { riskWord } from "@/lib/remediation";
import { scriptRunHref } from "@/lib/routes";
import { runProgress } from "@/lib/scripts";
import { usePolledData } from "@/lib/usePolledData";
import { type Column, DataTable, PrimaryCell } from "@/components/ui/DataTable";
import { StatusLabel } from "@/components/ui/StatusLabel";
import { formatExact } from "@/components/ui/time";

import admin from "./AdminPages.module.css";
import { requester } from "./ScriptLibrary";
import { Decision } from "./ScriptRunPage";

/**
 * Script runs waiting for a decision (ADR-0179), under the AI's commands
 * on the approval queue: runs asked by someone who may not approve their
 * tier, and runs of automation rules the risk rules keep for a person.
 * Hidden while there is none, or when they cannot be read.
 */
export function ScriptApprovals() {
  const fetcher = useCallback((signal: AbortSignal) => api.scriptRuns({ state: "awaiting_approval" }, signal), []);
  const { data, refresh } = usePolledData(fetcher);
  const runs = data?.runs ?? [];
  if (runs.length === 0) return null;
  const columns: Column<ScriptRun>[] = [
    {
      key: "run",
      header: t("scripts.approvals.script"),
      cell: (r) => (
        <PrimaryCell title={t("scripts.runs.of", { name: r.script_name, version: r.version })} href={scriptRunHref(r.id)} sub={requester(r)} />
      ),
    },
    { key: "tier", header: t("scripts.runs.tier"), cell: (r) => <StatusLabel kind="risk" value={riskWord(r.tier)} /> },
    { key: "servers", header: t("scripts.runs.servers.header"), cell: (r) => runProgress(r) },
    {
      key: "expires",
      header: t("scripts.approvals.expires"),
      cell: (r) => (r.expires_at ? <time dateTime={r.expires_at}>{formatExact(new Date(r.expires_at))}</time> : null),
    },
  ];
  return (
    <section className={admin.section} aria-label={t("scripts.approvals.title")}>
      <div className={admin.sectionHead}>
        <h2 className={admin.sectionTitle}>{t("scripts.approvals.title")}</h2>
      </div>
      <p className={admin.sectionNote}>{t("scripts.approvals.note")}</p>
      <DataTable
        bare
        label={t("scripts.approvals.title")}
        columns={columns}
        rows={runs}
        rowKey={(r) => r.id}
        rowActions={(r) => <Decision compact run={r} onDone={() => refresh()} />}
      />
    </section>
  );
}
