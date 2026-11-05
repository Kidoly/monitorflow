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
  RANGE_LABELS,
  READY_MADE,
  WIDTH_LABELS,
  copyName,
  copySpec,
  dropMissingServers,
  isRange,
  movePanel,
  newPanel,
  placePanel,
  placesOf,
  readyMade,
  serversWithin,
  sharingLabel,
  withinOf,
} from "@/lib/dashboards";
import { displayHref } from "@/lib/display";
import { modifiesCustomer, modifiesSite, useMe } from "@/lib/me";
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
  const fetcher = useCallback((signal: AbortSignal) => api.dashboards(signal), []);
  const { data, error, stale, failures, updatedAt, refresh } = usePolledData(fetcher, { intervalMs: DASHBOARD_REFRESH_MS });
  const columns: Column<DashboardSummary>[] = [
    {
      key: "name",
      header: "Name",
      cell: (d) => (
        <PrimaryCell
          title={d.name}
          href={scopedHref(dashboardHref(d.id), scope)}
          sub={`${d.panels} ${d.panels === 1 ? "panel" : "panels"}`}
        />
      ),
      sortValue: (d) => d.name,
    },
    { key: "shared", header: "Shared with", cell: (d) => sharingLabel(d.shared_with), sortValue: (d) => sharingLabel(d.shared_with) },
    {
      key: "updated",
      header: "Changed",
      cell: (d) => (
        <>
          {formatDay(new Date(d.updated_at), new Date())}
          {d.updated_by_username && <span className={admin.dim}> by {d.updated_by_username}</span>}
        </>
      ),
      sortValue: (d) => d.updated_at,
    },
  ];
  const newHref = scopedHref(`${DASHBOARDS_PATH}view/?new=1`, scope);
  return (
    <Page>
      <PageHeader
        title="Dashboards"
        meta={<RefreshLine updatedAt={updatedAt} stale={stale} intervalMs={DASHBOARD_REFRESH_MS} />}
        actions={
          <ButtonLink variant="k" href={newHref}>
            New dashboard
          </ButtonLink>
        }
      />
      {stale && updatedAt && <StaleBanner error={describeError(error)} updatedAt={updatedAt} failures={failures} onRetry={refresh} />}
      <section className={admin.section} aria-labelledby="ready-made">
        <div className={admin.sectionHead}>
          <h2 id="ready-made" className={admin.sectionTitle}>
            Ready-made
          </h2>
          <span className={admin.dim}>For every server you see. Make a copy to change one.</span>
        </div>
        <ul className={styles.cards}>
          {READY_MADE.map((d) => (
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
          <ErrorState what="the dashboards" error={describeError(error)} onRetry={refresh} />
        ) : (
          <LoadingBlock label="Loading dashboards" />
        )
      ) : data.dashboards.length === 0 ? (
        <EmptyState
          title="No dashboard of your own yet"
          actions={
            <ButtonLink variant="k" href={newHref}>
              New dashboard
            </ButtonLink>
          }
        >
          Build a page of charts and figures by picking from lists, for yourself or shared with a site or customer.
        </EmptyState>
      ) : (
        <DataTable label="Dashboards" columns={columns} rows={data.dashboards} rowKey={(d) => d.id} />
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
    { value: "personal", label: "Only me", sharing: { kind: "personal" } },
  ];
  for (const c of customers) {
    if (modifiesCustomer(me, c.id)) options.push({ value: `customer:${c.id}`, label: `Customer ${c.name}`, sharing: { kind: "customer", id: c.id } });
  }
  for (const s of sites) {
    if (modifiesSite(me, s.customerId, s.id)) options.push({ value: `site:${s.id}`, label: `Site ${s.name}`, sharing: { kind: "site", id: s.id } });
  }
  const value = sharingValue(current);
  if (!options.some((o) => o.value === value)) options.push({ value, label: sharingLabel(current), sharing: current });
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
        title="Dashboard not found"
        actions={<ButtonLink href={scopedHref(DASHBOARDS_PATH, scope)}>Back to the dashboards</ButtonLink>}
      />
    );
  }
  if (!shown) {
    return loadError !== null && !isUnauthorized(loadError) ? (
      <ErrorState what="the dashboard" error={describeError(loadError)} />
    ) : (
      <LoadingBlock label="Loading dashboard" />
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
    if (!editDraft.name.trim()) return setMessage({ tone: "bad", text: "Give the dashboard a name." });
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
    <div className={styles.ranges} role="group" aria-label="Time range">
      {RANGES.map((r) => (
        <Button key={r} size="small" active={r === range} aria-pressed={r === range} onClick={() => setParams({ range: r === shown.spec.range ? null : r })}>
          {RANGE_LABELS[r]}
        </Button>
      ))}
    </div>
  );

  const header = draft ? (
    <form className={styles.editHead} onSubmit={(e) => void save(e)} aria-label="Dashboard">
      <label className={admin.field}>
        Name
        <input
          className={fieldClass}
          required
          maxLength={MAX_NAME}
          value={draft.name}
          placeholder="Database servers"
          onChange={(e) => setDraft({ ...draft, name: e.target.value })}
        />
      </label>
      <label className={admin.field}>
        Shared with
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
          {draft.shared_with.kind === "personal" ? "Only you see it." : "Everyone there sees it; its operators change it."}
        </span>
      </label>
      <label className={admin.field}>
        Opens on
        <select
          className={fieldClass}
          value={draft.spec.range}
          onChange={(e) => setDraft({ ...draft, spec: { ...draft.spec, range: e.target.value as TimeRangeLabel } })}
        >
          {RANGES.map((r) => (
            <option key={r} value={r}>
              Last {RANGE_LABELS[r]}
            </option>
          ))}
        </select>
      </label>
      <div className={admin.formFoot}>
        <Button type="submit" variant="k" disabled={busy || editing !== null}>
          {busy ? "Saving…" : "Save"}
        </Button>
        <Button onClick={cancel} disabled={busy}>
          Cancel
        </Button>
      </div>
    </form>
  ) : null;

  return (
    <Page>
      <Breadcrumb parents={[{ label: "Dashboards", href: scopedHref(DASHBOARDS_PATH, scope) }]} current={shown.name || "New dashboard"} />
      <PageHeader
        detail
        title={shown.name || "New dashboard"}
        status={
          <Label tone="neutral">
            {builtIn
              ? "Ready-made"
              : draft
                ? (sharingOptions(me, servers ?? [], draft.shared_with).find((o) => o.value === sharingValue(draft.shared_with))?.label ??
                  sharingLabel(draft.shared_with))
                : sharingLabel(saved?.shared_with ?? shown.shared_with)}
          </Label>
        }
        meta={panels.length > 0 ? <RefreshLine updatedAt={updatedAt} stale={stale} intervalMs={DASHBOARD_REFRESH_MS} /> : undefined}
        actions={
          !draft && (
            <>
              {panels.length > 0 && (saved || builtIn) && (
                <ButtonLink href={scopedHref(displayHref(saved ? { id: saved.id } : { ready: builtIn!.id }), scope)}>
                  Show on a screen
                </ButtonLink>
              )}
              {canEdit && <Button onClick={startEditing}>Edit</Button>}
              <Button onClick={() => void copy()} disabled={busy}>
                Make a copy
              </Button>
              {canEdit && saved && !confirmDelete && (
                <Button variant="danger" onClick={() => setConfirmDelete(true)}>
                  Delete
                </Button>
              )}
            </>
          )
        }
      />
      {confirmDelete && saved && (
        <div className={styles.confirm} role="group" aria-label="Delete the dashboard">
          <span>
            Delete “{saved.name}” for everyone it is shared with? Its panels go with it; the servers and their data stay.
          </span>
          <Button onClick={() => setConfirmDelete(false)} disabled={busy}>
            Cancel
          </Button>
          <Button variant="danger" onClick={() => void remove()} disabled={busy}>
            Delete {saved.name}
          </Button>
        </div>
      )}
      {header}
      {message && <Banner tone={message.tone}>{message.text}</Banner>}
      {cleaned && cleaned.dropped > 0 && (
        <Banner tone="info">
          {cleaned.dropped} removed {cleaned.dropped === 1 ? "server was" : "servers were"} taken out of the panels. Save to keep it so.
        </Banner>
      )}
      {stale && updatedAt && <StaleBanner error={describeError(error)} updatedAt={updatedAt} failures={failures} onRetry={refresh} />}
      {!data && error !== null && !isUnauthorized(error) && <Banner tone="bad">Could not load the panels: {describeError(error)}</Banner>}
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
          title="No panels yet"
          actions={
            draft ? (
              <Button variant="k" onClick={() => setEditing("new")} disabled={!servers}>
                Add a panel
              </Button>
            ) : (
              canEdit && <Button onClick={startEditing}>Edit</Button>
            )
          }
        >
          A panel is a chart, a figure, a top list or the open alerts of the servers you choose.
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
                <div className={styles.tools} role="group" aria-label={`Arrange ${panel.title}`}>
                  <Button size="small" onClick={() => setEditing(index)} disabled={!servers || editing !== null} aria-label={`Edit ${panel.title}`}>
                    Edit
                  </Button>
                  <Button size="small" onClick={() => setPanels(movePanel(panels, index, -1))} disabled={index === 0} aria-label={`Move ${panel.title} earlier`}>
                    Earlier
                  </Button>
                  <Button
                    size="small"
                    onClick={() => setPanels(movePanel(panels, index, 1))}
                    disabled={index === panels.length - 1}
                    aria-label={`Move ${panel.title} later`}
                  >
                    Later
                  </Button>
                  <select
                    className={fieldClass}
                    aria-label={`Width of ${panel.title}`}
                    value={panel.width}
                    onChange={(e) => setPanels(panels.map((p, i) => (i === index ? { ...p, width: Number(e.target.value) as Panel["width"] } : p)))}
                  >
                    {([1, 2, 3] as const).map((w) => (
                      <option key={w} value={w}>
                        {WIDTH_LABELS[w]}
                      </option>
                    ))}
                  </select>
                  <Button size="small" variant="danger" onClick={() => setPanels(panels.filter((_, i) => i !== index))} aria-label={`Remove ${panel.title}`}>
                    Remove
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
            Add a panel
          </Button>
          {panels.length >= MAX_PANELS && <span className={admin.dim}> A dashboard has at most {MAX_PANELS} panels.</span>}
        </div>
      )}
      {draft && <p className={admin.footnote}>Drag a panel to move it, or use Earlier and Later. Nothing is kept until you save.</p>}
      {!draft && saved?.can_link && <DisplayLinks dashboard={saved} />}
    </Page>
  );
}
