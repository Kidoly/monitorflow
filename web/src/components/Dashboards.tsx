"use client";

import Link from "next/link";
import { useRouter, useSearchParams } from "next/navigation";
import { type DragEvent, type FormEvent, useCallback, useEffect, useMemo, useState } from "react";

import {
  ApiError,
  type Dashboard,
  type DashboardSpec,
  type DashboardSummary,
  type Panel,
  type Server,
  type Sharing,
  type TimeRangeLabel,
  type Within,
  api,
  describeError,
  isUnauthorized,
} from "@/lib/api";
import {
  DASHBOARD_REFRESH_MS,
  MAX_NAME,
  MAX_PANELS,
  RANGES,
  copyName,
  copySpec,
  dropMissingServers,
  isRange,
  movePanel,
  newPanel,
  placePanel,
  placesOf,
  rangeLabel,
  readyMade,
  readyMades,
  serversWithin,
  sharingLabel,
  widthLabel,
  withinOf,
} from "@/lib/dashboards";
import { t } from "@/i18n";
import { displayHref } from "@/lib/display";
import { modifiesCustomer, modifiesSite, multiCustomer, useMe, useMultiCustomer } from "@/lib/me";
import { DASHBOARDS_PATH, dashboardHref, readyMadeHref } from "@/lib/routes";
import { scopedHref, useScope } from "@/lib/scope";
import { usePolledData } from "@/lib/usePolledData";
import { useUrlParams } from "@/lib/urlState";
import { UUID } from "@/lib/uuid";
import { Breadcrumb } from "@/components/ui/Breadcrumb";
import { Button, ButtonLink, fieldClass } from "@/components/ui/Button";
import { type Column, DataTable, PrimaryCell } from "@/components/ui/DataTable";
import { Page } from "@/components/ui/Page";
import { Banner, EmptyState, ErrorState, LoadingBlock, NotFound, PageHeader, RefreshLine, StaleBanner } from "@/components/ui/PageState";
import { Label } from "@/components/ui/StatusLabel";
import { formatDay } from "@/components/ui/time";

import admin from "./AdminPages.module.css";
import { DashboardPanel } from "./DashboardPanel";
import { DisplayLinks } from "./DisplayLinks";
import styles from "./Dashboards.module.css";
import { PanelForm } from "./PanelForm";

/** `/dashboards/`: the ready-made dashboards and those the user sees. */
export function DashboardsPage() {
  const scope = useScope();
  const multi = useMultiCustomer();
  const fetcher = useCallback((signal: AbortSignal) => api.dashboards(signal), []);
  const { data, error, stale, failures, updatedAt, refresh } = usePolledData(fetcher, { intervalMs: DASHBOARD_REFRESH_MS });
  const columns: Column<DashboardSummary>[] = [
    {
      key: "name",
      header: t("dashboards.list.name"),
      cell: (d) => (
        <PrimaryCell
          title={d.name}
          href={scopedHref(dashboardHref(d.id), scope)}
          sub={t("dashboards.list.panels", { count: d.panels })}
        />
      ),
      sortValue: (d) => d.name,
    },
    { key: "shared", header: t("dashboards.list.shared"), cell: (d) => sharingLabel(d.shared_with, multi), sortValue: (d) => sharingLabel(d.shared_with, multi) },
    {
      key: "updated",
      header: t("dashboards.list.changed"),
      cell: (d) => (
        <>
          {formatDay(new Date(d.updated_at), new Date())}
          {d.updated_by_username && <span className={admin.dim}>{t("dashboards.list.by", { name: d.updated_by_username })}</span>}
        </>
      ),
      sortValue: (d) => d.updated_at,
    },
  ];
  const newHref = scopedHref(`${DASHBOARDS_PATH}view/?new=1`, scope);
  return (
    <Page>
      <PageHeader
        title={t("dashboards.list.title")}
        meta={<RefreshLine updatedAt={updatedAt} stale={stale} intervalMs={DASHBOARD_REFRESH_MS} />}
        actions={
          <ButtonLink variant="k" href={newHref}>
            {t("dashboards.list.new")}
          </ButtonLink>
        }
      />
      {stale && updatedAt && <StaleBanner error={describeError(error)} updatedAt={updatedAt} failures={failures} onRetry={refresh} />}
      <section className={admin.section} aria-labelledby="ready-made">
        <div className={admin.sectionHead}>
          <h2 id="ready-made" className={admin.sectionTitle}>
            {t("dashboards.list.readyMade")}
          </h2>
          <span className={admin.dim}>{t("dashboards.list.readyNote")}</span>
        </div>
        <ul className={styles.cards}>
          {readyMades().map((d) => (
            <li key={d.id}>
              <Link href={scopedHref(readyMadeHref(d.id), scope)} className={styles.card}>
                <b>{d.name}</b>
                <span className={admin.dim}>{d.description}</span>
              </Link>
            </li>
          ))}
        </ul>
      </section>
      {!data ? (
        error !== null && !isUnauthorized(error) ? (
          <ErrorState what={t("dashboards.list.what")} error={describeError(error)} onRetry={refresh} />
        ) : (
          <LoadingBlock label={t("dashboards.list.loading")} />
        )
      ) : data.dashboards.length === 0 ? (
        <EmptyState
          title={t("dashboards.list.none")}
          actions={
            <ButtonLink variant="k" href={newHref}>
              {t("dashboards.list.new")}
            </ButtonLink>
          }
        >
          {t("dashboards.list.noneText")}
        </EmptyState>
      ) : (
        <DataTable label={t("dashboards.list.title")} columns={columns} rows={data.dashboards} rowKey={(d) => d.id} />
      )}
    </Page>
  );
}

/** The page of one dashboard: `?id=`, `?ready=` or `?new=1`. */
export function DashboardView() {
  const params = useSearchParams();
  const id = params.get("id");
  const ready = params.get("ready");
  const isNew = params.get("new") === "1";
  return <DashboardPage key={`${id}/${ready}/${isNew}`} id={id} ready={ready} isNew={isNew} />;
}

/** What the editor changes before Save. */
interface Draft {
  name: string;
  shared_with: Sharing;
  spec: DashboardSpec;
}

const EMPTY: Draft = { name: "", shared_with: { kind: "personal" }, spec: { version: 1, range: "6h", panels: [] } };

/** Where a dashboard can be shared, for `me`: only me, then what it edits. */
function sharingOptions(me: ReturnType<typeof useMe>, servers: Server[], current: Sharing): { value: string; label: string; sharing: Sharing }[] {
  const { sites, customers } = placesOf(servers);
  const options: { value: string; label: string; sharing: Sharing }[] = [
    { value: "personal", label: t("dashboards.sharing.personal"), sharing: { kind: "personal" } },
  ];
  for (const c of customers) {
    if (modifiesCustomer(me, c.id)) options.push({ value: `customer:${c.id}`, label: multiCustomer(me) ? t("dashboards.sharing.customer", { name: c.name }) : t("dashboards.sharing.allSites"), sharing: { kind: "customer", id: c.id } });
  }
  for (const s of sites) {
    if (modifiesSite(me, s.customerId, s.id)) options.push({ value: `site:${s.id}`, label: t("dashboards.sharing.site", { name: s.name }), sharing: { kind: "site", id: s.id } });
  }
  const value = sharingValue(current);
  if (!options.some((o) => o.value === value)) options.push({ value, label: sharingLabel(current, multiCustomer(me)), sharing: current });
  return options;
}

const sharingValue = (s: Sharing) => (s.kind === "personal" ? "personal" : `${s.kind}:${s.id}`);

function DashboardPage({ id, ready, isNew }: { id: string | null; ready: string | null; isNew: boolean }) {
  const me = useMe();
  const router = useRouter();
  const scope = useScope();
  const [params, setParams] = useUrlParams();
  const builtIn = readyMade(ready);
  const valid = isNew || builtIn !== undefined || (id !== null && UUID.test(id));

  const [saved, setSaved] = useState<Dashboard | null>(null);
  const [loadError, setLoadError] = useState<unknown>(null);
  const [draft, setDraft] = useState<Draft | null>(isNew ? EMPTY : null);
  const [servers, setServers] = useState<Server[] | null>(null);
  const [editing, setEditing] = useState<number | "new" | null>(null);
  const [busy, setBusy] = useState(false);
  const [message, setMessage] = useState<{ tone: "bad" | "info"; text: string } | null>(null);
  const [confirmDelete, setConfirmDelete] = useState(false);
  const [dragged, setDragged] = useState<number | null>(null);

  useEffect(() => {
    if (!id || !UUID.test(id) || isNew) return;
    const controller = new AbortController();
    api.dashboard(id, controller.signal).then(setSaved, (e: unknown) => {
      if (!controller.signal.aborted) setLoadError(e);
    });
    return () => controller.abort();
  }, [id, isNew]);

  // Servers for the editor's lists, once editing starts.
  const isEditing = draft !== null;
  useEffect(() => {
    if (!isEditing || servers) return;
    const controller = new AbortController();
    api.servers(controller.signal).then(
      (list) => setServers(list.servers),
      () => {
        if (!controller.signal.aborted) setServers([]);
      },
    );
    return () => controller.abort();
  }, [isEditing, servers]);

  // The editor takes chosen servers that are gone out of the panels, and says so.
  const known = useMemo(
    () => (servers && servers.length > 0 ? new Set(servers.filter((s) => !s.revoked_at).map((s) => s.id)) : null),
    [servers],
  );
  const cleaned = draft && known ? dropMissingServers(draft.spec, known) : null;
  const editDraft: Draft | null = draft && cleaned ? { ...draft, spec: cleaned.spec } : draft;

  const shown: Draft | null = editDraft ?? (saved ? { name: saved.name, shared_with: saved.shared_with, spec: saved.spec } : null) ??
    (builtIn ? { name: builtIn.name, shared_with: { kind: "personal" }, spec: builtIn.spec } : null);
  const withinKey = JSON.stringify(
    builtIn
      ? scope.customerId
        ? { kind: "customer", id: scope.customerId }
        : { kind: "all" }
      : shown
        ? withinOf(shown.shared_with)
        : { kind: "all" },
  );
  const within = useMemo(() => JSON.parse(withinKey) as Within, [withinKey]);
  const urlRange = params.get("range");
  const range: TimeRangeLabel = isRange(urlRange) ? urlRange : (shown?.spec.range ?? "6h");
  const panels = shown?.spec.panels ?? [];
  const request = JSON.stringify({ range, within, panels });
  const fetcher = useCallback(
    (signal: AbortSignal) => api.dashboardData(JSON.parse(request) as Parameters<typeof api.dashboardData>[0], signal),
    [request],
  );
  const { data, error, stale, failures, updatedAt, refresh } = usePolledData(fetcher, {
    intervalMs: DASHBOARD_REFRESH_MS,
    enabled: valid && shown !== null && panels.length > 0,
  });

  useEffect(() => {
    if (shown?.name) document.title = `${shown.name} · MonitorFlow`;
  }, [shown?.name]);

  const choosable = useMemo(() => (servers ? serversWithin(servers, within) : []), [servers, within]);

  if (!valid || (loadError instanceof ApiError && loadError.status === 404)) {
    return (
      <NotFound
        title={t("dashboards.view.notFound")}
        actions={<ButtonLink href={scopedHref(DASHBOARDS_PATH, scope)}>{t("dashboards.view.back")}</ButtonLink>}
      />
    );
  }
  if (!shown) {
    return loadError !== null && !isUnauthorized(loadError) ? (
      <ErrorState what={t("dashboards.view.what")} error={describeError(loadError)} />
    ) : (
      <LoadingBlock label={t("dashboards.view.loading")} />
    );
  }

  const canEdit = saved?.can_edit ?? isNew;
  const byId = new Map(data?.panels.map((p) => [p.id, p]));

  function setPanels(next: Panel[]) {
    if (editDraft) setDraft({ ...editDraft, spec: { ...editDraft.spec, panels: next } });
  }

  function startEditing() {
    if (!saved) return;
    setDraft({ name: saved.name, shared_with: saved.shared_with, spec: saved.spec });
    setMessage(null);
  }

  function cancel() {
    if (isNew) return router.push(scopedHref(DASHBOARDS_PATH, scope));
    setDraft(null);
    setEditing(null);
    setMessage(null);
  }

  async function save(event: FormEvent) {
    event.preventDefault();
    if (!editDraft) return;
    if (!editDraft.name.trim()) return setMessage({ tone: "bad", text: t("dashboards.view.nameRequired") });
    setBusy(true);
    setMessage(null);
    try {
      const body = { name: editDraft.name.trim(), shared_with: editDraft.shared_with, spec: editDraft.spec };
      const result = saved ? await api.updateDashboard(saved.id, { ...body, revision: saved.revision }) : await api.createDashboard(body);
      setSaved(result);
      setDraft(null);
      setEditing(null);
      if (!saved) router.replace(scopedHref(dashboardHref(result.id), scope));
    } catch (e) {
      setMessage({ tone: "bad", text: describeError(e) });
    } finally {
      setBusy(false);
    }
  }

  async function copy() {
    if (!shown) return;
    setBusy(true);
    try {
      const result = await api.createDashboard({
        name: copyName(shown.name),
        shared_with: { kind: "personal" },
        spec: { ...copySpec(shown.spec, within), range },
      });
      router.push(scopedHref(dashboardHref(result.id), scope));
    } catch (e) {
      setMessage({ tone: "bad", text: describeError(e) });
      setBusy(false);
    }
  }

  async function remove() {
    if (!saved) return;
    setBusy(true);
    try {
      await api.deleteDashboard(saved.id);
      router.push(scopedHref(DASHBOARDS_PATH, scope));
    } catch (e) {
      setMessage({ tone: "bad", text: describeError(e) });
      setBusy(false);
    }
  }

  function onDrop(event: DragEvent, to: number) {
    event.preventDefault();
    if (dragged !== null && dragged !== to) setPanels(placePanel(panels, dragged, to));
    setDragged(null);
  }

  const rangeButtons = (
    <div className={styles.ranges} role="group" aria-label={t("dashboards.view.range")}>
      {RANGES.map((r) => (
        <Button key={r} size="small" active={r === range} aria-pressed={r === range} onClick={() => setParams({ range: r === shown.spec.range ? null : r })}>
          {rangeLabel(r)}
        </Button>
      ))}
    </div>
  );

  const header = draft ? (
    <form className={styles.editHead} onSubmit={(e) => void save(e)} aria-label={t("dashboards.view.form")}>
      <label className={admin.field}>
        {t("dashboards.view.name")}
        <input
          className={fieldClass}
          required
          maxLength={MAX_NAME}
          value={draft.name}
          placeholder={t("dashboards.view.namePlaceholder")}
          onChange={(e) => setDraft({ ...draft, name: e.target.value })}
        />
      </label>
      <label className={admin.field}>
        {t("dashboards.view.sharedWith")}
        <select
          className={fieldClass}
          value={sharingValue(draft.shared_with)}
          onChange={(e) => {
            const option = sharingOptions(me, servers ?? [], draft.shared_with).find((o) => o.value === e.target.value);
            if (option) setDraft({ ...draft, shared_with: option.sharing });
          }}
        >
          {sharingOptions(me, servers ?? [], draft.shared_with).map((o) => (
            <option key={o.value} value={o.value}>
              {o.label}
            </option>
          ))}
        </select>
        <span className={admin.fieldNote}>
          {draft.shared_with.kind === "personal" ? t("dashboards.view.onlyYou") : t("dashboards.view.everyone")}
        </span>
      </label>
      <label className={admin.field}>
        {t("dashboards.view.opensOn")}
        <select
          className={fieldClass}
          value={draft.spec.range}
          onChange={(e) => setDraft({ ...draft, spec: { ...draft.spec, range: e.target.value as TimeRangeLabel } })}
        >
          {RANGES.map((r) => (
            <option key={r} value={r}>
              {t("dashboards.view.last", { range: rangeLabel(r) })}
            </option>
          ))}
        </select>
      </label>
      <div className={admin.formFoot}>
        <Button type="submit" variant="k" disabled={busy || editing !== null}>
          {busy ? t("dashboards.view.saving") : t("dashboards.view.save")}
        </Button>
        <Button onClick={cancel} disabled={busy}>
          {t("dashboards.view.cancel")}
        </Button>
      </div>
    </form>
  ) : null;

  return (
    <Page>
      <Breadcrumb
        parents={[{ label: t("dashboards.list.title"), href: scopedHref(DASHBOARDS_PATH, scope) }]}
        current={shown.name || t("dashboards.view.new")}
      />
      <PageHeader
        detail
        title={shown.name || t("dashboards.view.new")}
        status={
          <Label tone="neutral">
            {builtIn
              ? t("dashboards.view.readyMade")
              : draft
                ? (sharingOptions(me, servers ?? [], draft.shared_with).find((o) => o.value === sharingValue(draft.shared_with))?.label ??
                  sharingLabel(draft.shared_with, multiCustomer(me)))
                : sharingLabel(saved?.shared_with ?? shown.shared_with, multiCustomer(me))}
          </Label>
        }
        meta={panels.length > 0 ? <RefreshLine updatedAt={updatedAt} stale={stale} intervalMs={DASHBOARD_REFRESH_MS} /> : undefined}
        actions={
          !draft && (
            <>
              {panels.length > 0 && (saved || builtIn) && (
                <ButtonLink href={scopedHref(displayHref(saved ? { id: saved.id } : { ready: builtIn!.id }), scope)}>
                  {t("dashboards.view.show")}
                </ButtonLink>
              )}
              {canEdit && <Button onClick={startEditing}>{t("dashboards.view.edit")}</Button>}
              <Button onClick={() => void copy()} disabled={busy}>
                {t("dashboards.view.copy")}
              </Button>
              {canEdit && saved && !confirmDelete && (
                <Button variant="danger" onClick={() => setConfirmDelete(true)}>
                  {t("dashboards.view.delete")}
                </Button>
              )}
            </>
          )
        }
      />
      {confirmDelete && saved && (
        <div className={styles.confirm} role="group" aria-label={t("dashboards.view.deleteGroup")}>
          <span>{t("dashboards.view.deleteText", { name: saved.name })}</span>
          <Button onClick={() => setConfirmDelete(false)} disabled={busy}>
            {t("dashboards.view.cancel")}
          </Button>
          <Button variant="danger" onClick={() => void remove()} disabled={busy}>
            {t("dashboards.view.deleteNamed", { name: saved.name })}
          </Button>
        </div>
      )}
      {header}
      {message && <Banner tone={message.tone}>{message.text}</Banner>}
      {cleaned && cleaned.dropped > 0 && (
        <Banner tone="info">
          {t("dashboards.view.dropped", { count: cleaned.dropped })}
        </Banner>
      )}
      {stale && updatedAt && <StaleBanner error={describeError(error)} updatedAt={updatedAt} failures={failures} onRetry={refresh} />}
      {!data && error !== null && !isUnauthorized(error) && <Banner tone="bad">{t("dashboards.view.panelsFailed", { error: describeError(error) })}</Banner>}
      {panels.length > 0 && rangeButtons}

      {draft && editing !== null && servers && (
        <PanelForm
          key={editing}
          initial={editing === "new" ? newPanel(panels.map((p) => p.id)) : panels[editing]!}
          isNew={editing === "new"}
          servers={choosable}
          within={within}
          onCancel={() => setEditing(null)}
          onDone={(panel) => {
            setPanels(editing === "new" ? [...panels, panel] : panels.map((p, i) => (i === editing ? panel : p)));
            setEditing(null);
          }}
        />
      )}

      {panels.length === 0 ? (
        <EmptyState
          title={t("dashboards.view.noPanels")}
          actions={
            draft ? (
              <Button variant="k" onClick={() => setEditing("new")} disabled={!servers}>
                {t("dashboards.view.addPanel")}
              </Button>
            ) : (
              canEdit && <Button onClick={startEditing}>{t("dashboards.view.edit")}</Button>
            )
          }
        >
          {t("dashboards.view.noPanelsText")}
        </EmptyState>
      ) : (
        <div className={styles.grid}>
          {panels.map((panel, index) => (
            <div
              key={panel.id}
              className={styles[`w${panel.width}`]}
              draggable={draft !== null && editing === null}
              onDragStart={() => setDragged(index)}
              onDragOver={(e) => draft && e.preventDefault()}
              onDrop={(e) => onDrop(e, index)}
              data-dragging={dragged === index || undefined}
            >
              <DashboardPanel panel={panel} data={byId.get(panel.id)} window={data} scope={scope} />
              {draft && (
                <div className={styles.tools} role="group" aria-label={t("dashboards.view.arrange", { title: panel.title })}>
                  <Button
                    size="small"
                    onClick={() => setEditing(index)}
                    disabled={!servers || editing !== null}
                    aria-label={t("dashboards.view.editPanel", { title: panel.title })}
                  >
                    {t("dashboards.view.edit")}
                  </Button>
                  <Button
                    size="small"
                    onClick={() => setPanels(movePanel(panels, index, -1))}
                    disabled={index === 0}
                    aria-label={t("dashboards.view.moveEarlier", { title: panel.title })}
                  >
                    {t("dashboards.view.earlier")}
                  </Button>
                  <Button
                    size="small"
                    onClick={() => setPanels(movePanel(panels, index, 1))}
                    disabled={index === panels.length - 1}
                    aria-label={t("dashboards.view.moveLater", { title: panel.title })}
                  >
                    {t("dashboards.view.later")}
                  </Button>
                  <select
                    className={fieldClass}
                    aria-label={t("dashboards.view.widthOf", { title: panel.title })}
                    value={panel.width}
                    onChange={(e) => setPanels(panels.map((p, i) => (i === index ? { ...p, width: Number(e.target.value) as Panel["width"] } : p)))}
                  >
                    {([1, 2, 3] as const).map((w) => (
                      <option key={w} value={w}>
                        {widthLabel(w)}
                      </option>
                    ))}
                  </select>
                  <Button size="small" variant="danger" onClick={() => setPanels(panels.filter((_, i) => i !== index))} aria-label={t("dashboards.view.removePanel", { title: panel.title })}
                  >
                    {t("dashboards.view.remove")}
                  </Button>
                </div>
              )}
            </div>
          ))}
        </div>
      )}
      {draft && panels.length > 0 && (
        <div>
          <Button variant="k" onClick={() => setEditing("new")} disabled={!servers || editing !== null || panels.length >= MAX_PANELS}>
            {t("dashboards.view.addPanel")}
          </Button>
          {panels.length >= MAX_PANELS && <span className={admin.dim}> {t("dashboards.view.maxPanels", { max: MAX_PANELS })}</span>}
        </div>
      )}
      {draft && <p className={admin.footnote}>{t("dashboards.view.footnote")}</p>}
      {!draft && saved?.can_link && <DisplayLinks dashboard={saved} />}
    </Page>
  );
}
