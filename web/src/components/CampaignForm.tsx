"use client";

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

import own from "./Campaigns.module.css";
import { Breadcrumb } from "./ui/Breadcrumb";
import { Button, fieldClass } from "./ui/Button";
import { Banner, EmptyState, LoadingBlock, NoPermission, PageHeader } from "./ui/PageState";

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
      <NoPermission title="Creating a campaign needs an administrator">
        It needs an administrator role on a whole customer. Ask an administrator for one.
      </NoPermission>
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
  // Agent versions the server holds (ADR-0053); none when unreadable.
  const [agentVersions, setAgentVersions] = useState<string[]>([]);
  const [draft, setDraft] = useState<CampaignDraft>(emptyDraft());
  const [submitted, setSubmitted] = useState(false);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    const controller = new AbortController();
    const { signal } = controller;
    api.agentReleases(signal).then(
      (releases) => {
        setAgentVersions(releases.versions);
        setDraft((d) => (d.agentVersion ? d : { ...d, agentVersion: releases.latest ?? "" }));
      },
      () => setAgentVersions([]),
    );
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
    return error ? <Banner tone="bad">{error}</Banner> : <LoadingBlock label="Loading servers" />;
  }
  if (inventory.customers.length === 0) {
    return <EmptyState>You administer no customer to update.</EmptyState>;
  }

  const sites = inventory.sites.filter((s) => s.customer_id === draft.customerId);
  const servers = inventory.servers
    .filter((s) => s.customer_id === draft.customerId && s.revoked_at === null)
    .sort((a, b) => a.site_name.localeCompare(b.site_name) || a.hostname.localeCompare(b.hostname));
  const agentUpdate = draft.update === "agent";
  const targets = campaignTargets(inventory.servers, draft.customerId, draft.siteId, draft.serverIds, agentUpdate);
  const noPackages = (s: Server) => !agentUpdate && s.package_updates === false;
  const leftOut = servers.filter((s) => noPackages(s) && (s.site_id === draft.siteId || draft.serverIds.includes(s.id))).length;
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
    <div className={own.page}>
      <Breadcrumb parents={[{ label: "Campaigns", href: CAMPAIGNS_PATH }]} current="New campaign" />
      <PageHeader
        detail
        title="New campaign"
        meta="The campaign is created as a draft; nothing runs until you start it. Its servers are fixed now."
      />
      <form className={own.form} onSubmit={submit} aria-label="New campaign" noValidate>
        <div className={own.grid}>
          <label className={own.fld}>
            Customer
            <select
              className={fieldClass}
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
          <label className={own.fld}>
            Name
            <input
              className={fieldClass}
              value={draft.name}
              onChange={(e) => set({ name: e.target.value })}
              maxLength={64}
              required
            />
          </label>
        </div>

        <Fieldset legend="Servers">
          <label className={own.fld}>
            Every server of the site
            <select className={fieldClass} value={draft.siteId} onChange={(e) => set({ siteId: e.target.value })}>
              <option value="">No site</option>
              {sites.map((s) => (
                <option key={s.id} value={s.id}>
                  {s.name}
                </option>
              ))}
            </select>
          </label>
          <p className={own.note}>And or instead, these servers:</p>
          <ul className={own.servers} aria-label="Servers">
            {servers.map((s) => (
              <li key={s.id}>
                <label>
                  <input
                    type="checkbox"
                    checked={!noPackages(s) && (draft.serverIds.includes(s.id) || s.site_id === draft.siteId)}
                    disabled={noPackages(s) || s.site_id === draft.siteId}
                    onChange={(e) => set({ serverIds: toggle(draft.serverIds, s.id, e.target.checked) })}
                  />
                  {s.hostname} <span className={own.dim}>({s.site_name}{noPackages(s) && ", no package updates"})</span>
                </label>
              </li>
            ))}
          </ul>
          <p className={own.note} role="status">
            {targets.length} server{targets.length === 1 ? "" : "s"} targeted.
            {leftOut > 0 && ` ${leftOut} left out: package updates aren't available on ${leftOut === 1 ? "it" : "them"}.`}
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
          <div className={own.row}>
            <Button onClick={() => set({ waves: [...draft.waves, { kind: "size", value: "1", servers: [] }] })}>
              Add wave
            </Button>
          </div>
          <WavePlan draft={draft} targets={targets} />
        </Fieldset>

        <Fieldset legend="Update">
          <div className={own.row} role="radiogroup" aria-label="Update kind">
            {(
              [
                ["all", "All upgrades"],
                ["security", "Security updates only"],
                ["packages", "These packages"],
                ["agent", "The MonitorFlow agent"],
              ] as [UpdateKind, string][]
            )
              .filter(([kind]) => kind !== "agent" || agentVersions.length > 0)
              .map(([kind, label]) => (
                <label key={kind} className={own.inline}>
                  <input type="radio" name="update" checked={draft.update === kind} onChange={() => set({ update: kind })} />
                  {label}
                </label>
              ))}
          </div>
          {draft.update === "agent" && (
            <label className={own.fld}>
              Agent version
              <select className={fieldClass} value={draft.agentVersion} onChange={(e) => set({ agentVersion: e.target.value })}>
                {agentVersions.map((v) => (
                  <option key={v} value={v}>
                    {v}
                  </option>
                ))}
              </select>
            </label>
          )}
          {draft.update === "packages" && (
            <label className={own.fld}>
              Packages, or KB numbers for Windows servers
              <textarea
                className={own.textarea}
                value={draft.packages}
                onChange={(e) => set({ packages: e.target.value })}
                rows={2}
                placeholder="openssl libssl3 or KB5034441"
              />
            </label>
          )}
          <div className={own.row}>
            {draft.update !== "agent" && (
              <label className={own.inline}>
                <input
                  type="checkbox"
                  checked={draft.allowReboot}
                  onChange={(e) => set({ allowReboot: e.target.checked })}
                />
                Allow reboot
              </label>
            )}
            <label className={own.fld}>
              Each command may start within
              <select className={fieldClass} value={draft.ttl} onChange={(e) => set({ ttl: Number(e.target.value) })}>
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
          {draft.windows.length === 0 && <p className={own.note}>No window: commands may be queued at any time.</p>}
          {draft.windows.map((window, i) => (
            <WindowRow
              key={i}
              index={i}
              window={window}
              onChange={(w) => set({ windows: replace(draft.windows, i, w) })}
              onRemove={() => set({ windows: draft.windows.filter((_, j) => j !== i) })}
            />
          ))}
          <div className={own.row}>
            <Button onClick={() => set({ windows: [...draft.windows, newWindow("weekly")] })}>Add weekly window</Button>
            <Button onClick={() => set({ windows: [...draft.windows, newWindow("once")] })}>Add one-off window</Button>
          </div>
        </Fieldset>

        <Fieldset legend="Failure threshold">
          <div className={own.row}>
            <label className={own.fld}>
              Stop when more than
              <input
                className={fieldClass}
                type="number"
                min={0}
                value={draft.thresholdValue}
                onChange={(e) => set({ thresholdValue: e.target.value })}
              />
            </label>
            <label className={own.fld}>
              Unit
              <select
                className={fieldClass}
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
          <Banner tone="bad">
            <strong>Fix these first:</strong>
            <ul className={own.errors}>
              {errors.map((e) => (
                <li key={e}>{e}</li>
              ))}
            </ul>
          </Banner>
        )}
        {error && <Banner tone="bad">{error}</Banner>}
        <div className={own.row}>
          <Button variant="k" type="submit" disabled={busy}>
            Create campaign
          </Button>
        </div>
      </form>
    </div>
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
      <label className={own.fld}>
        {label}
        <select className={fieldClass} value={wave.kind} onChange={(e) => onChange({ ...wave, kind: e.target.value as WaveDraft["kind"] })}>
          <option value="size">Number of servers</option>
          <option value="percent">Percentage of servers</option>
          <option value="servers">Chosen servers</option>
        </select>
      </label>
      {wave.kind === "servers" ? (
        <label className={own.fld}>
          Servers
          <select
            className={own.multiple}
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
        <label className={own.fld}>
          {wave.kind === "size" ? "Servers" : "Percent"}
          <input
            className={fieldClass}
            type="number"
            min={1}
            max={wave.kind === "percent" ? 100 : undefined}
            value={wave.value}
            onChange={(e) => onChange({ ...wave, value: e.target.value })}
          />
        </label>
      )}
      <Button onClick={onRemove} aria-label={`Remove ${label.toLowerCase()}`}>
        Remove
      </Button>
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
  if ("error" in plan) return <p className={own.note}>Waves cannot be planned: {plan.error}.</p>;
  const sizes = plan.waves.map((w, i) => `wave ${i + 1}: ${w.length}`).join(", ");
  return (
    <p className={own.note}>
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
          <label className={own.fld}>
            From
            <input className={fieldClass} type="time" value={window.start} onChange={(e) => set({ start: e.target.value })} />
          </label>
          <label className={own.fld}>
            To
            <input className={fieldClass} type="time" value={window.end} onChange={(e) => set({ end: e.target.value })} />
          </label>
          <label className={own.fld}>
            Time zone
            <input className={fieldClass} value={window.timezone} onChange={(e) => set({ timezone: e.target.value })} />
          </label>
        </>
      ) : (
        <>
          <label className={own.fld}>
            Starts (your local time)
            <input className={fieldClass} type="datetime-local" value={window.startsAt} onChange={(e) => set({ startsAt: e.target.value })} />
          </label>
          <label className={own.fld}>
            Ends
            <input className={fieldClass} type="datetime-local" value={window.endsAt} onChange={(e) => set({ endsAt: e.target.value })} />
          </label>
        </>
      )}
      <Button onClick={onRemove} aria-label={`Remove ${label.toLowerCase()}`}>
        Remove
      </Button>
    </div>
  );
}

function Fieldset({ legend, children }: { legend: string; children: ReactNode }) {
  return (
    <fieldset className={own.fieldset}>
      <legend>{legend}</legend>
      <div className={own.fields}>{children}</div>
    </fieldset>
  );
}

function toggle<T>(list: T[], item: T, on: boolean): T[] {
  return on ? (list.includes(item) ? list : [...list, item]) : list.filter((x) => x !== item);
}

function replace<T>(list: T[], index: number, item: T): T[] {
  return list.map((x, i) => (i === index ? item : x));
}
