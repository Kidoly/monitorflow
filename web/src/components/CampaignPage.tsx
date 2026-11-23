"use client";

import Link from "next/link";
import { useRouter, useSearchParams } from "next/navigation";
import { useCallback, useEffect, useState } from "react";

import { t } from "@/i18n";
import {
  ApiError,
  type CampaignAction,
  type CampaignDetail,
  type CampaignRun,
  type CampaignServer,
  type CampaignWave,
  type Me,
  api,
  describeError,
  isAbortError,
} from "@/lib/api";
import {
  canAct,
  canDelete,
  canRelaunch,
  describeSources,
  describeThreshold,
  describeUpdate,
  majorConfirmText,
  upgradeStepText,
  describeWindow,
  failedServers,
  isFinal,
} from "@/lib/campaigns";
import { formatDateTime } from "@/lib/format";
import { administersCustomer, multiCustomer, readsCustomers, useMe } from "@/lib/me";
import { CAMPAIGNS_PATH, copyCampaignHref, editCampaignHref, serverHref } from "@/lib/routes";
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
    return multiCustomer(me) ? (
      <NoPermission title={t("campaigns.role.customerTitle")}>{t("campaigns.role.customerShort")}</NoPermission>
    ) : (
      <NoPermission title={t("campaigns.role.sitesTitle")}>{t("campaigns.role.sitesShort")}</NoPermission>
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
      <NotFound
        title={t("campaigns.notFound")}
        actions={<ButtonLink href={CAMPAIGNS_PATH}>{t("campaigns.backToCampaigns")}</ButtonLink>}
      >
        {t("campaigns.notFoundText")}
      </NotFound>
    );
  }
  if (!campaign) {
    return error ? <Banner tone="bad">{error}</Banner> : <LoadingBlock label={t("campaigns.loading")} />;
  }
  const { progress } = campaign;
  const facts = [
    ...(multiCustomer(me) ? [{ label: t("campaigns.fact.customer"), value: campaign.customer_name }] : []),
    { label: t("campaigns.fact.update"), value: describeUpdate(campaign.spec, campaign.agent_version, campaign.major_upgrade ?? null) },
    { label: t("campaigns.fact.stopsAt"), value: describeThreshold(campaign.failure_threshold) },
    {
      label: t("campaigns.fact.windows"),
      value: campaign.windows.length === 0 ? t("campaigns.anyTime") : campaign.windows.map(describeWindow).join("; "),
    },
    ...(campaign.sources
      ? [
          {
            label: t("campaigns.fact.snapshots"),
            value: t(campaign.sources.mode === "replace" ? "campaigns.fact.replace" : "campaigns.fact.add", {
              sources: describeSources(campaign.sources),
            }),
          },
        ]
      : []),
    { label: t("campaigns.fact.created"), value: formatDateTime(campaign.created_at) },
    // Start moves a campaign to Scheduled until its window opens: until a batch is queued, it is only requested.
    ...(campaign.started_at
      ? [
          {
            label: campaign.current_wave === null ? t("campaigns.fact.startRequested") : t("campaigns.fact.started"),
            value: formatDateTime(campaign.started_at),
          },
        ]
      : []),
    ...(campaign.finished_at
      ? [{ label: t("campaigns.fact.finished"), value: formatDateTime(campaign.finished_at) }]
      : []),
  ];

  return (
    <div className={own.page}>
      <Breadcrumb parents={[{ label: t("campaigns.label"), href: CAMPAIGNS_PATH }]} />
      <PageHeader
        detail
        title={campaign.name}
        status={<CampaignStateLabel state={campaign.state} />}
        meta={waveText(campaign)}
        actions={
          administersCustomer(me, campaign.customer_id) && <Actions campaign={campaign} onChange={setCampaign} />
        }
      />
      {error && <Banner tone="warn">{t("campaigns.refreshFailed", { error })}</Banner>}
      {campaign.error && <Banner tone="bad" role="status">{campaign.error}</Banner>}
      <FactsBox columns={2} facts={facts} />
      <StatTiles
        compact
        label={t("campaigns.serversByStatus")}
        tiles={[
          { label: t("campaigns.count.succeeded"), value: progress.succeeded, total: progress.total, tone: "ok" },
          { label: t("campaigns.count.failed"), value: progress.failed, tone: progress.failed > 0 ? "bad" : "plain" },
          {
            label: t("campaigns.count.inProgress"),
            value: progress.in_progress,
            tone: progress.in_progress > 0 ? "info" : "plain",
          },
          { label: t("campaigns.count.pending"), value: progress.pending, tone: "plain" },
          ...(progress.cancelled > 0
            ? [{ label: t("campaigns.count.cancelled"), value: progress.cancelled, tone: "plain" as const }]
            : []),
        ]}
      />
      {campaign.waves.map((wave) => (
        <Wave
          key={wave.index}
          wave={wave}
          current={wave.index === (campaign.current_wave ?? 0)}
          several={campaign.waves.length > 1}
        />
      ))}
      {(campaign.previous_runs ?? 0) > 0 && (
        <EarlierRuns key={campaign.previous_runs} id={campaign.id} />
      )}
    </div>
  );
}

/** The runs that ended before the campaign was edited or run again (ADR-0173), newest first. */
function EarlierRuns({ id }: { id: string }) {
  const [runs, setRuns] = useState<CampaignRun[] | null>(null);
  const [error, setError] = useState<string | null>(null);
  useEffect(() => {
    const controller = new AbortController();
    api.campaignRuns(id, controller.signal).then(
      (list) => setRuns(list.runs),
      (e: unknown) => {
        if (!isAbortError(e)) setError(describeError(e));
      },
    );
    return () => controller.abort();
  }, [id]);
  return (
    <section aria-labelledby="campaign-runs-title" className={own.runs}>
      <h2 id="campaign-runs-title" className={own.runsTitle}>
        {t("campaigns.runs.title")}
      </h2>
      {error ? (
        <Banner tone="warn">{t("campaigns.runs.failed", { error })}</Banner>
      ) : !runs ? (
        <LoadingBlock label={t("campaigns.runs.loading")} />
      ) : (
        runs.map((run) => <Run key={run.number} run={run} />)
      )}
    </section>
  );
}

function Run({ run }: { run: CampaignRun }) {
  const title = t("campaigns.runs.run", { number: run.number });
  const { succeeded, total } = run.progress;
  return (
    <details className={own.wave} aria-label={title}>
      <summary>
        <strong>{title}</strong>
        <CampaignStateLabel state={run.state} />
        <span>{t("campaigns.runs.succeeded", { succeeded, total })}</span>
        <span className={own.dim}>
          {run.started_at && t("campaigns.wave.started", { time: formatDateTime(run.started_at) })}
          {t("campaigns.wave.finished", { time: formatDateTime(run.finished_at) })}
        </span>
      </summary>
      <div className={own.runBody}>
        <p className={own.dim}>
          {run.settings.name} · {describeUpdate(run.settings.spec, run.settings.agent_version, run.settings.major_upgrade ?? null)}
        </p>
        {run.error && <p className={own.dim}>{run.error}</p>}
        {run.waves.map((wave) => (
          <Wave key={wave.index} wave={wave} current={false} several={run.waves.length > 1} />
        ))}
      </div>
    </details>
  );
}

const actionLabel = (action: CampaignAction) => t(`campaigns.action.${action}`);

function Actions({ campaign, onChange }: { campaign: CampaignDetail; onChange: (c: CampaignDetail) => void }) {
  const router = useRouter();
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);

  async function act(action: CampaignAction) {
    setBusy(true);
    setError(null);
    try {
      onChange(await api.campaignAction(campaign.id, action));
    } catch (e) {
      setError(t("campaigns.failed", { action: actionLabel(action), error: describeError(e) }));
    } finally {
      setBusy(false);
    }
  }

  return (
    <div className={own.actions}>
      {/* A major upgrade starts after a confirmation naming the releases and the servers (ADR-0182). */}
      {campaign.major_upgrade && canAct("start", campaign.state) && (
        <InlineConfirm
          trigger={actionLabel("start")}
          title={majorConfirmText(campaign.major_upgrade, campaign.progress.total)}
          consequence={t("campaigns.major.confirmNote")}
          confirmLabel={actionLabel("start")}
          triggerVariant="k"
          confirmVariant="k"
          onConfirm={async () => onChange(await api.campaignAction(campaign.id, "start"))}
          describeError={(e) => t("campaigns.failed", { action: actionLabel("start"), error: describeError(e) })}
        />
      )}
      {(["start", "pause", "resume"] as const)
        .filter((action) => canAct(action, campaign.state) && !(action === "start" && campaign.major_upgrade))
        .map((action) => (
          <Button
            key={action}
            variant={action === "start" ? "k" : "default"}
            disabled={busy}
            onClick={() => void act(action)}
          >
            {actionLabel(action)}
          </Button>
        ))}
      {/* Always offered: the form says what can change now (ADR-0173). */}
      <ButtonLink href={editCampaignHref(campaign.id)}>{t("campaigns.action.edit")}</ButtonLink>
      <ButtonLink href={copyCampaignHref(campaign.id)}>{t("campaigns.action.duplicate")}</ButtonLink>
      {canRelaunch(campaign) && failedServers(campaign) > 0 && (
        <InlineConfirm
          trigger={t("campaigns.relaunchFailed.trigger", { count: failedServers(campaign) })}
          title={t("campaigns.relaunchFailed.title", { name: campaign.name })}
          consequence={t("campaigns.relaunchFailed.consequence")}
          confirmLabel={t("campaigns.relaunchFailed.confirm")}
          triggerVariant="default"
          confirmVariant="k"
          pendingLabel={t("campaigns.relaunch.pending")}
          onConfirm={async () => onChange(await api.relaunchCampaign(campaign.id, "failed"))}
          describeError={(e) => t("campaigns.relaunch.failed", { error: describeError(e) })}
        />
      )}
      {canRelaunch(campaign) && (
        <InlineConfirm
          trigger={t("campaigns.relaunch.trigger")}
          title={t("campaigns.relaunch.title", { name: campaign.name })}
          consequence={t("campaigns.relaunch.consequence")}
          confirmLabel={t("campaigns.relaunch.confirm")}
          triggerVariant="default"
          confirmVariant="k"
          pendingLabel={t("campaigns.relaunch.pending")}
          onConfirm={async () => onChange(await api.relaunchCampaign(campaign.id, "all"))}
          describeError={(e) => t("campaigns.relaunch.failed", { error: describeError(e) })}
        />
      )}
      {/* A draft is deleted rather than stopped. */}
      {canAct("stop", campaign.state) && campaign.state !== "draft" && (
        <InlineConfirm
          trigger={t("campaigns.stop.trigger")}
          title={t("campaigns.stop.title", { name: campaign.name })}
          consequence={t("campaigns.stop.consequence")}
          confirmLabel={t("campaigns.stop.confirm")}
          pendingLabel={t("campaigns.stop.pending")}
          onConfirm={async () => onChange(await api.campaignAction(campaign.id, "stop"))}
          describeError={(e) => t("campaigns.stop.failed", { error: describeError(e) })}
        />
      )}
      {canDelete(campaign) && (
        <InlineConfirm
          trigger={t("campaigns.delete.trigger")}
          title={t("campaigns.delete.title", { name: campaign.name })}
          consequence={t("campaigns.delete.consequence")}
          confirmLabel={t("campaigns.delete.confirm")}
          pendingLabel={t("campaigns.delete.pending")}
          onConfirm={async () => {
            await api.deleteCampaign(campaign.id);
            router.push(CAMPAIGNS_PATH);
          }}
          describeError={(e) => t("campaigns.delete.failed", { error: describeError(e) })}
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

const serverColumns = (): Column<CampaignServer>[] => [
  {
    key: "server",
    header: t("campaigns.column.server"),
    cell: (s) => (
      <Link href={serverHref(s.server_id)}>
        <b>{s.hostname}</b>
      </Link>
    ),
  },
  {
    key: "status",
    header: t("campaigns.column.state"),
    cell: (s) => {
      const step = upgradeStepText(s);
      return (
        <>
          <ServerStatusLabel status={s.status} />
          {step && <span className={own.dim}> {step}</span>}
        </>
      );
    },
  },
  { key: "exit", header: t("campaigns.column.exitCode"), cell: (s) => s.exit_code },
  {
    key: "finished",
    header: t("campaigns.column.finished"),
    // Empty until it is: the status column already says it is not done.
    cell: (s) => (s.finished_at ? formatDateTime(s.finished_at) : null),
  },
  { key: "error", header: t("campaigns.column.error"), className: own.wrap, cell: (s) => s.error ?? "" },
];

const WAVE_TONES = { pending: "neutral", running: "info", done: "ok" } as const;

/**
 * A batch as a `<details>` block: a `<progress>` with its percentage, when
 * it ran, then its servers. The counters above tell the numbers; the
 * batch's state shows only when there are several.
 */
function Wave({ wave, current, several }: { wave: CampaignWave; current: boolean; several: boolean }) {
  const number = wave.index + 1;
  const title = t("campaigns.wave.title", { number });
  const { total, succeeded, failed, cancelled } = wave.progress;
  const done = succeeded + failed + cancelled;
  const percent = total ? Math.round((done * 100) / total) : 0;

  return (
    <section aria-label={title}>
      <details className={own.wave} open={current || wave.state === "running"}>
        <summary>
          <strong>{title}</strong>
          {several && <Label tone={WAVE_TONES[wave.state]}>{t(`campaigns.wave.${wave.state}`)}</Label>}
          <progress max={total} value={done} aria-label={t("campaigns.wave.progress", { number })} />
          <span>{percent} %</span>
          {wave.started_at && (
            <span className={own.dim}>
              {t("campaigns.wave.started", { time: formatDateTime(wave.started_at) })}
              {wave.finished_at && t("campaigns.wave.finished", { time: formatDateTime(wave.finished_at) })}
            </span>
          )}
        </summary>
        <DataTable
          dense
          label={t("campaigns.wave.servers", { number })}
          columns={serverColumns()}
          rows={wave.servers}
          rowKey={(s) => s.server_id}
        />
      </details>
    </section>
  );
}
