"use client";

import Link from "next/link";
import { useSearchParams } from "next/navigation";
import { type ReactNode, useCallback, useState } from "react";

import {
  ApiError,
  type CampaignAction,
  type CampaignDetail,
  type CampaignWave,
  type Me,
  api,
  describeError,
  isAbortError,
} from "@/lib/api";
import { canAct, describeThreshold, describeUpdate, describeWindow, isFinal } from "@/lib/campaigns";
import { formatDateTime } from "@/lib/format";
import { administersCustomer, readsCustomers, useMe } from "@/lib/me";
import { CAMPAIGNS_PATH, serverHref } from "@/lib/routes";
import { usePolling } from "@/lib/usePolling";
import { UUID } from "@/lib/uuid";

import styles from "./Admin.module.css";
import { Progress, StateBadge, waveText } from "./CampaignList";
import own from "./Campaigns.module.css";
import { REFRESH_MS } from "./FleetOverview";

/** `/campaigns/view/?id=…` (ADR-0020 routing). */
export function CampaignView() {
  const id = useSearchParams().get("id");
  const me = useMe();
  if (!me || !readsCustomers(me)) {
    return <p className={styles.empty}>Campaigns need a role on a whole customer. Ask an administrator for one.</p>;
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

  if (!valid || campaign === null) {
    return (
      <section className={styles.empty}>
        <h1>Campaign not found</h1>
        <p>It may not exist, or your roles do not cover its customer.</p>
        <Link href={CAMPAIGNS_PATH}>Back to campaigns</Link>
      </section>
    );
  }
  if (!campaign) {
    return error ? (
      <p className={styles.error} role="alert">
        {error}
      </p>
    ) : (
      <p role="status">Loading campaign…</p>
    );
  }
  return (
    <article aria-labelledby="campaign-title">
      <p className={styles.meta}>
        <Link href={CAMPAIGNS_PATH}>Campaigns</Link> / {campaign.customer_name}
      </p>
      <h1 id="campaign-title">{campaign.name}</h1>
      {error && (
        <p className={styles.error} role="alert">
          Refresh failed: {error} Showing the last data received.
        </p>
      )}
      <dl className={styles.facts}>
        <Fact term="State">
          <StateBadge state={campaign.state} />
        </Fact>
        <Fact term="Progress">{waveText(campaign)}</Fact>
        <Fact term="Update">{describeUpdate(campaign.spec)}</Fact>
        <Fact term="Stops at">{describeThreshold(campaign.failure_threshold)}</Fact>
        <Fact term="Windows">
          {campaign.windows.length === 0 ? "any time" : campaign.windows.map(describeWindow).join("; ")}
        </Fact>
        <Fact term="Created">{formatDateTime(campaign.created_at)}</Fact>
        {campaign.started_at && <Fact term="Started">{formatDateTime(campaign.started_at)}</Fact>}
        {campaign.finished_at && <Fact term="Finished">{formatDateTime(campaign.finished_at)}</Fact>}
      </dl>
      {campaign.error && <p className={styles.error}>{campaign.error}</p>}
      <Progress progress={campaign.progress} />
      {administersCustomer(me, campaign.customer_id) && <Actions campaign={campaign} onChange={setCampaign} />}
      {campaign.waves.map((wave) => (
        <Wave key={wave.index} wave={wave} />
      ))}
    </article>
  );
}

const LABELS: Record<CampaignAction, string> = { start: "Start", pause: "Pause", resume: "Resume", stop: "Stop" };

function Actions({ campaign, onChange }: { campaign: CampaignDetail; onChange: (c: CampaignDetail) => void }) {
  const [busy, setBusy] = useState(false);
  const [confirming, setConfirming] = useState(false);
  const [error, setError] = useState<string | null>(null);

  async function act(action: CampaignAction) {
    setBusy(true);
    setError(null);
    try {
      onChange(await api.campaignAction(campaign.id, action));
      setConfirming(false);
    } catch (e) {
      setError(`${LABELS[action]} failed. ${describeError(e)}`);
    } finally {
      setBusy(false);
    }
  }

  return (
    <>
      <div className={own.actions}>
        {(["start", "pause", "resume"] as const).map((action) => (
          <button
            key={action}
            className={styles.button}
            type="button"
            disabled={busy || !canAct(action, campaign.state)}
            onClick={() => void act(action)}
          >
            {LABELS[action]}
          </button>
        ))}
        <button
          className={styles.danger}
          type="button"
          disabled={busy || confirming || !canAct("stop", campaign.state)}
          onClick={() => setConfirming(true)}
        >
          Stop…
        </button>
      </div>
      {confirming && (
        <div className={styles.confirm} role="group" aria-label="Stop the campaign">
          <p>
            Stopping is final. Commands not sent yet are cancelled; those already on their servers finish and are
            counted.
          </p>
          <button className={styles.danger} type="button" disabled={busy} onClick={() => void act("stop")}>
            Stop the campaign
          </button>{" "}
          <button className={styles.secondary} type="button" onClick={() => setConfirming(false)}>
            Cancel
          </button>
        </div>
      )}
      {error && (
        <p className={styles.error} role="alert">
          {error}
        </p>
      )}
    </>
  );
}

function Wave({ wave }: { wave: CampaignWave }) {
  const title = `Wave ${wave.index + 1}`;
  return (
    <section className={own.wave} aria-label={title}>
      <h2>
        {title} <StateBadge state={wave.state === "done" ? "completed" : wave.state} />
      </h2>
      <p className={styles.meta}>
        {wave.started_at ? `Started ${formatDateTime(wave.started_at)}` : "Not started"}
        {wave.finished_at && `, finished ${formatDateTime(wave.finished_at)}`}
      </p>
      <Progress progress={wave.progress} />
      <div className={styles.scroll}>
        <table className={styles.table} aria-label={`Servers of ${title.toLowerCase()}`}>
          <thead>
            <tr>
              <th scope="col">Server</th>
              <th scope="col">Status</th>
              <th scope="col">Exit code</th>
              <th scope="col">Finished</th>
              <th scope="col">Error</th>
            </tr>
          </thead>
          <tbody>
            {wave.servers.map((server) => (
              <tr key={server.server_id}>
                <th scope="row">
                  <Link href={serverHref(server.server_id)}>{server.hostname}</Link>
                </th>
                <td>
                  <StateBadge state={server.status} />
                </td>
                <td>{server.exit_code ?? "–"}</td>
                <td>{server.finished_at ? formatDateTime(server.finished_at) : "–"}</td>
                <td>{server.error ?? ""}</td>
              </tr>
            ))}
          </tbody>
        </table>
      </div>
    </section>
  );
}

function Fact({ term, children }: { term: string; children: ReactNode }) {
  return (
    <div>
      <dt>{term}</dt>
      <dd>{children}</dd>
    </div>
  );
}
