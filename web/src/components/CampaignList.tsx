"use client";

import { usePathname, useSearchParams } from "next/navigation";
import { useCallback, useState } from "react";

import {
  type Campaign,
  type CampaignAction,
  type CampaignDetail,
  type CampaignServer,
  type CampaignServerStatus,
  type CampaignState,
  type Site,
  api,
  describeError,
} from "@/lib/api";
import { CAMPAIGN_STATES, SERVER_STATUSES, campaignSubline, canAct, isFinal, shortWindow } from "@/lib/campaigns";
import { administersCustomer, readsCustomers, useMe } from "@/lib/me";
import { shortTime } from "@/lib/mirrors";
import { NEW_CAMPAIGN_PATH, campaignHref, serverHref } from "@/lib/routes";
import { useScope } from "@/lib/scope";
import { usePolledData } from "@/lib/usePolledData";

import own from "./Campaigns.module.css";
import { Button, ButtonLink } from "./ui/Button";
import { type Column, DataTable, PrimaryCell, useUrlSort } from "./ui/DataTable";
import { FilterBar } from "./ui/FilterBar";
import { Banner, EmptyState, ErrorState, LoadingBlock, NoPermission, PageHeader, RefreshLine, StaleBanner } from "./ui/PageState";
import { StatTiles } from "./ui/StatTiles";
import { Label } from "./ui/StatusLabel";

const DAY_MS = 86_400_000;

/** "Wave 2 of 3", or how many waves before the first is queued. */
export function waveText(campaign: Pick<Campaign, "current_wave" | "wave_count">): string {
  const waves = `${campaign.wave_count} wave${campaign.wave_count === 1 ? "" : "s"}`;
  return campaign.current_wave === null ? waves : `Wave ${campaign.current_wave + 1} of ${campaign.wave_count}`;
}

/** A campaign state as a tinted word. */
export function CampaignStateLabel({ state }: { state: CampaignState }) {
  const [word, tone] = CAMPAIGN_STATES[state];
  return <Label tone={tone}>{word}</Label>;
}

/** A server's status in a campaign as a tinted word. */
export function ServerStatusLabel({ status }: { status: CampaignServerStatus }) {
  const [word, tone] = SERVER_STATUSES[status];
  return <Label tone={tone}>{word}</Label>;
}

/** "3 / 4" in bold, then what is not succeeded: "· 1 failed · 1 in progress". */
export function ProgressText({ campaign }: { campaign: Pick<Campaign, "progress" | "state"> }) {
  const { total, succeeded, failed, in_progress, cancelled } = campaign.progress;
  const rest = [
    failed > 0 && `${failed} failed`,
    in_progress > 0 && `${in_progress} in progress`,
    cancelled > 0 && `${cancelled} cancelled`,
  ].filter(Boolean);
  const count = `${succeeded} / ${total}`;
  if (campaign.state === "draft" || campaign.state === "scheduled") return <span className={own.dim}>{count}</span>;
  return (
    <>
      <strong>{count}</strong>
      {rest.length > 0 && <span className={own.dim}> · {rest.join(" · ")}</span>}
    </>
  );
}

/** Running, paused, scheduled, failed, draft, then the finished; newest first within each. */
const RANK: Record<CampaignState, number> = {
  running: 0,
  paused: 1,
  scheduled: 2,
  failed: 3,
  draft: 4,
  stopped: 5,
  completed: 6,
};
const byRank = (a: Campaign, b: Campaign) => RANK[a.state] - RANK[b.state] || b.created_at.localeCompare(a.created_at);

const ROW_ACTIONS: CampaignAction[] = ["start", "pause", "resume"];
const ACTION_LABELS: Record<CampaignAction, string> = { start: "Start", pause: "Pause", resume: "Resume", stop: "Stop" };

interface Listing {
  campaigns: Campaign[];
  more: boolean;
  sites: Site[];
  /** The newest running campaign with its servers. */
  running: CampaignDetail | null;
}

/** One fetch for the page's single timer: campaigns, site names and the running campaign's servers. */
async function fetchListing(signal: AbortSignal): Promise<Listing> {
  const [list, sites] = await Promise.all([api.campaigns(signal), api.sites(signal)]);
  const running = list.campaigns.find((c) => c.state === "running");
  return {
    campaigns: list.campaigns,
    more: list.next_cursor !== null,
    sites: sites.sites,
    running: running ? await api.campaign(running.id, signal) : null,
  };
}

/** `/campaigns/`: counters, the campaigns table and the servers of the running campaign. */
export function CampaignListPage() {
  const me = useMe();
  if (!readsCustomers(me)) {
    return (
      <NoPermission title="Campaigns need a customer role">
        Campaigns span sites: they need a role on a whole customer. Ask an administrator for one.
      </NoPermission>
    );
  }
  return <CampaignList />;
}

function CampaignList() {
  const me = useMe();
  const { data, error, stale, failures, updatedAt, refresh } = usePolledData(fetchListing);
  const [actionError, setActionError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  const params = useSearchParams();
  const pathname = usePathname();
  const scope = useScope();
  const [sort, setSort] = useUrlSort();

  const act = useCallback(
    async (campaign: Campaign, action: CampaignAction) => {
      setBusy(true);
      setActionError(null);
      try {
        await api.campaignAction(campaign.id, action);
        refresh();
      } catch (e) {
        setActionError(`${ACTION_LABELS[action]} ${campaign.name} failed. ${describeError(e)}`);
      } finally {
        setBusy(false);
      }
    },
    [refresh],
  );

  const header = (
    <PageHeader
      title="Update campaigns"
      meta={updatedAt !== null || error !== null ? <RefreshLine updatedAt={updatedAt} stale={stale} /> : undefined}
      actions={
        administersCustomer(me) && (
          <ButtonLink variant="k" href={NEW_CAMPAIGN_PATH}>
            New campaign
          </ButtonLink>
        )
      }
    />
  );
  if (!data || !updatedAt) {
    return (
      <div className={own.page}>
        {header}
        {error ? <ErrorState what="campaigns" error={describeError(error)} onRetry={refresh} /> : <LoadingBlock />}
      </div>
    );
  }

  const now = updatedAt;
  const inScope = data.campaigns.filter((c) => !scope.customerId || c.customer_id === scope.customerId);
  const state = params.get("state") as CampaignState | null;
  const search = (params.get("q") ?? "").trim().toLowerCase();
  const rows = inScope.filter(
    (c) => (!state || c.state === state) && (!search || c.name.toLowerCase().includes(search)),
  );
  const customers = new Set(inScope.map((c) => c.customer_id));
  const siteName = (id: string) => data.sites.find((s) => s.id === id)?.name ?? "a site";
  const tile = (label: string, value: CampaignState, tone: "info" | "neutral" | "bad" | "ok", count: number) => {
    const next = new URLSearchParams(params);
    if (state === value) next.delete("state");
    else next.set("state", value);
    const query = next.toString();
    return { label, value: count, tone, href: query ? `${pathname}?${query}` : pathname, active: state === value };
  };
  const count = (s: CampaignState) => inScope.filter((c) => c.state === s).length;
  const recent = inScope.filter(
    (c) => c.state === "completed" && c.finished_at && now.getTime() - new Date(c.finished_at).getTime() <= 30 * DAY_MS,
  ).length;
  const running = data.running && inScope.some((c) => c.id === data.running!.id) ? data.running : null;

  const columns: Column<Campaign>[] = [
    {
      key: "name",
      header: "Campaign",
      className: own.wide,
      sortValue: (c) => c.name,
      cell: (c) => (
        <PrimaryCell
          title={c.name}
          href={campaignHref(c.id)}
          sub={customers.size > 1 ? `${c.customer_name} · ${campaignSubline(c)}` : campaignSubline(c)}
        />
      ),
    },
    {
      key: "state",
      header: "State",
      className: own.wrap,
      sortValue: (c) => RANK[c.state],
      cell: (c) => (
        <>
          <CampaignStateLabel state={c.state} />
          {(c.state === "running" || c.state === "paused") && c.current_wave !== null && (
            <span className={own.dim}> {waveText(c).toLowerCase()}</span>
          )}
          {c.state === "failed" && c.error && <span className={own.dim}> {c.error}</span>}
        </>
      ),
    },
    {
      key: "target",
      header: "Target",
      cell: (c) => (c.site_id ? `${siteName(c.site_id)} (${c.progress.total})` : `${c.progress.total} servers`),
    },
    { key: "progress", header: "Progress", className: own.wrap, cell: (c) => <ProgressText campaign={c} /> },
    {
      key: "window",
      header: "Window",
      className: `${own.dim} ${own.wrap}`,
      cell: (c) =>
        c.windows.length === 0
          ? "any time"
          : `${shortWindow(c.windows[0]!)}${c.windows.length > 1 ? ` +${c.windows.length - 1}` : ""}`,
    },
  ];

  return (
    <div className={own.page}>
      {header}
      {stale && <StaleBanner error={describeError(error)} updatedAt={updatedAt} failures={failures} onRetry={refresh} />}
      {actionError && <Banner tone="bad">{actionError}</Banner>}
      <StatTiles
        dense
        tiles={[
          tile("Running", "running", "info", count("running")),
          tile("Scheduled", "scheduled", "neutral", count("scheduled")),
          tile("Failed", "failed", "bad", count("failed")),
          tile("Succeeded, 30 d", "completed", "ok", recent),
        ]}
      />
      <FilterBar
        label="Campaign filters"
        filters={[
          {
            name: "state",
            label: "State",
            options: (Object.keys(CAMPAIGN_STATES) as CampaignState[]).map((s) => ({
              value: s,
              label: CAMPAIGN_STATES[s][0],
            })),
          },
        ]}
        search={{ name: "q", label: "Search", placeholder: "Campaign name", width: 200 }}
      />
      {inScope.length === 0 ? (
        <EmptyState title="No campaigns yet">
          A campaign updates a site or chosen servers in waves, inside maintenance windows.
        </EmptyState>
      ) : rows.length === 0 ? (
        <EmptyState>No campaign matches these filters.</EmptyState>
      ) : (
        <DataTable
          dense
          label="Campaigns"
          columns={columns}
          rows={rows}
          rowKey={(c) => c.id}
          sort={sort}
          onSortChange={setSort}
          defaultOrder={byRank}
          rowMuted={(c) => isFinal(c.state) && c.state !== "failed"}
          rowActions={(c) =>
            administersCustomer(me, c.customer_id) &&
            ROW_ACTIONS.filter((a) => canAct(a, c.state)).map((a) => (
              <Button key={a} size="small" disabled={busy} onClick={() => void act(c, a)}>
                {ACTION_LABELS[a]} <span className="visually-hidden">{c.name}</span>
              </Button>
            ))
          }
        />
      )}
      {running && <RunningServers campaign={running} sites={data.sites} now={now} />}
      <p className={own.note}>
        {data.more ? `Showing the newest ${data.campaigns.length} campaigns. ` : ""}
        Open a campaign for its waves, windows and Stop.
      </p>
    </div>
  );
}

interface Target extends CampaignServer {
  wave: number;
}

/** Every server of the running campaign, current wave first. */
function RunningServers({ campaign, sites, now }: { campaign: CampaignDetail; sites: Site[]; now: Date }) {
  const servers: Target[] = campaign.waves.flatMap((w) => w.servers.map((s) => ({ ...s, wave: w.index })));
  const current = campaign.current_wave ?? 0;
  const order = (a: Target, b: Target) =>
    Number(a.wave !== current) - Number(b.wave !== current) || a.wave - b.wave || a.hostname.localeCompare(b.hostname);
  const columns: Column<Target>[] = [
    {
      key: "server",
      header: `Server · ${campaign.name}`,
      cell: (s) => (
        <PrimaryCell
          title={s.hostname}
          href={serverHref(s.server_id)}
          sub={sites.find((site) => site.id === s.site_id)?.name}
        />
      ),
    },
    { key: "wave", header: "Wave", cell: (s) => `${s.wave + 1} of ${campaign.wave_count}` },
    {
      key: "state",
      header: "State",
      cell: (s) => (
        <>
          <ServerStatusLabel status={s.status} />
          {s.finished_at && <span className={own.dim}> {shortTime(new Date(s.finished_at), now)}</span>}
        </>
      ),
    },
    {
      key: "result",
      header: "Result",
      cell: (s) => s.error ?? (s.exit_code === null ? <span className={own.dim}>—</span> : `exit ${s.exit_code}`),
    },
  ];
  return (
    <DataTable
      dense
      label="Servers in the running campaign"
      columns={columns}
      rows={servers}
      rowKey={(s) => s.server_id}
      defaultOrder={order}
    />
  );
}
