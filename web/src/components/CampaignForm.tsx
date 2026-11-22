"use client";

import { useRouter } from "next/navigation";
import { type FormEvent, type ReactNode, useEffect, useState } from "react";

import { type PlainKey, t } from "@/i18n";
import { type Customer, type Me, type Server, type Site, api, describeError, isAbortError } from "@/lib/api";
import {
  type CampaignDraft,
  type UpdateKind,
  type UpdateSources,
  type WaveDraft,
  type WindowDraft,
  MANAGER_NAMES,
  THIRD_PARTY_MANAGERS,
  WEEKDAYS,
  campaignTargets,
  managerFits,
  weekdayName,
  emptyDraft,
  newWindow,
  planWaves,
  toRequest,
  validateCampaign,
} from "@/lib/campaigns";
import { formatDuration } from "@/lib/format";
import { administersCustomer, multiCustomer, multiSite, useMe } from "@/lib/me";
import { CAMPAIGNS_PATH, campaignHref } from "@/lib/routes";
import { Switch } from "@/components/ui/Switch";

import own from "./Campaigns.module.css";
import { Breadcrumb } from "./ui/Breadcrumb";
import { Button, ButtonLink, fieldClass } from "./ui/Button";
import { Banner, EmptyState, LoadingBlock, NoPermission, PageHeader } from "./ui/PageState";

/** Command validity choices; the API takes 60 s to 7 days. */
const TTLS = [900, 3600, 4 * 3600, 86_400, 604_800];

const SOURCES: [UpdateSources, PlainKey][] = [
  ["os", "campaigns.form.sources.os"],
  ["apps", "campaigns.form.sources.apps"],
  ["both", "campaigns.form.sources.both"],
];

const UPDATE_KINDS: [UpdateKind, PlainKey][] = [
  ["all", "campaigns.update.all"],
  ["security", "campaigns.form.security"],
  ["packages", "campaigns.form.packages"],
  ["agent", "campaigns.form.agent"],
];

/** `/campaigns/new/`. */
export function NewCampaignPage() {
  const me = useMe();
  if (!me || !administersCustomer(me)) {
    return (
      <NoPermission title={t("campaigns.form.adminTitle")}>
        {multiCustomer(me) ? t("campaigns.form.adminCustomer") : t("campaigns.form.adminSites")}
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
    return error ? <Banner tone="bad">{error}</Banner> : <LoadingBlock label={t("campaigns.form.loading")} />;
  }
  if (inventory.customers.length === 0) {
    return (
      <EmptyState actions={<ButtonLink href={CAMPAIGNS_PATH}>{t("campaigns.backToCampaigns")}</ButtonLink>}>
        {multiCustomer(me) ? t("campaigns.form.noCustomer") : t("campaigns.form.noSite")}
      </EmptyState>
    );
  }

  const sites = inventory.sites.filter((s) => s.customer_id === draft.customerId);
  const servers = inventory.servers
    .filter((s) => s.customer_id === draft.customerId && s.revoked_at === null)
    .sort((a, b) => a.site_name.localeCompare(b.site_name) || a.hostname.localeCompare(b.hostname));
  const agentUpdate = draft.update === "agent";
  const appsOnly = !agentUpdate && draft.sources === "apps" ? draft.managers : null;
  const targets = campaignTargets(inventory.servers, draft.customerId, draft.siteId, draft.serverIds, agentUpdate, appsOnly);
  const noPackages = (s: Server) =>
    appsOnly ? !appsOnly.some((m) => managerFits(m, s.os_family)) : !agentUpdate && s.package_updates === false;
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
      setError(t("campaigns.form.createFailed", { error: describeError(e) }));
      setBusy(false);
    }
  }

  return (
    <div className={own.page}>
      <Breadcrumb parents={[{ label: t("campaigns.label"), href: CAMPAIGNS_PATH }]} />
      <PageHeader detail title={t("campaigns.newCampaign")} meta={t("campaigns.form.meta")} />
      <form className={own.form} onSubmit={submit} aria-label={t("campaigns.newCampaign")} noValidate>
        <div className={own.grid}>
          {multiCustomer(me) && (
            <label className={own.fld}>
              {t("campaigns.fact.customer")}
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
          )}
          <label className={own.fld}>
            {t("campaigns.form.name")}
            <input
              className={fieldClass}
              value={draft.name}
              onChange={(e) => set({ name: e.target.value })}
              maxLength={64}
              required
            />
          </label>
        </div>

        <Fieldset legend={t("campaigns.form.servers")}>
          <label className={own.fld}>
            {t("campaigns.form.site")}
            <select className={fieldClass} value={draft.siteId} onChange={(e) => set({ siteId: e.target.value })}>
              <option value="">{t("campaigns.form.noSiteOption")}</option>
              {sites.map((s) => (
                <option key={s.id} value={s.id}>
                  {s.name}
                </option>
              ))}
            </select>
          </label>
          <p className={own.note}>{t("campaigns.form.alsoServers")}</p>
          <ul className={own.servers} aria-label={t("campaigns.form.servers")}>
            {servers.map((s) => (
              <li key={s.id}>
                <label>
                  <input
                    type="checkbox"
                    checked={!noPackages(s) && (draft.serverIds.includes(s.id) || s.site_id === draft.siteId)}
                    disabled={noPackages(s) || s.site_id === draft.siteId}
                    onChange={(e) => set({ serverIds: toggle(draft.serverIds, s.id, e.target.checked) })}
                  />
                  {s.hostname}
                  {(multiSite(me) || noPackages(s)) && (
                    <>
                      {" "}
                      <span className={own.dim}>
                        ({[multiSite(me) && s.site_name, noPackages(s) && t("campaigns.form.noPackages")].filter(Boolean).join(", ")})
                      </span>
                    </>
                  )}
                </label>
              </li>
            ))}
          </ul>
          <p className={own.note} role="status">
            {t("campaigns.form.targeted", { count: targets.length })}
            {leftOut > 0 && ` ${t(appsOnly ? "campaigns.form.leftOutApps" : "campaigns.form.leftOut", { count: leftOut })}`}
          </p>
        </Fieldset>

        <Fieldset legend={t("campaigns.form.batches")}>
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
              {t("campaigns.form.addBatch")}
            </Button>
          </div>
          <WavePlan draft={draft} targets={targets} />
        </Fieldset>

        <Fieldset legend={t("campaigns.form.update")}>
          {!agentUpdate && (
            <div className={own.row} role="radiogroup" aria-label={t("campaigns.form.sources")}>
              {SOURCES.map(([sources, label]) => (
                <label key={sources} className={own.inline}>
                  <input type="radio" name="sources" checked={draft.sources === sources} onChange={() =>
                      set({ sources, ...(sources === "apps" && draft.update !== "agent" && { update: "all" as const }) })
                    }
                  />
                  {t(label)}
                </label>
              ))}
            </div>
          )}
          {!agentUpdate && draft.sources !== "os" && (
            <>
              <div className={own.row} role="group" aria-label={t("campaigns.form.managers")}>
                {THIRD_PARTY_MANAGERS.map((manager) => (
                  <label key={manager} className={own.inline}>
                    <input
                      type="checkbox"
                      checked={draft.managers.includes(manager)}
                      onChange={(e) =>
                        set({
                          managers: e.target.checked
                            ? THIRD_PARTY_MANAGERS.filter((m) => m === manager || draft.managers.includes(m))
                            : draft.managers.filter((m) => m !== manager),
                        })
                      }
                    />
                    {MANAGER_NAMES[manager]}
                  </label>
                ))}
              </div>
              <p className={own.note}>
                {t("campaigns.form.appsNote")} {draft.sources === "apps" && t("campaigns.form.appsOnlyNote")}
              </p>
            </>
          )}
          <div className={own.row} role="radiogroup" aria-label={t("campaigns.form.updateKind")}>
            {UPDATE_KINDS.filter(([kind]) => kind !== "agent" || agentVersions.length > 0)
              .filter(([kind]) => draft.sources !== "apps" || kind === "all" || kind === "agent")
              .map(([kind, label]) => (
                <label key={kind} className={own.inline}>
                  <input type="radio" name="update" checked={draft.update === kind} onChange={() => set({ update: kind })} />
                  {t(label)}
                </label>
              ))}
          </div>
          {draft.update === "agent" && (
            <label className={own.fld}>
              {t("campaigns.form.agentVersion")}
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
              {t("campaigns.form.packageList")}
              <textarea
                className={own.textarea}
                value={draft.packages}
                onChange={(e) => set({ packages: e.target.value })}
                rows={2}
                placeholder={t("campaigns.form.packagePlaceholder")}
              />
            </label>
          )}
          <div className={own.row}>
            {draft.update !== "agent" && (
              <Switch
                rowClassName={own.inline}
                checked={draft.allowReboot}
                onChange={(allowReboot) => set({ allowReboot })}
                label={t("campaigns.form.reboot")}
              />
            )}
            <label className={own.fld}>
              {t("campaigns.form.ttl")}
              <select className={fieldClass} value={draft.ttl} onChange={(e) => set({ ttl: Number(e.target.value) })}>
                {TTLS.map((seconds) => (
                  <option key={seconds} value={seconds}>
                    {formatDuration(seconds)}
                  </option>
                ))}
              </select>
            </label>
          </div>
        </Fieldset>

        <Fieldset legend={t("campaigns.form.windows")}>
          {draft.windows.length === 0 && <p className={own.note}>{t("campaigns.form.noWindow")}</p>}
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
            <Button onClick={() => set({ windows: [...draft.windows, newWindow("weekly")] })}>
              {t("campaigns.form.addWeekly")}
            </Button>
            <Button onClick={() => set({ windows: [...draft.windows, newWindow("once")] })}>
              {t("campaigns.form.addOnce")}
            </Button>
          </div>
        </Fieldset>

        <Fieldset legend={t("campaigns.form.threshold")}>
          <div className={own.row}>
            <label className={own.fld}>
              {t("campaigns.form.stopWhen")}
              <input
                className={fieldClass}
                type="number"
                min={0}
                value={draft.thresholdValue}
                onChange={(e) => set({ thresholdValue: e.target.value })}
              />
            </label>
            <label className={own.fld}>
              {t("campaigns.form.unit")}
              <select
                className={fieldClass}
                value={draft.thresholdUnit}
                onChange={(e) => set({ thresholdUnit: e.target.value as CampaignDraft["thresholdUnit"] })}
              >
                <option value="count">{t("campaigns.form.unitCount")}</option>
                <option value="percent">{t("campaigns.form.unitPercent")}</option>
              </select>
            </label>
          </div>
        </Fieldset>

        {submitted && errors.length > 0 && (
          <Banner tone="bad">
            <strong>{t("campaigns.form.fixFirst")}</strong>
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
            {t("campaigns.form.create")}
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
  const number = index + 1;
  const label = t("campaigns.wave.title", { number });
  return (
    <div className={own.row} role="group" aria-label={label}>
      <label className={own.fld}>
        {label}
        <select
          className={fieldClass}
          value={wave.kind}
          onChange={(e) => onChange({ ...wave, kind: e.target.value as WaveDraft["kind"] })}
        >
          <option value="size">{t("campaigns.form.bySize")}</option>
          <option value="percent">{t("campaigns.form.byPercent")}</option>
          <option value="servers">{t("campaigns.form.byServers")}</option>
        </select>
      </label>
      {wave.kind === "servers" ? (
        <label className={own.fld}>
          {t("campaigns.form.servers")}
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
          {wave.kind === "size" ? t("campaigns.form.servers") : t("campaigns.form.percent")}
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
      <Button onClick={onRemove} aria-label={t("campaigns.form.removeBatch", { number })}>
        {t("campaigns.form.remove")}
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
  if ("error" in plan) return <p className={own.note}>{t("campaigns.form.cannotPlan", { error: plan.error })}</p>;
  const sizes = plan.waves.map((w, i) => t("campaigns.form.planBatch", { number: i + 1, count: w.length })).join(", ");
  return <p className={own.note}>{t("campaigns.form.plan", { sizes })}</p>;
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
  const number = index + 1;
  const label = t("campaigns.check.window", { number });
  const set = (patch: Partial<WindowDraft>) => onChange({ ...window, ...patch });
  return (
    <div className={own.row} role="group" aria-label={label}>
      {window.kind === "weekly" ? (
        <>
          {WEEKDAYS.map((day) => (
            <label key={day} className={own.inline}>
              <input
                type="checkbox"
                checked={window.weekdays.includes(day)}
                onChange={(e) => set({ weekdays: toggle(window.weekdays, day, e.target.checked) })}
              />
              {weekdayName(day)}
            </label>
          ))}
          <label className={own.fld}>
            {t("campaigns.form.from")}
            <input className={fieldClass} type="time" value={window.start} onChange={(e) => set({ start: e.target.value })} />
          </label>
          <label className={own.fld}>
            {t("campaigns.form.to")}
            <input className={fieldClass} type="time" value={window.end} onChange={(e) => set({ end: e.target.value })} />
          </label>
          <label className={own.fld}>
            {t("campaigns.form.timezone")}
            <input className={fieldClass} value={window.timezone} onChange={(e) => set({ timezone: e.target.value })} />
          </label>
        </>
      ) : (
        <>
          <label className={own.fld}>
            {t("campaigns.form.starts")}
            <input
              className={fieldClass}
              type="datetime-local"
              value={window.startsAt}
              onChange={(e) => set({ startsAt: e.target.value })}
            />
          </label>
          <label className={own.fld}>
            {t("campaigns.form.ends")}
            <input
              className={fieldClass}
              type="datetime-local"
              value={window.endsAt}
              onChange={(e) => set({ endsAt: e.target.value })}
            />
          </label>
        </>
      )}
      <Button onClick={onRemove} aria-label={t("campaigns.form.removeWindow", { number })}>
        {t("campaigns.form.remove")}
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
