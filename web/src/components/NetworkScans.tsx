"use client";

import { useRouter } from "next/navigation";
import { type FormEvent, useCallback, useEffect, useState } from "react";

import { t } from "@/i18n";
import {
  type NetworkScan,
  type NetworkScanRequest,
  type ScanEvery,
  type Site,
  type SnmpCredentialsRequest,
  api,
  describeError,
} from "@/lib/api";
import { placeLabel } from "@/lib/customers";
import { DEFAULT_PORTS, everyLabel, parsePorts, probeWords, splitTargets, validTarget } from "@/lib/discovery";
import { modifiesSite, multiCustomer, multiSite, useMe } from "@/lib/me";
import { discoveryHref } from "@/lib/routes";
import { scopedHref, useScope } from "@/lib/scope";
import { usePolledData } from "@/lib/usePolledData";
import { Button, fieldClass } from "@/components/ui/Button";
import { type Column, DataTable, PrimaryCell } from "@/components/ui/DataTable";
import { Missing } from "@/components/ui/Missing";
import { Page } from "@/components/ui/Page";
import { EmptyState, ErrorState, LoadingBlock, PageHeader, RefreshLine, StaleBanner } from "@/components/ui/PageState";
import { RelativeTime } from "@/components/ui/RelativeTime";

import styles from "./AdminPages.module.css";
import { ProbeSelect } from "./AddDevice";
import { NEW_CREDENTIALS, SnmpFields, snmpProblem, snmpRequest } from "./SnmpCredentials";

/** The sites the user may scan, read once. */
export function useScannableSites(): Site[] | null {
  const me = useMe();
  const [sites, setSites] = useState<Site[] | null>(null);
  useEffect(() => {
    const controller = new AbortController();
    api
      .sites(controller.signal)
      .then(({ sites }) => setSites(sites.filter((site) => modifiesSite(me, site.customer_id, site.id))))
      .catch(() => {
        if (!controller.signal.aborted) setSites([]);
      });
    return () => controller.abort();
  }, [me]);
  return sites;
}

/** What the form checks before sending; `null` when nothing is wrong. */
export function formProblems(name: string, targets: string, ports: string): Record<string, string> {
  const problems: Record<string, string> = {};
  if (name.trim() === "") problems.name = t("discovery.error.name");
  const entries = splitTargets(targets);
  const bad = entries.find((entry) => !validTarget(entry));
  if (entries.length === 0) problems.targets = t("discovery.error.targets");
  else if (bad) problems.targets = t("discovery.error.target", { entry: bad });
  if (parsePorts(ports) === null) problems.ports = t("discovery.error.ports");
  return problems;
}

/** New scan, or the change of `scan`: its addresses, ports, probe, schedule and SNMP credentials. */
export function ScanForm({
  scan,
  sites,
  onSaved,
  onCancel,
}: {
  scan?: NetworkScan;
  sites: Site[];
  onSaved: (scan: NetworkScan) => void;
  onCancel: () => void;
}) {
  const me = useMe();
  const scope = useScope();
  const [siteId, setSiteId] = useState(scan?.site_id ?? scope.siteId ?? sites[0]?.id ?? "");
  const [name, setName] = useState(scan?.name ?? "");
  const [targets, setTargets] = useState(scan?.targets.join("\n") ?? "");
  const [ports, setPorts] = useState((scan?.ports ?? DEFAULT_PORTS).join(", "));
  const [probe, setProbe] = useState(scan ? (scan.probe_relay_id ?? "central") : "");
  const [every, setEvery] = useState<ScanEvery | "">(scan?.every ?? "");
  const [notify, setNotify] = useState(scan?.notify ?? false);
  const [snmp, setSnmp] = useState(false);
  const [removeSnmp, setRemoveSnmp] = useState(false);
  const [credentials, setCredentials] = useState<SnmpCredentialsRequest>(NEW_CREDENTIALS);
  const [problems, setProblems] = useState<Record<string, string>>({});
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);

  async function submit(event: FormEvent) {
    event.preventDefault();
    const found = formProblems(name, targets, ports);
    if (snmp) {
      const problem = snmpProblem(credentials, false);
      if (problem) found.snmp = problem;
    }
    setProblems(found);
    if (Object.keys(found).length > 0) return;
    const body: NetworkScanRequest = {
      name: name.trim(),
      targets: splitTargets(targets),
      ports: parsePorts(ports) ?? [],
      every: every || null,
      notify,
      ...(probe && { probe }),
      ...(snmp && { snmp: snmpRequest(credentials) }),
      ...(removeSnmp && { remove_snmp: true }),
    };
    setBusy(true);
    setError(null);
    try {
      onSaved(scan ? await api.updateNetworkScan(scan.id, body) : await api.createNetworkScan({ ...body, site_id: siteId }));
    } catch (e) {
      setError(t("discovery.form.failed", { error: describeError(e) }));
      setBusy(false);
    }
  }

  const fieldError = (key: string) =>
    problems[key] && (
      <span role="alert" className={styles.formError}>
        {problems[key]}
      </span>
    );
  return (
    <form
      className={styles.panel}
      onSubmit={(e) => void submit(e)}
      aria-label={scan ? t("discovery.form.editTitle", { name: scan.name }) : t("discovery.form.newTitle")}
      noValidate
    >
      <label className={styles.field}>
        {t("discovery.form.name")}
        <input
          className={fieldClass}
          value={name}
          placeholder={t("discovery.form.namePlaceholder")}
          onChange={(e) => setName(e.target.value)}
          aria-invalid={problems.name ? true : undefined}
        />
        {fieldError("name")}
      </label>
      <label className={styles.field}>
        {t("discovery.form.targets")}
        <textarea
          className={fieldClass}
          rows={3}
          value={targets}
          placeholder={t("discovery.form.targetsPlaceholder")}
          spellCheck={false}
          onChange={(e) => setTargets(e.target.value)}
          aria-invalid={problems.targets ? true : undefined}
        />
        {fieldError("targets") || <span className={styles.sectionNote}>{t("discovery.form.targetsHint")}</span>}
      </label>
      {!scan && multiSite(me) && (
        <label className={styles.field}>
          {t("discovery.form.site")}
          <select
            className={fieldClass}
            value={siteId}
            onChange={(e) => {
              setSiteId(e.target.value);
              setProbe("");
            }}
          >
            {sites.map((site) => (
              <option key={site.id} value={site.id}>
                {placeLabel(site.customer_name, site.name, multiCustomer(me))}
              </option>
            ))}
          </select>
        </label>
      )}
      {siteId && (
        <ProbeSelect key={siteId} siteId={siteId} value={probe} onChange={setProbe} label={t("discovery.form.from")} />
      )}
      <label className={styles.field}>
        {t("discovery.form.ports")}
        <input
          className={fieldClass}
          value={ports}
          spellCheck={false}
          onChange={(e) => setPorts(e.target.value)}
          aria-invalid={problems.ports ? true : undefined}
        />
        {fieldError("ports") || <span className={styles.sectionNote}>{t("discovery.form.portsHint")}</span>}
      </label>
      <label className={styles.field}>
        {t("discovery.form.every")}
        <select className={fieldClass} value={every} onChange={(e) => setEvery(e.target.value as ScanEvery | "")}>
          {(["", "hour", "day", "week"] as const).map((value) => (
            <option key={value} value={value}>
              {everyLabel(value || null)}
            </option>
          ))}
        </select>
      </label>
      <label className={styles.check}>
        <input type="checkbox" checked={notify} onChange={(e) => setNotify(e.target.checked)} /> {t("discovery.form.notify")}
      </label>
      <label className={styles.check}>
        <input type="checkbox" checked={snmp} onChange={(e) => setSnmp(e.target.checked)} /> {t("discovery.form.snmp")}
      </label>
      <span className={styles.sectionNote}>{t("discovery.form.snmpHint")}</span>
      {snmp && (
        <fieldset className={styles.panel} aria-label={t("discovery.form.snmp")}>
          <SnmpFields value={credentials} onChange={setCredentials} />
          {fieldError("snmp")}
        </fieldset>
      )}
      {scan?.snmp && !snmp && (
        <label className={styles.check}>
          <input type="checkbox" checked={removeSnmp} onChange={(e) => setRemoveSnmp(e.target.checked)} />{" "}
          {t("discovery.form.removeSnmp")}
        </label>
      )}
      <p className={styles.sectionNote}>{t("discovery.form.polite")}</p>
      <div className={styles.formFoot}>
        <Button variant="k" type="submit" disabled={busy || !siteId}>
          {busy ? t("discovery.form.saving") : scan ? t("discovery.form.save") : t("discovery.form.create")}
        </Button>
        <Button onClick={onCancel} disabled={busy}>
          {t("discovery.form.cancel")}
        </Button>
      </div>
      {error && (
        <p className={styles.formError} role="alert">
          {error}
        </p>
      )}
    </form>
  );
}

/** Fleet › Network scans (ADR-0154). */
export function NetworkScansPage() {
  const me = useMe();
  const router = useRouter();
  const scope = useScope();
  const sites = useScannableSites();
  const [creating, setCreating] = useState(false);
  const fetcher = useCallback((signal: AbortSignal) => api.networkScans(signal), []);
  const { data, error, stale, failures, updatedAt, refresh } = usePolledData(fetcher);
  const scans = (data?.scans ?? []).filter((s) => !scope.siteId || s.site_id === scope.siteId);
  const canScan = (sites?.length ?? 0) > 0;
  const now = new Date();

  const columns: Column<NetworkScan>[] = [
    {
      key: "name",
      header: t("discovery.column.name"),
      sortValue: (s) => s.name,
      cell: (s) => (
        <PrimaryCell
          title={s.name}
          href={scopedHref(discoveryHref(s.id), scope)}
          sub={multiSite(me) ? `${s.site_name} · ${probeWords(s)}` : probeWords(s)}
        />
      ),
    },
    { key: "targets", header: t("discovery.column.targets"), cell: (s) => s.targets.join(", ") },
    { key: "every", header: t("discovery.column.every"), cell: (s) => everyLabel(s.every) },
    {
      key: "last",
      header: t("discovery.column.last"),
      cell: (s) =>
        s.last_run?.finished_at ? <RelativeTime value={s.last_run.finished_at} now={now} /> : <Missing word="never" />,
    },
    {
      key: "found",
      header: t("discovery.column.found"),
      cell: (s) => (s.last_run?.state === "done" ? s.last_run.answered : <Missing word="notYet" />),
    },
  ];

  return (
    <Page>
      <PageHeader
        title={t("discovery.title")}
        meta={<RefreshLine updatedAt={updatedAt} stale={stale} />}
        actions={
          canScan &&
          !creating && (
            <Button variant="k" onClick={() => setCreating(true)}>
              {t("discovery.list.new")}
            </Button>
          )
        }
      />
      <p className={styles.sectionNote}>{t("discovery.list.note")}</p>
      {creating && sites && (
        <ScanForm
          sites={sites}
          onSaved={(scan) => router.push(scopedHref(discoveryHref(scan.id), scope))}
          onCancel={() => setCreating(false)}
        />
      )}
      {stale && updatedAt && (
        <StaleBanner error={describeError(error)} updatedAt={updatedAt} failures={failures} onRetry={refresh} />
      )}
      {error !== undefined && error !== null && !data ? (
        <ErrorState what={t("discovery.list.label")} error={describeError(error)} onRetry={refresh} />
      ) : !data ? (
        <LoadingBlock />
      ) : scans.length === 0 && !creating ? (
        <EmptyState
          title={t("discovery.list.emptyTitle")}
          actions={
            canScan && (
              <Button variant="k" onClick={() => setCreating(true)}>
                {t("discovery.list.new")}
              </Button>
            )
          }
        >
          {t("discovery.list.empty")}
        </EmptyState>
      ) : (
        scans.length > 0 && (
          <DataTable label={t("discovery.list.label")} columns={columns} rows={scans} rowKey={(s) => s.id} />
        )
      )}
    </Page>
  );
}
