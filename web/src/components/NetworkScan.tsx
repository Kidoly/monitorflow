"use client";

import { useRouter, useSearchParams } from "next/navigation";
import { useCallback, useState } from "react";

import { rich, t } from "@/i18n";
import { type NetworkScanAddResult, type NetworkScanHost, api, describeError } from "@/lib/api";
import { active, checksWords, failureText, probeWords, sections } from "@/lib/discovery";
import { DISCOVERY_PATH, TOKENS_PATH, serverHref } from "@/lib/routes";
import { scopedHref, useScope } from "@/lib/scope";
import { usePolledData } from "@/lib/usePolledData";
import { Breadcrumb } from "@/components/ui/Breadcrumb";
import { Button, ButtonLink, fieldClass } from "@/components/ui/Button";
import { type Column, DataTable, PrimaryCell } from "@/components/ui/DataTable";
import { InlineConfirm } from "@/components/ui/InlineConfirm";
import { Page } from "@/components/ui/Page";
import { Banner, ErrorState, LoadingBlock, NotFound, PageHeader, RefreshLine } from "@/components/ui/PageState";
import { RelativeTime } from "@/components/ui/RelativeTime";
import { Label } from "@/components/ui/StatusLabel";

import styles from "./AdminPages.module.css";
import { ScanForm, useScannableSites } from "./NetworkScans";

const TEMPLATES = ["switch", "router", "firewall", "printer", "ups", "nas", "generic"] as const;

/** What answered, in words: "Answers ping · Open ports 22, 80 · SNMP: UPS card". */
export function answeredWords(host: NetworkScanHost): string {
  const parts = [host.ping ? t("discovery.pingAnswers") : t("discovery.noPing")];
  if (host.ports.length > 0) parts.push(t("discovery.openPorts", { ports: host.ports.join(", ") }));
  if (host.snmp) parts.push(t("discovery.snmpSays", { description: host.snmp_description ?? host.snmp_name ?? "" }));
  return parts.join(" · ");
}

/** The proposal of `host` in one line. */
export function proposalWords(host: NetworkScanHost): string {
  const p = host.proposal;
  switch (p.action) {
    case "agent":
      return t(p.os === "windows" ? "discovery.proposal.agent.windows" : "discovery.proposal.agent.linux");
    case "snmp_device":
      return t("discovery.proposal.snmp_device", {
        template: t(`devices.template.${(p.template ?? "generic") as "switch"}`),
      });
    case "monitored":
      return t("discovery.proposal.monitored", {
        name: p.monitored_by?.name ?? "",
        how: t(`discovery.matched.${p.monitored_by?.matched_by ?? "address"}`),
      });
    default:
      return t("discovery.proposal.device");
  }
}

/** Words for what `add` did. */
export function addedWords(result: NetworkScanAddResult): string[] {
  const lines = result.added.length > 0 ? [t("discovery.added", { count: result.added.length })] : [];
  for (const s of result.skipped) lines.push(t(`discovery.skipped.${s.reason}`, { address: s.address }));
  return lines;
}

/** Fleet › Network scans › one scan (ADR-0154). */
export function NetworkScanView() {
  const id = useSearchParams().get("id") ?? "";
  const scope = useScope();
  const router = useRouter();
  const sites = useScannableSites();
  const [editing, setEditing] = useState(false);
  const [busy, setBusy] = useState<string | null>(null);
  const [message, setMessage] = useState<{ tone: "ok" | "bad"; lines: string[] } | null>(null);
  const [templates, setTemplates] = useState<Record<string, string>>({});
  const [showDismissed, setShowDismissed] = useState(false);
  const fetcher = useCallback(
    (signal: AbortSignal) =>
      Promise.all([api.networkScan(id, signal), api.networkScanResults(id, signal)]).then(([scan, results]) => ({
        scan,
        results,
      })),
    [id],
  );
  const [fast, setFast] = useState(false);
  const { data, error, stale, updatedAt, refresh } = usePolledData(fetcher, {
    enabled: id !== "",
    intervalMs: fast ? 3_000 : 15_000,
  });
  const running = active(data?.results.current);
  if (running !== fast) setFast(running);

  if (!data) {
    if (error && describeError(error).toLowerCase().includes("not found")) {
      return (
        <Page>
          <NotFound title={t("discovery.view.notFound")} />
        </Page>
      );
    }
    return (
      <Page>
        {error ? (
          <ErrorState what={t("discovery.title")} error={describeError(error)} onRetry={refresh} />
        ) : (
          <LoadingBlock />
        )}
      </Page>
    );
  }
  const { scan, results } = data;
  const canModify = sites?.some((s) => s.id === scan.site_id) ?? false;
  const now = new Date();
  const { proposed, monitored, dismissed } = sections(results.hosts);
  const addable = proposed.filter((h) => h.proposal.action !== "agent");

  async function act(key: string, action: () => Promise<string[] | void>) {
    setBusy(key);
    setMessage(null);
    try {
      const lines = await action();
      if (lines) setMessage({ tone: "ok", lines });
      refresh();
    } catch (e) {
      setMessage({ tone: "bad", lines: [t("discovery.actionFailed", { error: describeError(e) })] });
    } finally {
      setBusy(null);
    }
  }
  const add = (hosts: NetworkScanHost[], all = false) =>
    act(all ? "all" : (hosts[0]?.address ?? ""), async () =>
      addedWords(
        await api.addFromNetworkScan(
          scan.id,
          all
            ? { all: true }
            : {
                devices: hosts.map((h) => ({
                  address: h.address,
                  ...(templates[h.address] && { template: templates[h.address] }),
                })),
              },
        ),
      ),
    );

  const current = results.current;
  const last = scan.last_run;
  const status = current ? (
    <Banner tone="info" role="status">
      {current.state === "running"
        ? t("discovery.view.scanning", { scanned: current.scanned, addresses: current.addresses })
        : scan.probe === "relay"
          ? t("discovery.view.queuedRelay", { name: scan.probe_key })
          : t("discovery.view.queued")}
    </Banner>
  ) : last?.state === "failed" ? (
    <Banner tone="bad">{failureText(last)}</Banner>
  ) : null;
  const run = results.run;

  const columns: Column<NetworkScanHost>[] = [
    {
      key: "address",
      header: t("discovery.column.address"),
      cell: (h) => (
        <PrimaryCell
          title={
            <>
              {h.address}{" "}
              {h.new && (
                <Label tone="info" small>
                  {t("discovery.newBadge")}
                </Label>
              )}
            </>
          }
          sub={[h.name, h.mac].filter(Boolean).join(" · ") || undefined}
        />
      ),
    },
    { key: "what", header: t("discovery.column.what"), cell: (h) => answeredWords(h) },
    {
      key: "proposal",
      header: t("discovery.column.proposal"),
      cell: (h) => (
        <>
          {h.proposal.monitored_by ? (
            <a href={scopedHref(serverHref(h.proposal.monitored_by.server_id), scope)}>{proposalWords(h)}</a>
          ) : (
            proposalWords(h)
          )}
          {h.proposal.checks.length > 0 && h.proposal.action !== "monitored" && (
            <span className={styles.sectionNote}>
              <br />
              {t("discovery.checks", { checks: checksWords(h) })}
            </span>
          )}
          {h.proposal.action === "snmp_device" && canModify && (
            <label className={styles.field}>
              {t("discovery.template")}
              <select
                className={fieldClass}
                value={templates[h.address] ?? h.proposal.template ?? "generic"}
                onChange={(e) => setTemplates({ ...templates, [h.address]: e.target.value })}
              >
                {TEMPLATES.map((key) => (
                  <option key={key} value={key}>
                    {t(`devices.template.${key}`)}
                  </option>
                ))}
              </select>
            </label>
          )}
        </>
      ),
    },
  ];
  const actions = (h: NetworkScanHost) =>
    canModify && (
      <div className={styles.buttons}>
        {h.proposal.action === "agent" && (
          <ButtonLink variant="k" size="small" href={scopedHref(TOKENS_PATH, scope)}>
            {t("discovery.install")}
          </ButtonLink>
        )}
        {h.proposal.action !== "dismissed" && (
          <Button
            size="small"
            variant={h.proposal.action === "agent" ? "default" : "k"}
            disabled={busy !== null}
            aria-label={`${h.proposal.action === "agent" ? t("discovery.addAsDevice") : t("discovery.add")} ${h.address}`}
            onClick={() => void add([h])}
          >
            {busy === h.address ? t("discovery.adding") : h.proposal.action === "agent" ? t("discovery.addAsDevice") : t("discovery.add")}
          </Button>
        )}
        <Button
          size="small"
          variant="ghost"
          disabled={busy !== null}
          aria-label={`${h.proposal.action === "dismissed" ? t("discovery.restore") : t("discovery.dismiss")} ${h.address}`}
          onClick={() =>
            void act(`d${h.address}`, () =>
              h.proposal.action === "dismissed"
                ? api.restoreScanAddress(scan.id, h.address)
                : api.dismissScanAddress(scan.id, h.address),
            )
          }
        >
          {h.proposal.action === "dismissed" ? t("discovery.restore") : t("discovery.dismiss")}
        </Button>
      </div>
    );

  return (
    <Page>
      <Breadcrumb parents={[{ label: t("discovery.title"), href: scopedHref(DISCOVERY_PATH, scope) }]} />
      <PageHeader
        detail
        title={scan.name}
        meta={<RefreshLine updatedAt={updatedAt} stale={stale} />}
        sub={
          <>
            {scan.targets.join(", ")} · {t("discovery.view.from", { probe: probeWords(scan) })}
          </>
        }
        actions={
          canModify && (
            <>
              <Button
                variant="k"
                disabled={current !== null || busy !== null}
                onClick={() => void act("run", async () => void (await api.runNetworkScan(scan.id)))}
              >
                {t("discovery.view.scanNow")}
              </Button>
              <Button onClick={() => setEditing(!editing)}>{t("discovery.view.edit")}</Button>
            </>
          )
        }
      />
      {editing && sites && (
        <ScanForm
          scan={scan}
          sites={sites}
          onSaved={() => {
            setEditing(false);
            refresh();
          }}
          onCancel={() => setEditing(false)}
        />
      )}
      {status}
      {message && (
        <Banner tone={message.tone} role="status">
          {message.lines.map((line) => (
            <div key={line}>{line}</div>
          ))}
        </Banner>
      )}
      {!run ? (
        !current && <p className={styles.sectionNote}>{t("discovery.view.never")}</p>
      ) : (
        <>
          <p className={styles.sectionNote}>
            {run.finished_at && (
              <>
                {rich("discovery.view.lastRun", { when: <RelativeTime value={run.finished_at} now={now} /> })}{" "}
              </>
            )}
            {t("discovery.view.answered", { count: run.answered })}{" "}
            {run.compared && t("discovery.view.changes", { new: run.new, gone: run.gone.length })}
          </p>
          {run.gone.length > 0 && (
            <p className={styles.sectionNote}>{t("discovery.view.gone", { addresses: run.gone.join(", ") })}</p>
          )}
          {run.ping_error && (
            <Banner tone="warn">{t("discovery.view.pingError", { probe: probeWords(scan), error: run.ping_error })}</Banner>
          )}
          {run.dropped > 0 && <p className={styles.sectionNote}>{t("discovery.view.dropped", { count: run.dropped })}</p>}
          <section className={styles.section} aria-label={t("discovery.section.proposed")}>
            <div className={styles.sectionHead}>
              <h2 className={styles.sectionTitle}>{t("discovery.section.proposed")}</h2>
              {canModify && addable.length > 0 && (
                <Button variant="k" disabled={busy !== null} onClick={() => void add(addable, true)}>
                  {busy === "all" ? t("discovery.adding") : t("discovery.addAll", { count: addable.length })}
                </Button>
              )}
            </div>
            {proposed.length === 0 ? (
              <p className={styles.sectionNote}>{t("discovery.section.nothing")}</p>
            ) : (
              <DataTable
                label={t("discovery.section.proposed")}
                columns={columns}
                rows={proposed}
                rowKey={(h) => h.address}
                rowActions={actions}
              />
            )}
          </section>
          {monitored.length > 0 && (
            <section className={styles.section} aria-label={t("discovery.section.monitored")}>
              <h2 className={styles.sectionTitle}>{t("discovery.section.monitored")}</h2>
              <DataTable
                label={t("discovery.section.monitored")}
                columns={columns}
                rows={monitored}
                rowKey={(h) => h.address}
              />
            </section>
          )}
          {dismissed.length > 0 && (
            <section className={styles.section} aria-label={t("discovery.section.dismissed")}>
              <Button variant="ghost" onClick={() => setShowDismissed(!showDismissed)}>
                {showDismissed ? t("discovery.hideDismissed") : t("discovery.showDismissed", { count: dismissed.length })}
              </Button>
              {showDismissed && (
                <DataTable
                  label={t("discovery.section.dismissed")}
                  columns={columns}
                  rows={dismissed}
                  rowKey={(h) => h.address}
                  rowActions={actions}
                  rowMuted={() => true}
                />
              )}
            </section>
          )}
        </>
      )}
      {canModify && (
        <InlineConfirm
          trigger={t("discovery.view.delete")}
          title={t("discovery.view.deleteConfirm", { name: scan.name })}
          consequence=""
          confirmLabel={t("discovery.view.delete")}
          onConfirm={async () => {
            await api.deleteNetworkScan(scan.id);
            router.push(scopedHref(DISCOVERY_PATH, scope));
          }}
        />
      )}
    </Page>
  );
}
