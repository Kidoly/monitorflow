"use client";

import Link from "next/link";
import { useRouter } from "next/navigation";
import { type FormEvent, type ReactNode, useEffect, useState } from "react";

import { type Customer, type Me, type Server, type Site, api, describeError, isAbortError } from "@/lib/api";
import {
  type CampaignDraft,
  type UpdateKind,
  type WaveDraft,
  type WindowDraft,
  WEEKDAYS,
  campaignTargets,
  emptyDraft,
  newWindow,
  planWaves,
  toRequest,
  validateCampaign,
} from "@/lib/campaigns";
import { administersCustomer, useMe } from "@/lib/me";
import { CAMPAIGNS_PATH, campaignHref } from "@/lib/routes";

import styles from "./Admin.module.css";
import own from "./Campaigns.module.css";

/** Command validity choices; the API takes 60 s to 7 days. */
const TTLS: [number, string][] = [
  [900, "15 minutes"],
  [3600, "1 hour"],
  [4 * 3600, "4 hours"],
  [86_400, "1 day"],
  [604_800, "7 days"],
];

/** `/campaigns/new/`. */
export function NewCampaignPage() {
  const me = useMe();
  if (!me || !administersCustomer(me)) {
    return (
      <p className={styles.empty}>
        Creating a campaign needs an administrator role on a whole customer. Ask an administrator for one.
      </p>
    );
  }
  return <CampaignForm me={me} />;
}

interface Inventory {
  customers: Customer[];
  sites: Site[];
  servers: Server[];
}

export function CampaignForm({ me }: { me: Me }) {
  const router = useRouter();
  const [inventory, setInventory] = useState<Inventory | null>(null);
  const [draft, setDraft] = useState<CampaignDraft>(emptyDraft());
  const [submitted, setSubmitted] = useState(false);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    const controller = new AbortController();
    const { signal } = controller;
    Promise.all([api.customers(signal), api.sites(signal), api.servers(signal)]).then(
      ([customers, sites, servers]) => {
        const administered = customers.customers.filter((c) => administersCustomer(me, c.id));
        setInventory({ customers: administered, sites: sites.sites, servers: servers.servers });
        setDraft((d) => (d.customerId ? d : { ...d, customerId: administered[0]?.id ?? "" }));
      },
      (e: unknown) => {
        if (!isAbortError(e)) setError(describeError(e));
      },
    );
    return () => controller.abort();
  }, [me]);

  const set = (patch: Partial<CampaignDraft>) => setDraft((d) => ({ ...d, ...patch }));

  if (!inventory) {
    return error ? (
      <p className={styles.error} role="alert">
        {error}
      </p>
    ) : (
      <p role="status">Loading servers…</p>
    );
  }
  if (inventory.customers.length === 0) {
    return <p className={styles.empty}>You administer no customer to update.</p>;
  }

  const sites = inventory.sites.filter((s) => s.customer_id === draft.customerId);
  const servers = inventory.servers
    .filter((s) => s.customer_id === draft.customerId && s.revoked_at === null)
    .sort((a, b) => a.site_name.localeCompare(b.site_name) || a.hostname.localeCompare(b.hostname));
  const targets = campaignTargets(inventory.servers, draft.customerId, draft.siteId, draft.serverIds);
  const request = toRequest(draft);
  const errors = validateCampaign(request, targets.map((t) => t.id));

  async function submit(event: FormEvent) {
    event.preventDefault();
    setSubmitted(true);
    setError(null);
    if (errors.length > 0) return;
    setBusy(true);
    try {
      const created = await api.createCampaign(request);
      router.push(campaignHref(created.id));
    } catch (e) {
      setError(`Creating the campaign failed. ${describeError(e)}`);
      setBusy(false);
    }
  }

  return (
    <section aria-labelledby="new-campaign-title">
      <p className={styles.meta}>
        <Link href={CAMPAIGNS_PATH}>Campaigns</Link> / New
      </p>
      <h1 id="new-campaign-title">New campaign</h1>
      <p className={styles.note}>
        The campaign is created as a draft; nothing runs until you start it. Its servers are fixed now.
      </p>
      <form className={styles.form} onSubmit={submit} aria-label="New campaign" noValidate>
        <label>
          Customer
          <select
            value={draft.customerId}
            onChange={(e) => set({ customerId: e.target.value, siteId: "", serverIds: [], waves: [] })}
          >
            {inventory.customers.map((c) => (
              <option key={c.id} value={c.id}>
                {c.name}
              </option>
            ))}
          </select>
        </label>
        <label>
          Name
          <input value={draft.name} onChange={(e) => set({ name: e.target.value })} maxLength={64} required />
        </label>

        <Fieldset legend="Servers">
          <label>
            Every server of the site
            <select value={draft.siteId} onChange={(e) => set({ siteId: e.target.value })}>
              <option value="">No site</option>
              {sites.map((s) => (
                <option key={s.id} value={s.id}>
                  {s.name}
                </option>
              ))}
            </select>
          </label>
          <p className={styles.note}>And or instead, these servers:</p>
          <ul className={own.servers} aria-label="Servers">
            {servers.map((s) => (
              <li key={s.id}>
                <label>
                  <input
                    type="checkbox"
                    checked={draft.serverIds.includes(s.id) || s.site_id === draft.siteId}
                    disabled={s.site_id === draft.siteId}
                    onChange={(e) => set({ serverIds: toggle(draft.serverIds, s.id, e.target.checked) })}
                  />
                  {s.hostname} <span className={styles.meta}>({s.site_name})</span>
                </label>
              </li>
            ))}
          </ul>
          <p role="status">
            {targets.length} server{targets.length === 1 ? "" : "s"} targeted.
          </p>
        </Fieldset>

        <Fieldset legend="Waves">
          {draft.waves.map((wave, i) => (
            <WaveRow
              key={i}
              index={i}
              wave={wave}
              targets={targets}
              onChange={(w) => set({ waves: replace(draft.waves, i, w) })}
              onRemove={() => set({ waves: draft.waves.filter((_, j) => j !== i) })}
            />
          ))}
          <button
            className={styles.secondary}
            type="button"
            onClick={() => set({ waves: [...draft.waves, { kind: "size", value: "1", servers: [] }] })}
          >
            Add wave
          </button>
          <WavePlan draft={draft} targets={targets} />
        </Fieldset>

        <Fieldset legend="Update">
          <div className={own.row} role="radiogroup" aria-label="Update kind">
            {(
              [
                ["all", "All upgrades"],
                ["security", "Security updates only"],
                ["packages", "These packages"],
              ] as [UpdateKind, string][]
            ).map(([kind, label]) => (
              <label key={kind} className={own.inline}>
                <input type="radio" name="update" checked={draft.update === kind} onChange={() => set({ update: kind })} />
                {label}
              </label>
            ))}
          </div>
          {draft.update === "packages" && (
            <label>
              Packages
              <textarea
                value={draft.packages}
                onChange={(e) => set({ packages: e.target.value })}
                rows={2}
                placeholder="openssl libssl3"
              />
            </label>
          )}
          <div className={own.row}>
            <label className={own.inline}>
              <input
                type="checkbox"
                checked={draft.allowReboot}
                onChange={(e) => set({ allowReboot: e.target.checked })}
              />
              Allow reboot
            </label>
            <label>
              Each command may start within
              <select value={draft.ttl} onChange={(e) => set({ ttl: Number(e.target.value) })}>
                {TTLS.map(([seconds, label]) => (
                  <option key={seconds} value={seconds}>
                    {label}
                  </option>
                ))}
              </select>
            </label>
          </div>
        </Fieldset>

        <Fieldset legend="Maintenance windows">
          {draft.windows.length === 0 && <p className={styles.note}>No window: commands may be queued at any time.</p>}
          {draft.windows.map((window, i) => (
            <WindowRow
              key={i}
              index={i}
              window={window}
              onChange={(w) => set({ windows: replace(draft.windows, i, w) })}
              onRemove={() => set({ windows: draft.windows.filter((_, j) => j !== i) })}
            />
          ))}
          <button
            className={styles.secondary}
            type="button"
            onClick={() => set({ windows: [...draft.windows, newWindow("weekly")] })}
          >
            Add weekly window
          </button>{" "}
          <button
            className={styles.secondary}
            type="button"
            onClick={() => set({ windows: [...draft.windows, newWindow("once")] })}
          >
            Add one-off window
          </button>
        </Fieldset>

        <Fieldset legend="Failure threshold">
          <div className={own.row}>
            <label>
              Stop when more than
              <input
                type="number"
                min={0}
                value={draft.thresholdValue}
                onChange={(e) => set({ thresholdValue: e.target.value })}
              />
            </label>
            <label>
              Unit
              <select
                value={draft.thresholdUnit}
                onChange={(e) => set({ thresholdUnit: e.target.value as CampaignDraft["thresholdUnit"] })}
              >
                <option value="count">servers of a wave fail</option>
                <option value="percent">percent of a wave fail</option>
              </select>
            </label>
          </div>
        </Fieldset>

        {submitted && errors.length > 0 && (
          <div className={styles.error} role="alert">
            <p>Fix these first:</p>
            <ul>
              {errors.map((e) => (
                <li key={e}>{e}</li>
              ))}
            </ul>
          </div>
        )}
        {error && (
          <p className={styles.error} role="alert">
            {error}
          </p>
        )}
        <button className={styles.button} type="submit" disabled={busy}>
          Create campaign
        </button>
      </form>
    </section>
  );
}

function WaveRow({
  index,
  wave,
  targets,
  onChange,
  onRemove,
}: {
  index: number;
  wave: WaveDraft;
  targets: Server[];
  onChange: (wave: WaveDraft) => void;
  onRemove: () => void;
}) {
  const label = `Wave ${index + 1}`;
  return (
    <div className={own.row} role="group" aria-label={label}>
      <label>
        {label}
        <select value={wave.kind} onChange={(e) => onChange({ ...wave, kind: e.target.value as WaveDraft["kind"] })}>
          <option value="size">Number of servers</option>
          <option value="percent">Percentage of servers</option>
          <option value="servers">Chosen servers</option>
        </select>
      </label>
      {wave.kind === "servers" ? (
        <label>
          Servers
          <select
            multiple
            value={wave.servers}
            onChange={(e) => onChange({ ...wave, servers: [...e.target.selectedOptions].map((o) => o.value) })}
          >
            {targets.map((t) => (
              <option key={t.id} value={t.id}>
                {t.hostname}
              </option>
            ))}
          </select>
        </label>
      ) : (
        <label>
          {wave.kind === "size" ? "Servers" : "Percent"}
          <input
            type="number"
            min={1}
            max={wave.kind === "percent" ? 100 : undefined}
            value={wave.value}
            onChange={(e) => onChange({ ...wave, value: e.target.value })}
          />
        </label>
      )}
      <button className={styles.secondary} type="button" onClick={onRemove} aria-label={`Remove ${label.toLowerCase()}`}>
        Remove
      </button>
    </div>
  );
}

/** How the targets would be split, as the server plans it. */
function WavePlan({ draft, targets }: { draft: CampaignDraft; targets: Server[] }) {
  if (targets.length === 0) return null;
  const plan = planWaves(
    targets.map((t) => t.id),
    toRequest(draft).waves,
  );
  if ("error" in plan) return <p className={styles.note}>Waves cannot be planned: {plan.error}.</p>;
  const sizes = plan.waves.map((w, i) => `wave ${i + 1}: ${w.length}`).join(", ");
  return (
    <p className={styles.note}>
      Servers per wave: {sizes}. Servers not placed by a wave form the last one.
    </p>
  );
}

function WindowRow({
  index,
  window,
  onChange,
  onRemove,
}: {
  index: number;
  window: WindowDraft;
  onChange: (window: WindowDraft) => void;
  onRemove: () => void;
}) {
  const label = `Window ${index + 1}`;
  const set = (patch: Partial<WindowDraft>) => onChange({ ...window, ...patch });
  return (
    <div className={own.row} role="group" aria-label={label}>
      {window.kind === "weekly" ? (
        <>
          {WEEKDAYS.map((day, i) => (
            <label key={day} className={own.inline}>
              <input
                type="checkbox"
                checked={window.weekdays.includes(i + 1)}
                onChange={(e) => set({ weekdays: toggle(window.weekdays, i + 1, e.target.checked) })}
              />
              {day}
            </label>
          ))}
          <label>
            From
            <input type="time" value={window.start} onChange={(e) => set({ start: e.target.value })} />
          </label>
          <label>
            To
            <input type="time" value={window.end} onChange={(e) => set({ end: e.target.value })} />
          </label>
          <label>
            Time zone
            <input value={window.timezone} onChange={(e) => set({ timezone: e.target.value })} />
          </label>
        </>
      ) : (
        <>
          <label>
            Starts (your local time)
            <input type="datetime-local" value={window.startsAt} onChange={(e) => set({ startsAt: e.target.value })} />
          </label>
          <label>
            Ends
            <input type="datetime-local" value={window.endsAt} onChange={(e) => set({ endsAt: e.target.value })} />
          </label>
        </>
      )}
      <button className={styles.secondary} type="button" onClick={onRemove} aria-label={`Remove ${label.toLowerCase()}`}>
        Remove
      </button>
    </div>
  );
}

function Fieldset({ legend, children }: { legend: string; children: ReactNode }) {
  return (
    <fieldset className={own.fieldset}>
      <legend>{legend}</legend>
      {children}
    </fieldset>
  );
}

function toggle<T>(list: T[], item: T, on: boolean): T[] {
  return on ? (list.includes(item) ? list : [...list, item]) : list.filter((x) => x !== item);
}

function replace<T>(list: T[], index: number, item: T): T[] {
  return list.map((x, i) => (i === index ? item : x));
}
