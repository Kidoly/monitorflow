"use client";

import Link from "next/link";
import { useSearchParams } from "next/navigation";
import { useCallback, useEffect, useState } from "react";

import {
  ApiError,
  type CampaignAction,
  type CampaignDetail,
  type CampaignServer,
  type CampaignWave,
  type Me,
  api,
  describeError,
  isAbortError,
} from "@/lib/api";
import { canAct, describeSources, describeThreshold, describeUpdate, describeWindow, isFinal } from "@/lib/campaigns";
import { formatDateTime } from "@/lib/format";
import { administersCustomer, readsCustomers, useMe } from "@/lib/me";
import { CAMPAIGNS_PATH, serverHref } from "@/lib/routes";
import { usePolling } from "@/lib/usePolling";
import { UUID } from "@/lib/uuid";

import { CampaignStateLabel, ServerStatusLabel, waveText } from "./CampaignList";
import own from "./Campaigns.module.css";
import { REFRESH_MS } from "./FleetOverview";
import { Breadcrumb } from "./ui/Breadcrumb";
import { Button, ButtonLink } from "./ui/Button";
import { type Column, DataTable } from "./ui/DataTable";
import { FactsBox } from "./ui/FactsBox";
import { InlineConfirm } from "./ui/InlineConfirm";
import { Banner, LoadingBlock, NoPermission, NotFound, PageHeader } from "./ui/PageState";
import { StatTiles } from "./ui/StatTiles";
import { Label } from "./ui/StatusLabel";

/** `/campaigns/view/?id=…` (ADR-0020 routing). */
export function CampaignView() {
  const id = useSearchParams().get("id");
  const me = useMe();
  if (!me || !readsCustomers(me)) {
    return (
      <NoPermission title="Campaigns need a customer role">
        Campaigns need a role on a whole customer. Ask an administrator for one.
      </NoPermission>
    );
  }
  return <CampaignPage key={id} id={id} me={me} />;
}

/** Nothing changes any more: a draft, or a final campaign without commands in flight. */
function settled(campaign: CampaignDetail | null | undefined): boolean {
  if (!campaign) return campaign === null;
  return campaign.state === "draft" || (isFinal(campaign.state) && campaign.progress.in_progress === 0);
}

export function CampaignPage({ id, me }: { id: string | null; me: Me }) {
  const valid = id !== null && UUID.test(id);
  /** Undefined until loaded, null when not found or out of scope. */
  const [campaign, setCampaign] = useState<CampaignDetail | null | undefined>(undefined);
  const [error, setError] = useState<string | null>(null);
  const load = useCallback(
    async (signal?: AbortSignal) => {
      try {
        setCampaign(await api.campaign(id ?? "", signal));
        setError(null);
      } catch (e) {
        if (isAbortError(e)) return;
        if (e instanceof ApiError && e.status === 404) setCampaign(null);
        else setError(describeError(e));
      }
    },
    [id],
  );
  // Polls while the runner or agents may still change something; an
  // action's answer restarts it.
  usePolling(load, REFRESH_MS, valid && !settled(campaign));
  useEffect(() => {
    if (campaign) document.title = `${campaign.name} · MonitorFlow`;
  }, [campaign]);

  if (!valid || campaign === null) {
    return (
      <NotFound title="Campaign not found" actions={<ButtonLink href={CAMPAIGNS_PATH}>Back to campaigns</ButtonLink>}>
        It may not exist, or your roles do not cover its customer.
      </NotFound>
    );
  }
  if (!campaign) {
    return error ? <Banner tone="bad">{error}</Banner> : <LoadingBlock label="Loading campaign" />;
  }
  const { progress } = campaign;
  const facts = [
    { label: "Customer", value: campaign.customer_name },
    { label: "Update", value: describeUpdate(campaign.spec) },
    { label: "Stops at", value: describeThreshold(campaign.failure_threshold) },
    {
      label: "Windows",
      value: campaign.windows.length === 0 ? "any time" : campaign.windows.map(describeWindow).join("; "),
    },
    ...(campaign.sources
      ? [
          {
            label: "Snapshots",
            value: `${describeSources(campaign.sources)} (${campaign.sources.mode === "replace" ? "other sources set aside" : "other sources kept"})`,
          },
        ]
      : []),
    { label: "Created", value: formatDateTime(campaign.created_at) },
    ...(campaign.started_at ? [{ label: "Started", value: formatDateTime(campaign.started_at) }] : []),
    ...(campaign.finished_at ? [{ label: "Finished", value: formatDateTime(campaign.finished_at) }] : []),
  ];

  return (
    <div className={own.page}>
      <Breadcrumb parents={[{ label: "Campaigns", href: CAMPAIGNS_PATH }]} current={campaign.name} />
      <PageHeader
        detail
        title={campaign.name}
        status={<CampaignStateLabel state={campaign.state} />}
        meta={waveText(campaign)}
        actions={
          administersCustomer(me, campaign.customer_id) && <Actions campaign={campaign} onChange={setCampaign} />
        }
      />
      {error && <Banner tone="warn">Refresh failed: {error} Showing the last data received.</Banner>}
      {campaign.error && <Banner tone="bad" role="status">{campaign.error}</Banner>}
      <FactsBox columns={2} facts={facts} />
      <StatTiles
        compact
        label="Servers by status"
        tiles={[
          { label: "Succeeded", value: progress.succeeded, total: progress.total, tone: "ok" },
          { label: "Failed", value: progress.failed, tone: progress.failed > 0 ? "bad" : "plain" },
          { label: "In progress", value: progress.in_progress, tone: progress.in_progress > 0 ? "info" : "plain" },
          { label: "Pending", value: progress.pending, tone: "plain" },
          ...(progress.cancelled > 0
            ? [{ label: "Cancelled", value: progress.cancelled, tone: "plain" as const }]
            : []),
        ]}
      />
      {campaign.waves.map((wave) => (
        <Wave key={wave.index} wave={wave} current={wave.index === (campaign.current_wave ?? 0)} />
      ))}
    </div>
  );
}

const LABELS: Record<CampaignAction, string> = { start: "Start", pause: "Pause", resume: "Resume", stop: "Stop" };

function Actions({ campaign, onChange }: { campaign: CampaignDetail; onChange: (c: CampaignDetail) => void }) {
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);

  async function act(action: CampaignAction) {
    setBusy(true);
    setError(null);
    try {
      onChange(await api.campaignAction(campaign.id, action));
    } catch (e) {
      setError(`${LABELS[action]} failed. ${describeError(e)}`);
    } finally {
      setBusy(false);
    }
  }

  return (
    <div className={own.actions}>
      {(["start", "pause", "resume"] as const)
        .filter((action) => canAct(action, campaign.state))
        .map((action) => (
          <Button
            key={action}
            variant={action === "start" ? "k" : "default"}
            disabled={busy}
            onClick={() => void act(action)}
          >
            {LABELS[action]}
          </Button>
        ))}
      {canAct("stop", campaign.state) && (
        <InlineConfirm
          trigger="Stop…"
          title={`Stop ${campaign.name}?`}
          consequence="Stopping is final. Commands not sent yet are cancelled; those already on their servers finish and are counted."
          confirmLabel="Stop the campaign"
          pendingLabel="Stopping…"
          onConfirm={async () => onChange(await api.campaignAction(campaign.id, "stop"))}
          describeError={(e) => `Stop failed. ${describeError(e)}`}
        />
      )}
      {error && (
        <p className={own.error} role="alert">
          {error}
        </p>
      )}
    </div>
  );
}

const SERVER_COLUMNS: Column<CampaignServer>[] = [
  {
    key: "server",
    header: "Server",
    cell: (s) => (
      <Link href={serverHref(s.server_id)}>
        <b>{s.hostname}</b>
      </Link>
    ),
  },
  { key: "status", header: "State", cell: (s) => <ServerStatusLabel status={s.status} /> },
  { key: "exit", header: "Exit code", cell: (s) => s.exit_code ?? <span className={own.dim}>—</span> },
  {
    key: "finished",
    header: "Finished",
    cell: (s) => (s.finished_at ? formatDateTime(s.finished_at) : <span className={own.dim}>—</span>),
  },
  { key: "error", header: "Error", className: own.wrap, cell: (s) => s.error ?? "" },
];

const WAVE_STATES = { pending: ["Pending", "neutral"], running: ["Running", "info"], done: ["Done", "ok"] } as const;

/** A wave as a `<details>` block: state, a `<progress>` with its percentage, then its servers. */
function Wave({ wave, current }: { wave: CampaignWave; current: boolean }) {
  const title = `Wave ${wave.index + 1}`;
  const { total, succeeded, failed, cancelled } = wave.progress;
  const done = succeeded + failed + cancelled;
  const percent = total ? Math.round((done * 100) / total) : 0;
  const [word, tone] = WAVE_STATES[wave.state];
  return (
    <section aria-label={title}>
      <details className={own.wave} open={current || wave.state === "running"}>
        <summary>
          <strong>{title}</strong>
          <Label tone={tone}>{word}</Label>
          <progress max={total} value={done} aria-label={`${title} progress`} />
          <span>{percent} %</span>
          <span className={own.dim}>
            {succeeded} succeeded{failed > 0 && `, ${failed} failed`} of {total}
            {wave.started_at ? ` · started ${formatDateTime(wave.started_at)}` : " · not started"}
            {wave.finished_at && `, finished ${formatDateTime(wave.finished_at)}`}
          </span>
        </summary>
        <DataTable
          dense
          label={`Servers of ${title.toLowerCase()}`}
          columns={SERVER_COLUMNS}
          rows={wave.servers}
          rowKey={(s) => s.server_id}
        />
      </details>
    </section>
  );
}
