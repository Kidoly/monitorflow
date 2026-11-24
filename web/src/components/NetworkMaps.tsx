"use client";

import { useRouter } from "next/navigation";
import { type FormEvent, useCallback, useEffect, useState } from "react";

import { t } from "@/i18n";
import { type MapPlace, type MapSummary, api, describeError } from "@/lib/api";
import { placeLabel } from "@/lib/customers";
import { emptyDocument } from "@/lib/maps";
import { modifiesCustomer, modifiesSite, multiCustomer, multiSite, useMe } from "@/lib/me";
import { mapHref } from "@/lib/routes";
import { scopedHref, useScope } from "@/lib/scope";
import { usePolledData } from "@/lib/usePolledData";
import { Button, fieldClass } from "@/components/ui/Button";
import { type Column, DataTable, PrimaryCell } from "@/components/ui/DataTable";
import { Page } from "@/components/ui/Page";
import { EmptyState, ErrorState, LoadingBlock, PageHeader, RefreshLine, StaleBanner } from "@/components/ui/PageState";
import { RelativeTime } from "@/components/ui/RelativeTime";

import styles from "./AdminPages.module.css";

/** A place a map can belong to, with its words. */
interface PlaceChoice {
  value: string;
  place: MapPlace;
  label: string;
}

/** The sites and customers the user may make maps of, read once. */
function usePlaces(): PlaceChoice[] | null {
  const me = useMe();
  const [places, setPlaces] = useState<PlaceChoice[] | null>(null);
  useEffect(() => {
    const controller = new AbortController();
    Promise.all([api.sites(controller.signal), api.customers(controller.signal)])
      .then(([{ sites }, { customers }]) => {
        const many = multiCustomer(me);
        const ofCustomers: PlaceChoice[] =
          multiSite(me) || many
            ? customers
                .filter((c) => modifiesCustomer(me, c.id))
                .map((c) => ({
                  value: `customer:${c.id}`,
                  place: { kind: "customer", id: c.id },
                  label: many ? t("maps.place.customer", { name: c.name }) : t("maps.place.everySite"),
                }))
            : [];
        const ofSites: PlaceChoice[] = sites
          .filter((s) => modifiesSite(me, s.customer_id, s.id))
          .map((s) => ({ value: `site:${s.id}`, place: { kind: "site", id: s.id }, label: placeLabel(s.customer_name, s.name, many) }));
        setPlaces([...ofSites, ...ofCustomers]);
      })
      .catch(() => {
        if (!controller.signal.aborted) setPlaces([]);
      });
    return () => controller.abort();
  }, [me]);
  return places;
}

/** A new map: its name and where it belongs. */
function NewMapForm({ places, onCancel }: { places: PlaceChoice[]; onCancel: () => void }) {
  const router = useRouter();
  const scope = useScope();
  const [name, setName] = useState("");
  const [value, setValue] = useState(() => places.find((p) => p.place.id === scope.siteId)?.value ?? places[0]?.value ?? "");
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);

  async function submit(event: FormEvent) {
    event.preventDefault();
    const choice = places.find((p) => p.value === value);
    if (!name.trim()) return setError(t("maps.form.nameRequired"));
    if (!choice) return;
    setBusy(true);
    setError(null);
    try {
      const made = await api.createMap({ name: name.trim(), place: choice.place, document: emptyDocument() });
      router.push(scopedHref(`${mapHref(made.id)}&edit=1`, scope));
    } catch (e) {
      setError(t("maps.form.failed", { error: describeError(e) }));
      setBusy(false);
    }
  }

  return (
    <form className={styles.panel} onSubmit={(e) => void submit(e)} aria-label={t("maps.form.title")} noValidate>
      <label className={styles.field}>
        {t("maps.form.name")}
        <input className={fieldClass} value={name} maxLength={80} placeholder={t("maps.form.namePlaceholder")} onChange={(e) => setName(e.target.value)} />
      </label>
      {places.length > 1 && (
        <label className={styles.field}>
          {t("maps.form.place")}
          <select className={fieldClass} value={value} onChange={(e) => setValue(e.target.value)}>
            {places.map((p) => (
              <option key={p.value} value={p.value}>
                {p.label}
              </option>
            ))}
          </select>
        </label>
      )}
      <p className={styles.sectionNote}>{t("maps.form.note")}</p>
      <div className={styles.formFoot}>
        <Button variant="k" type="submit" disabled={busy}>
          {busy ? t("maps.form.making") : t("maps.form.make")}
        </Button>
        <Button onClick={onCancel} disabled={busy}>
          {t("maps.form.cancel")}
        </Button>
      </div>
      {error && (
        <p className={styles.formError} role="alert">
          {error}
        </p>
      )}
    </form>
  );
}

/** Monitoring › Network maps (ADR-0185). */
export function NetworkMapsPage() {
  const me = useMe();
  const scope = useScope();
  const places = usePlaces();
  const [creating, setCreating] = useState(false);
  const fetcher = useCallback((signal: AbortSignal) => api.maps(signal), []);
  const { data, error, stale, failures, updatedAt, refresh } = usePolledData(fetcher);
  const maps = (data?.maps ?? []).filter(
    (m) => !scope.siteId || (m.place.kind === "site" ? m.place.id === scope.siteId : true),
  );
  const canMake = (places?.length ?? 0) > 0;
  const now = new Date();
  const columns: Column<MapSummary>[] = [
    {
      key: "name",
      header: t("maps.column.name"),
      sortValue: (m) => m.name,
      cell: (m) => (
        <PrimaryCell
          title={m.name}
          href={scopedHref(mapHref(m.id), scope)}
          sub={multiSite(me) || multiCustomer(me) ? m.place.name : undefined}
        />
      ),
    },
    {
      key: "updated",
      header: t("maps.column.updated"),
      sortValue: (m) => m.updated_at,
      cell: (m) => (
        <>
          <RelativeTime value={m.updated_at} now={now} />
          {m.updated_by_username && ` · ${m.updated_by_username}`}
        </>
      ),
    },
  ];
  const newButton = canMake && !creating && (
    <Button variant="k" onClick={() => setCreating(true)}>
      {t("maps.list.new")}
    </Button>
  );

  return (
    <Page>
      <PageHeader title={t("maps.title")} meta={<RefreshLine updatedAt={updatedAt} stale={stale} />} actions={newButton} />
      <p className={styles.sectionNote}>{t("maps.list.note")}</p>
      {creating && places && <NewMapForm places={places} onCancel={() => setCreating(false)} />}
      {stale && updatedAt && <StaleBanner error={describeError(error)} updatedAt={updatedAt} failures={failures} onRetry={refresh} />}
      {error !== undefined && error !== null && !data ? (
        <ErrorState what={t("maps.title")} error={describeError(error)} onRetry={refresh} />
      ) : !data ? (
        <LoadingBlock />
      ) : maps.length === 0 && !creating ? (
        <EmptyState title={t("maps.list.emptyTitle")} actions={newButton}>
          {t("maps.list.empty")}
        </EmptyState>
      ) : (
        maps.length > 0 && <DataTable label={t("maps.title")} columns={columns} rows={maps} rowKey={(m) => m.id} />
      )}
    </Page>
  );
}
