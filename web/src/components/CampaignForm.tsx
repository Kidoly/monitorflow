"use client";

import Link from "next/link";
import { useRouter, useSearchParams } from "next/navigation";
import { type FormEvent, type ReactNode, useEffect, useState } from "react";

import { type PlainKey, t } from "@/i18n";
import {
  ApiError,
  type CampaignDetail,
  type Customer,
  type Me,
  type Server,
  type ServerUpgrade,
  type Site,
  type SoftwareEntry,
  api,
  describeError,
  isAbortError,
} from "@/lib/api";
import {
  type CampaignDraft,
  type UpdateKind,
  type WaveDraft,
  MANAGER_NAMES,
  THIRD_PARTY_MANAGERS,
  WINDOWS_CLASSIFICATIONS,
  campaignTargets,
  classificationName,
  copyName,
  draftFromCampaign,
  editMode,
  includesApps,
  includesOs,
  managerFits,
  emptyDraft,
  newWindow,
  planWaves,
  requestFromCampaign,
  toRequest,
  updateSources,
  validateCampaign,
  MAJOR_UPGRADES,
  MAX_PARALLEL,
  releaseLabel,
  runsRelease,
  softwareFamilies,
} from "@/lib/campaigns";
import { formatDuration } from "@/lib/format";
import { administersCustomer, multiCustomer, multiSite, useMe } from "@/lib/me";
import { CAMPAIGNS_PATH, SOFTWARE_CATALOG_PATH, campaignHref } from "@/lib/routes";
import { UUID } from "@/lib/uuid";
import { Switch } from "@/components/ui/Switch";
import { SegmentedControl } from "@/components/ui/SegmentedControl";

import { CampaignServerPicker } from "./CampaignServerPicker";
import { MajorUpgradeChecks } from "./MajorUpgrade";
import { CampaignWindowCard } from "./CampaignWindowCard";
import own from "./Campaigns.module.css";
import { Breadcrumb } from "./ui/Breadcrumb";
import { Button, ButtonLink, fieldClass } from "./ui/Button";
import { Banner, EmptyState, LoadingBlock, NoPermission, NotFound, PageHeader } from "./ui/PageState";

/** Command validity choices; the API takes 60 s to 7 days. */
const TTLS = [900, 3600, 4 * 3600, 86_400, 604_800];

const UPDATE_KINDS: [UpdateKind, PlainKey][] = [
  ["all", "campaigns.update.all"],
  ["security", "campaigns.form.security"],
  ["packages", "campaigns.form.packages"],
  ["agent", "campaigns.form.agent"],
  ["major", "campaigns.form.major"],
  ["software", "campaigns.form.software"],
];

/** `/campaigns/new/`; `?edit=<id>` edits a campaign, `?from=<id>` starts a copy of one (ADR-0179). */
export function NewCampaignPage() {
  const me = useMe();
  const params = useSearchParams();
  const editId = params.get("edit");
  const sourceId = editId ?? params.get("from");
  if (!me || !administersCustomer(me)) {
    return (
      <NoPermission title={t("campaigns.form.adminTitle")}>
        {multiCustomer(me) ? t("campaigns.form.adminCustomer") : t("campaigns.form.adminSites")}
      </NoPermission>
    );
  }
  if (sourceId !== null) {
    return <FromCampaign key={sourceId} id={sourceId} edit={editId !== null} me={me} />;
  }
  // From a server's Updates tab: a major upgrade of that server (ADR-0182).
  const major = params.get("major");
  if (major !== null && UUID.test(major)) {
    return <CampaignForm me={me} initial={{ ...emptyDraft(), update: "major", serverIds: [major] }} />;
  }
  return <CampaignForm me={me} />;
}

/** The form filled with the campaign `id`, to edit it or to create a copy. */
function FromCampaign({ id, edit, me }: { id: string; edit: boolean; me: Me }) {
  const valid = UUID.test(id);
  /** Undefined until loaded, null when not found. */
  const [campaign, setCampaign] = useState<CampaignDetail | null | undefined>(undefined);
  const [error, setError] = useState<string | null>(null);
  useEffect(() => {
    if (!valid) return;
    const controller = new AbortController();
    api.campaign(id, controller.signal).then(setCampaign, (e: unknown) => {
      if (isAbortError(e)) return;
      if (e instanceof ApiError && e.status === 404) setCampaign(null);
      else setError(describeError(e));
    });
    return () => controller.abort();
  }, [id, valid]);

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
    return error ? <Banner tone="bad">{error}</Banner> : <LoadingBlock label={t("campaigns.form.loadingCampaign")} />;
  }
  if (edit && editMode(campaign) === "busy") {
    return (
      <EmptyState actions={<ButtonLink href={campaignHref(campaign.id)}>{campaign.name}</ButtonLink>}>
        <strong>{t("campaigns.form.busyTitle")}</strong> {t("campaigns.form.busy", { name: campaign.name })}
      </EmptyState>
    );
  }
  const initial = draftFromCampaign(campaign);
  if (!edit) initial.name = copyName(campaign.name);
  return <CampaignForm me={me} initial={initial} source={campaign} edit={edit} />;
}

interface Inventory {
  customers: Customer[];
  sites: Site[];
  servers: Server[];
}

export function CampaignForm({
  me,
  initial,
  source,
  edit = false,
}: {
  me: Me;
  /** The draft to start from; empty by default. */
  initial?: CampaignDraft;
  /** The campaign edited (with `edit`) or copied. */
  source?: CampaignDetail;
  edit?: boolean;
}) {
  const router = useRouter();
  const [inventory, setInventory] = useState<Inventory | null>(null);
  // Agent versions the server holds (ADR-0053); none when unreadable.
  const [agentVersions, setAgentVersions] = useState<string[]>([]);
  const [draft, setDraft] = useState<CampaignDraft>(() => initial ?? emptyDraft());
  const [submitted, setSubmitted] = useState(false);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  // The software catalogue of the customer, read once a software campaign is chosen (ADR-0188).
  const [catalog, setCatalog] = useState<{ customerId: string; software: SoftwareEntry[] } | null>(null);
  const [catalogError, setCatalogError] = useState<string | null>(null);
  const softwareUpdate = draft.update === "software";

  useEffect(() => {
    if (!softwareUpdate || !draft.customerId) return;
    const controller = new AbortController();
    const customerId = draft.customerId;
    api.softwareCatalog(customerId, controller.signal).then(
      (list) => {
        setCatalog({ customerId, software: list.software });
        setCatalogError(null);
      },
      (e: unknown) => {
        if (!isAbortError(e)) setCatalogError(describeError(e));
      },
    );
    return () => controller.abort();
  }, [softwareUpdate, draft.customerId]);

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
  const mode = edit && source ? editMode(source) : null;
  // A run under way keeps its servers, batches and update (ADR-0173).
  const locked = mode === "running";

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
  const major = draft.update === "major";
  const agentUpdate = draft.update === "agent";
  const wholeSystem = agentUpdate || major;
  const appsOnly = !wholeSystem && !softwareUpdate && draft.sources === "apps" ? draft.managers : null;
  // Software goes to the systems of the managers of the entries chosen (ADR-0188).
  const chosenManagers = [
    ...(catalog?.software ?? []).filter((e) => draft.softwareIds.includes(e.id)).map((e) => e.manager),
    ...(source?.software?.packages ?? []).filter((p) => draft.softwareIds.includes(p.catalog_id)).map((p) => p.manager),
  ];
  const families = softwareFamilies(chosenManagers);
  const targets = campaignTargets(
    inventory.servers,
    draft.customerId,
    draft.siteId,
    draft.serverIds,
    agentUpdate,
    appsOnly,
    softwareUpdate && families,
  ).filter((s) => !major || runsRelease(s.os_version, draft.majorFrom));
  const noPackages = (s: Server) =>
    major
      ? !runsRelease(s.os_version, draft.majorFrom)
      : softwareUpdate
        ? !families.includes(s.os_family)
        : appsOnly
          ? !appsOnly.some((m) => managerFits(m, s.os_family))
          : !agentUpdate && s.package_updates === false;
  // OS updates of Windows servers take approved classifications and held updates (ADR-0188).
  const osUpdate = !wholeSystem && !softwareUpdate && includesOs(draft.sources);
  const wingetApps = !wholeSystem && !softwareUpdate && includesApps(draft.sources) && draft.managers.includes("winget");
  const leftOut = servers.filter((s) => noPackages(s) && (s.site_id === draft.siteId || draft.serverIds.includes(s.id))).length;
  const request =
    locked && source
      ? {
          ...requestFromCampaign(source, draft),
          name: draft.name,
          windows: toRequest(draft).windows,
          failure_threshold: toRequest(draft).failure_threshold,
          // Changes while a run is under way, like the windows (ADR-0188).
          ...((draft.recurring || source.recurring) && { recurring: draft.recurring && !source.major_upgrade }),
        }
      : toRequest(draft);
  // Locked, the batches stay those frozen: check them against the servers they hold.
  const frozen = source?.waves.flatMap((w) => w.servers.map((s) => s.server_id)) ?? [];
  const errors = validateCampaign(request, locked ? frozen : targets.map((t) => t.id));

  async function submit(event: FormEvent) {
    event.preventDefault();
    setSubmitted(true);
    setError(null);
    if (errors.length > 0) return;
    setBusy(true);
    try {
      const saved = edit && source ? await api.updateCampaign(source.id, request) : await api.createCampaign(request);
      router.push(campaignHref(saved.id));
    } catch (e) {
      setError(t(edit ? "campaigns.form.saveFailed" : "campaigns.form.createFailed", { error: describeError(e) }));
      setBusy(false);
    }
  }

  const title = edit && source ? t("campaigns.form.editTitle", { name: source.name }) : t("campaigns.newCampaign");
  const meta = edit
    ? mode === "running"
      ? t("campaigns.form.runningMeta")
      : mode === "next" && source
        ? t("campaigns.form.nextMeta", { name: source.name })
        : t("campaigns.form.editMeta")
    : source
      ? t("campaigns.form.copyMeta", { name: source.name })
      : t("campaigns.form.meta");

  return (
    <div className={own.page}>
      <Breadcrumb
        parents={[
          { label: t("campaigns.label"), href: CAMPAIGNS_PATH },
          ...(source ? [{ label: source.name, href: campaignHref(source.id) }] : []),
        ]}
      />
      <PageHeader detail title={title} meta={meta} />
      <form className={own.form} onSubmit={submit} aria-label={title} noValidate>
        <div className={own.grid}>
          {multiCustomer(me) && (
            <label className={own.fld}>
              {t("campaigns.fact.customer")}
              <select
                className={fieldClass}
                value={draft.customerId}
                disabled={locked}
                onChange={(e) => set({ customerId: e.target.value, siteId: "", serverIds: [], waves: [], softwareIds: [] })}
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

        <Fieldset legend={t("campaigns.form.servers")} locked={locked}>
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
          <CampaignServerPicker
            servers={servers}
            siteId={draft.siteId}
            selected={draft.serverIds}
            onChange={(serverIds) => set({ serverIds })}
            unavailable={(s) =>
              noPackages(s)
                ? major
                  ? t("campaigns.major.notOnRelease", { release: releaseLabel(draft.majorFrom) })
                  : t(softwareUpdate ? "campaigns.form.notWindows" : "campaigns.form.noPackages")
                : null
            }
            showSite={multiSite(me)}
          />
          <p className={own.note} role="status">
            {t("campaigns.form.targeted", { count: targets.length })}
            {leftOut > 0 &&
              ` ${t(
                softwareUpdate
                  ? "campaigns.form.leftOutSoftware"
                  : appsOnly
                    ? "campaigns.form.leftOutApps"
                    : "campaigns.form.leftOut",
                { count: leftOut },
              )}`}
          </p>
        </Fieldset>

        <Fieldset legend={t("campaigns.form.batches")} locked={locked}>
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

        <Fieldset legend={t("campaigns.form.update")} locked={locked}>
          {!wholeSystem && !softwareUpdate && (
            <div className={own.row} role="group" aria-label={t("campaigns.form.sources")}>
              <Switch
                checked={includesOs(draft.sources)}
                onChange={(checked) => {
                  const sources = updateSources(checked, includesApps(draft.sources));
                  // Security updates and package lists are the OS's.
                  set({ sources, ...(!includesOs(sources) && { update: "all" as const }) });
                }}
                label={t("campaigns.form.sources.os")}
              />
              <Switch
                checked={includesApps(draft.sources)}
                onChange={(checked) => set({ sources: updateSources(includesOs(draft.sources), checked) })}
                label={t("campaigns.form.sources.apps")}
              />
            </div>
          )}
          {!wholeSystem && !softwareUpdate && includesApps(draft.sources) && (
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
          <SegmentedControl
            label={t("campaigns.form.updateKind")}
            options={UPDATE_KINDS.filter(([kind]) => kind !== "agent" || agentVersions.length > 0)
              .filter(
                ([kind]) =>
                  includesOs(draft.sources) || kind === "all" || kind === "agent" || kind === "major" || kind === "software",
              )
              .map(([value, label]) => ({ value, label: t(label) }))}
            value={draft.update}
            onChange={(update) =>
              // A major upgrade is of the OS; back from software, OS updates as by default.
              set({
                update,
                ...((update === "major" || (draft.update === "software" && update !== "software")) && {
                  sources: "os" as const,
                }),
              })
            }
          />
          {major && (
            <MajorFields
              draft={draft}
              targets={targets}
              onChange={set}
            />
          )}
          {softwareUpdate && (
            <SoftwareChoice
              draft={draft}
              set={set}
              catalog={catalog?.customerId === draft.customerId ? catalog.software : null}
              error={catalogError}
              source={source}
            />
          )}
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
          {wingetApps && (
            <label className={own.fld}>
              {t("campaigns.form.appHold")}
              <textarea
                className={own.textarea}
                value={draft.appHold}
                onChange={(e) => set({ appHold: e.target.value })}
                rows={1}
                placeholder={t("campaigns.form.appHoldPlaceholder")}
                spellCheck={false}
              />
            </label>
          )}
          {osUpdate && <WindowsApprovals draft={draft} set={set} />}
          <div className={own.row}>
            {!wholeSystem && (
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
                {[...new Set([...TTLS, draft.ttl])].sort((a, b) => a - b).map((seconds) => (
                  <option key={seconds} value={seconds}>
                    {formatDuration(seconds)}
                  </option>
                ))}
              </select>
            </label>
          </div>
          <p className={own.note}>{t("campaigns.form.ttlNote")}</p>
        </Fieldset>

        <Fieldset legend={t("campaigns.form.windows")}>
          {draft.windows.length === 0 && <p className={own.note}>{t("campaigns.form.noWindow")}</p>}
          {draft.windows.map((window, i) => (
            <CampaignWindowCard
              key={i}
              index={i}
              window={window}
              onChange={(w) => set({ windows: replace(draft.windows, i, w) })}
              onRemove={() => set({ windows: draft.windows.filter((_, j) => j !== i) })}
            />
          ))}
          <div className={own.row}>
            <Button onClick={() => set({ windows: [...draft.windows, newWindow("weekly")] })}>
              {t("campaigns.form.addWindow")}
            </Button>
          </div>
          {!major && (
            <>
              <Switch
                rowClassName={own.inline}
                checked={draft.recurring}
                onChange={(recurring) => set({ recurring })}
                label={t("campaigns.form.recurring")}
              />
              <p className={own.note}>{t("campaigns.form.recurringNote")}</p>
            </>
          )}
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
            {edit ? t("campaigns.form.save") : t("campaigns.form.create")}
          </Button>
        </div>
      </form>
    </div>
  );
}

/** Install or remove, and which entries of the customer's software catalogue (ADR-0188). */
function SoftwareChoice({
  draft,
  set,
  catalog,
  error,
  source,
}: {
  draft: CampaignDraft;
  set: (patch: Partial<CampaignDraft>) => void;
  /** Null while loading. */
  catalog: SoftwareEntry[] | null;
  error: string | null;
  source?: CampaignDetail;
}) {
  const toggle = (id: string, on: boolean) =>
    set({ softwareIds: on ? [...draft.softwareIds.filter((x) => x !== id), id] : draft.softwareIds.filter((x) => x !== id) });
  // Entries the campaign holds that were deleted from the catalogue since: shown, so they can be unticked.
  const gone = (source?.software?.packages ?? []).filter(
    (p) => draft.softwareIds.includes(p.catalog_id) && catalog !== null && !catalog.some((e) => e.id === p.catalog_id),
  );
  return (
    <>
      <SegmentedControl
        label={t("campaigns.form.softwareAction")}
        options={[
          { value: "install" as const, label: t("campaigns.form.install") },
          { value: "remove" as const, label: t("campaigns.form.uninstall") },
        ]}
        value={draft.softwareAction}
        onChange={(softwareAction) => set({ softwareAction })}
      />
      <p className={own.note}>{t("campaigns.form.softwareNote")}</p>
      {error ? (
        <Banner tone="bad">{t("campaigns.form.softwareFailed", { error })}</Banner>
      ) : catalog === null ? (
        <LoadingBlock label={t("campaigns.form.softwareLoading")} />
      ) : catalog.length === 0 && gone.length === 0 ? (
        <p className={own.note}>
          {t("campaigns.form.softwareEmpty")} <Link href={SOFTWARE_CATALOG_PATH}>{t("campaigns.software.link")}</Link>
        </p>
      ) : (
        <div className={own.fld} role="group" aria-label={t("campaigns.form.softwareList")}>
          {t("campaigns.form.softwareList")}
          <ul className={own.softwareList}>
            {catalog.map((entry) => (
              <li key={entry.id}>
                <label className={own.inline}>
                  <input
                    type="checkbox"
                    checked={draft.softwareIds.includes(entry.id)}
                    onChange={(e) => toggle(entry.id, e.target.checked)}
                  />
                  {entry.name}
                  <span className={own.dim}>
                    {entry.package_id} · {entry.manager}
                    {entry.manager === "winget" ? ` · ${entry.source}` : entry.mirror ? ` · ${entry.mirror}` : ""}
                  </span>
                </label>
              </li>
            ))}
            {gone.map((p) => (
              <li key={p.catalog_id}>
                <label className={own.inline}>
                  <input type="checkbox" checked onChange={(e) => toggle(p.catalog_id, e.target.checked)} />
                  {p.name}
                  <span className={own.dim}>
                    {p.id} · {t("campaigns.form.softwareGone")}
                  </span>
                </label>
              </li>
            ))}
          </ul>
          <span className={own.note}>
            <Link href={SOFTWARE_CATALOG_PATH}>{t("campaigns.software.link")}</Link>
          </span>
        </div>
      )}
    </>
  );
}

/** Approved Windows Update classifications and held updates (ADR-0188); Windows servers only. */
function WindowsApprovals({ draft, set }: { draft: CampaignDraft; set: (patch: Partial<CampaignDraft>) => void }) {
  // Classifications narrow all or security updates, not named packages: the API refuses them together.
  const classifications = draft.update !== "packages";
  return (
    <div className={own.subgroup} role="group" aria-label={t("campaigns.form.windowsUpdate")}>
      <strong className={own.subtitle}>{t("campaigns.form.windowsUpdate")}</strong>
      <p className={own.note}>{t("campaigns.form.windowsOnly")}</p>
      {classifications && (
        <div className={own.fld} role="group" aria-label={t("campaigns.form.classifications")}>
          {t("campaigns.form.classifications")}
          <div className={own.row}>
            {WINDOWS_CLASSIFICATIONS.map((name) => (
              <label key={name} className={own.inline}>
                <input
                  type="checkbox"
                  checked={draft.classifications.includes(name)}
                  onChange={(e) =>
                    set({
                      classifications: e.target.checked
                        ? WINDOWS_CLASSIFICATIONS.filter((c) => c === name || draft.classifications.includes(c))
                        : draft.classifications.filter((c) => c !== name),
                    })
                  }
                />
                {classificationName(name)}
              </label>
            ))}
          </div>
          {draft.classifications.length === 0 && (
            <span className={own.note}>{t("campaigns.form.everyClassification")}</span>
          )}
        </div>
      )}
      <label className={own.fld}>
        {t("campaigns.form.hold")}
        <textarea
          className={own.textarea}
          value={draft.hold}
          onChange={(e) => set({ hold: e.target.value })}
          rows={1}
          placeholder={t("campaigns.form.holdPlaceholder")}
          spellCheck={false}
        />
      </label>
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

/** A group of fields; `locked` disables them all and says why. */
function Fieldset({ legend, locked = false, children }: { legend: string; locked?: boolean; children: ReactNode }) {
  return (
    <fieldset className={own.fieldset} disabled={locked}>
      <legend>{legend}</legend>
      <div className={own.fields}>
        {locked && <p className={own.note}>{t("campaigns.form.locked")}</p>}
        {children}
      </div>
    </fieldset>
  );
}

function replace<T>(list: T[], index: number, item: T): T[] {
  return list.map((x, i) => (i === index ? item : x));
}

/**
 * The fields of a major upgrade (ADR-0182): the releases, how many servers of a batch at once,
 * the checks and rollback of every server targeted, and the box for servers without a snapshot.
 */
function MajorFields({
  draft,
  targets,
  onChange,
}: {
  draft: CampaignDraft;
  targets: Server[];
  onChange: (patch: Partial<CampaignDraft>) => void;
}) {
  const [servers, setServers] = useState<ServerUpgrade[] | null>(null);
  const [error, setError] = useState<string | null>(null);
  const customerId = draft.customerId;
  useEffect(() => {
    const controller = new AbortController();
    api.majorUpgrades({ customer_id: customerId }, controller.signal).then(
      (list) => setServers(list.servers),
      (e: unknown) => {
        if (!isAbortError(e)) setError(describeError(e));
      },
    );
    return () => controller.abort();
  }, [customerId]);
  const ids = new Set(targets.map((s) => s.id));
  const shown = (servers ?? []).filter((s) => ids.has(s.server_id));
  const limited = shown.filter((s) => !s.rollback.available).length;
  return (
    <>
      <div className={own.row}>
        <label className={own.fld}>
          {t("campaigns.form.majorRelease")}
          <select className={fieldClass} value={draft.majorFrom} onChange={(e) => onChange({ majorFrom: e.target.value })}>
            {MAJOR_UPGRADES.map((u) => (
              <option key={u.from} value={u.from}>
                {t("campaigns.form.majorPair", { from: u.fromLabel, to: u.toLabel })}
              </option>
            ))}
          </select>
        </label>
        <label className={own.fld}>
          {t("campaigns.form.parallel")}
          <input
            className={fieldClass}
            type="number"
            min={1}
            max={MAX_PARALLEL}
            value={draft.parallel}
            onChange={(e) => onChange({ parallel: e.target.value })}
          />
        </label>
      </div>
      <p className={own.note}>{t("campaigns.form.majorNote")}</p>
      {error && <Banner tone="bad">{error}</Banner>}
      {!servers && !error && <LoadingBlock label={t("campaigns.major.loading")} />}
      {servers && shown.length === 0 && <p className={own.note}>{t("campaigns.major.noServer", { release: releaseLabel(draft.majorFrom) })}</p>}
      {shown.length > 0 && <MajorUpgradeChecks servers={shown} />}
      {limited > 0 && (
        <>
          <Banner tone="warn">{t("campaigns.major.limited", { count: limited })}</Banner>
          <Switch
            rowClassName={own.inline}
            checked={draft.acceptLimited}
            onChange={(acceptLimited) => onChange({ acceptLimited })}
            label={t("campaigns.form.acceptLimited")}
          />
        </>
      )}
    </>
  );
}
