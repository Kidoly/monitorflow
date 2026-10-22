"use client";

import Link from "next/link";
import { useCallback, useState } from "react";

import {
  type Campaign,
  type CampaignProgress,
  type CampaignServerStatus,
  type CampaignState,
  api,
  describeError,
  isAbortError,
} from "@/lib/api";
import { formatDateTime } from "@/lib/format";
import { administersCustomer, readsCustomers, useMe } from "@/lib/me";
import { NEW_CAMPAIGN_PATH, campaignHref } from "@/lib/routes";
import { usePolling } from "@/lib/usePolling";

import styles from "./Admin.module.css";
import own from "./Campaigns.module.css";
import { REFRESH_MS } from "./FleetOverview";

/** `/campaigns/`: the newest campaigns, refreshed while the tab is visible. */
export function CampaignListPage() {
  const me = useMe();
  const allowed = readsCustomers(me);
  const [campaigns, setCampaigns] = useState<Campaign[] | null>(null);
  const [more, setMore] = useState(false);
  const [error, setError] = useState<string | null>(null);

  const load = useCallback(async (signal?: AbortSignal) => {
    try {
      const list = await api.campaigns(signal);
      setCampaigns(list.campaigns);
      setMore(list.next_cursor !== null);
      setError(null);
    } catch (e) {
      if (!isAbortError(e)) setError(describeError(e));
    }
  }, []);
  usePolling(load, REFRESH_MS, allowed);

  if (!allowed) {
    return (
      <p className={styles.empty}>
        Campaigns span sites: they need a role on a whole customer. Ask an administrator for one.
      </p>
    );
  }
  return (
    <section aria-labelledby="campaigns-title">
      <h1 id="campaigns-title">Update campaigns</h1>
      {administersCustomer(me) && (
        <p>
          <Link className={styles.button} href={NEW_CAMPAIGN_PATH}>
            New campaign
          </Link>
        </p>
      )}
      {error && (
        <p className={styles.error} role="alert">
          {error}
        </p>
      )}
      {!campaigns && !error && <p role="status">Loading campaigns…</p>}
      {campaigns?.length === 0 && <p className={styles.empty}>No campaigns yet.</p>}
      {campaigns && campaigns.length > 0 && (
        <div className={styles.scroll}>
          <table className={styles.table} aria-labelledby="campaigns-title">
            <thead>
              <tr>
                <th scope="col">Name</th>
                <th scope="col">Customer</th>
                <th scope="col">State</th>
                <th scope="col">Wave</th>
                <th scope="col">Progress</th>
                <th scope="col">Created</th>
              </tr>
            </thead>
            <tbody>
              {campaigns.map((campaign) => (
                <tr key={campaign.id}>
                  <th scope="row">
                    <Link href={campaignHref(campaign.id)}>{campaign.name}</Link>
                  </th>
                  <td>{campaign.customer_name}</td>
                  <td>
                    <StateBadge state={campaign.state} />
                  </td>
                  <td>{waveText(campaign)}</td>
                  <td>
                    <Progress progress={campaign.progress} />
                  </td>
                  <td>{formatDateTime(campaign.created_at)}</td>
                </tr>
              ))}
            </tbody>
          </table>
        </div>
      )}
      {more && <p className={styles.note}>Showing the newest {campaigns?.length} campaigns.</p>}
    </section>
  );
}

/** "Wave 2 of 3", or how many waves before the first is queued. */
export function waveText(campaign: Pick<Campaign, "current_wave" | "wave_count">): string {
  const waves = `${campaign.wave_count} wave${campaign.wave_count === 1 ? "" : "s"}`;
  return campaign.current_wave === null ? waves : `Wave ${campaign.current_wave + 1} of ${campaign.wave_count}`;
}

const TONES: Partial<Record<CampaignState | CampaignServerStatus, "good" | "bad" | "busy">> = {
  completed: "good",
  succeeded: "good",
  failed: "bad",
  expired: "bad",
  skipped: "bad",
  stopped: "bad",
  running: "busy",
  scheduled: "busy",
  queued: "busy",
  sent: "busy",
};

/** A campaign state or a server status, coloured and always written out. */
export function StateBadge({ state }: { state: CampaignState | CampaignServerStatus }) {
  return (
    <span className={own.state} data-tone={TONES[state]}>
      {state}
    </span>
  );
}

/** Counts in words, then a bar of the same counts. */
export function Progress({ progress }: { progress: CampaignProgress }) {
  const { total, succeeded, failed, in_progress, cancelled, pending } = progress;
  const parts = [
    `${succeeded} succeeded`,
    failed > 0 && `${failed} failed`,
    in_progress > 0 && `${in_progress} in progress`,
    cancelled > 0 && `${cancelled} cancelled`,
    pending > 0 && `${pending} pending`,
  ].filter(Boolean);
  const width = (n: number) => ({ width: `${total ? (n * 100) / total : 0}%` });
  return (
    <div>
      <div className={own.bar} aria-hidden="true">
        <span className={own.succeeded} style={width(succeeded)} />
        <span className={own.failed} style={width(failed)} />
        <span className={own.inProgress} style={width(in_progress)} />
        <span className={own.cancelled} style={width(cancelled)} />
      </div>
      <span className={own.counts}>
        {parts.join(", ")} of {total}
      </span>
    </div>
  );
}
