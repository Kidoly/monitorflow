"use client";

import Link from "next/link";
import { type FormEvent, useCallback, useEffect, useState } from "react";

import { t } from "@/i18n";
import {
  type PsaAssetLink,
  type PsaAssetSettings,
  type PsaAssetType,
  type PsaRecord,
  api,
  isAbortError,
} from "@/lib/api";
import { multiCustomer, useMe } from "@/lib/me";
import { serverHref } from "@/lib/routes";
import { usePolledData } from "@/lib/usePolledData";

import styles from "./AdminPages.module.css";
import { psaError } from "./PsaSiteLinks";
import { Button, fieldClass } from "./ui/Button";
import { type Column, DataTable } from "./ui/DataTable";
import { EmptyState, ErrorState, LoadingBlock } from "./ui/PageState";
import { RelativeTime } from "./ui/RelativeTime";
import { Label } from "./ui/StatusLabel";
import type { Tone } from "./ui/tone";

const FAMILIES = ["linux", "windows"] as const;
type Family = (typeof FAMILIES)[number];
const VALUES = ["hostname_field", "serial_field", "manufacturer_field", "model_field"] as const;
type Value = (typeof VALUES)[number];
/** HaloPSA's asset tag, which every asset has besides its type's fields. */
const TAG = "inventory_number";

const LINK_TONES: Record<PsaAssetLink["state"], Tone> = {
  proposed: "warn",
  create: "info",
  linked: "ok",
  failed: "bad",
};

type Draft = Record<Family, { type: string } & Record<Value, string>>;

function draftOf(settings: PsaAssetSettings): Draft {
  const one = (family: Family) => {
    const kind = settings.types.find((k) => k.family === family);
    return {
      type: kind ? String(kind.type_id) : "",
      hostname_field: kind?.hostname_field ?? "",
      serial_field: kind?.serial_field ?? "",
      manufacturer_field: kind?.manufacturer_field ?? "",
      model_field: kind?.model_field ?? "",
    };
  };
  return { linux: one("linux"), windows: one("windows") };
}

/** The assets step of the HaloPSA page (ADR-0164, ADR-0191). */
export function PsaAssets() {
  const { data, error, refresh } = usePolledData(api.psaAssetSettings);
  const several = multiCustomer(useMe());
  return (
    <section className={styles.section} aria-label={t("psa.step.assets")}>
      <div className={styles.sectionHead}>
        <h2 className={styles.sectionTitle}>{t("psa.step.assets")}</h2>
      </div>
      <p className={styles.sectionNote}>{several ? t("psa.assets.intro") : t("psa.assets.introCompany")}</p>
      <div className={styles.sectionBody}>
        {data ? (
          <>
            <AssetSettings key={JSON.stringify(data)} saved={data} onSaved={refresh} />
            {data.asset_sync && <AssetLinks />}
          </>
        ) : error ? (
          <ErrorState what={t("psa.assets.what")} error={psaError(error)} onRetry={refresh} />
        ) : (
          <LoadingBlock />
        )}
      </div>
    </section>
  );
}

function AssetSettings({ saved, onSaved }: { saved: PsaAssetSettings; onSaved: () => void }) {
  const [sync, setSync] = useState(saved.asset_sync);
  const [inactive, setInactive] = useState(saved.asset_inactive_on_removal);
  const [draft, setDraft] = useState<Draft>(() => draftOf(saved));
  const [types, setTypes] = useState<PsaRecord[] | { error: string } | null>(null);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [done, setDone] = useState(false);

  useEffect(() => {
    const controller = new AbortController();
    api
      .psaAssetTypes(controller.signal)
      .then((list) => setTypes(list.records))
      .catch((e: unknown) => {
        if (!isAbortError(e)) setTypes({ error: psaError(e) });
      });
    return () => controller.abort();
  }, []);

  async function submit(event: FormEvent) {
    event.preventDefault();
    const chosen: Omit<PsaAssetType, "type_name">[] = FAMILIES.filter((f) => draft[f].type !== "").map((family) => {
      const d = draft[family];
      return {
        family,
        type_id: Number(d.type),
        hostname_field: d.hostname_field || null,
        serial_field: d.serial_field || null,
        manufacturer_field: d.manufacturer_field || null,
        model_field: d.model_field || null,
      };
    });
    setBusy(true);
    setError(null);
    try {
      await api.savePsaAssetSettings({ asset_sync: sync, asset_inactive_on_removal: inactive, types: chosen });
      setDone(true);
      onSaved();
    } catch (e) {
      setError(psaError(e));
    } finally {
      setBusy(false);
    }
  }

  if (types === null) return <span className={styles.dim}>{t("psa.tickets.reading")}</span>;
  if ("error" in types) {
    return (
      <p className={styles.formError} role="alert">
        {types.error}
      </p>
    );
  }
  const change = (family: Family, values: Partial<Draft[Family]>) => {
    setDraft({ ...draft, [family]: { ...draft[family], ...values } });
    setDone(false);
  };
  return (
    <form className={styles.stack} onSubmit={submit} aria-label={t("psa.step.assets")}>
      <label>
        <input
          type="checkbox"
          checked={sync}
          onChange={(e) => {
            setSync(e.target.checked);
            setDone(false);
          }}
        />{" "}
        {t("psa.assets.sync")}
      </label>
      {FAMILIES.map((family) => (
        <FamilyFields
          key={family}
          family={family}
          types={types}
          value={draft[family]}
          onChange={(values) => change(family, values)}
        />
      ))}
      <label>
        <input
          type="checkbox"
          checked={inactive}
          onChange={(e) => {
            setInactive(e.target.checked);
            setDone(false);
          }}
        />{" "}
        {t("psa.assets.inactive")}
      </label>
      {error && (
        <p className={styles.formError} role="alert">
          {error}
        </p>
      )}
      <div className={styles.formFoot}>
        <Button type="submit" variant="k" disabled={busy}>
          {t("psa.tickets.save")}
        </Button>
        {done && <span className={styles.dim}>{t("psa.tickets.saved")}</span>}
      </div>
    </form>
  );
}

/** One family's asset type and where each value goes. */
function FamilyFields({
  family,
  types,
  value,
  onChange,
}: {
  family: Family;
  types: PsaRecord[];
  value: Draft[Family];
  onChange: (values: Partial<Draft[Family]>) => void;
}) {
  // Keyed by the asset type they were read for.
  const [read, setRead] = useState<{ type: string; records: PsaRecord[] } | null>(null);
  const fields = read?.type === value.type ? read.records : null;
  useEffect(() => {
    const type = value.type;
    if (!type) return;
    const controller = new AbortController();
    api
      .psaAssetFields(Number(type), controller.signal)
      .then((list) => setRead({ type, records: list.records }))
      .catch((e: unknown) => {
        if (!isAbortError(e)) setRead({ type, records: [] });
      });
    return () => controller.abort();
  }, [value.type]);
  return (
    <fieldset className={styles.fieldset}>
      <legend className={styles.legend}>{t(`psa.assets.family.${family}`)}</legend>
      <label className={styles.field}>
        {t("psa.assets.type")}
        <select
          className={fieldClass}
          value={value.type}
          onChange={(e) =>
            onChange({ type: e.target.value, hostname_field: "", serial_field: "", manufacturer_field: "", model_field: "" })
          }
        >
          <option value="">{t("psa.assets.notSynced")}</option>
          {types.map((r) => (
            <option key={r.id} value={String(r.id)}>
              {r.name}
            </option>
          ))}
        </select>
      </label>
      {value.type &&
        VALUES.map((key) => (
          <label key={key} className={styles.field}>
            {t(`psa.assets.value.${key}`)}
            <select
              className={fieldClass}
              value={value[key]}
              disabled={fields === null}
              onChange={(e) => onChange({ [key]: e.target.value })}
            >
              <option value="">{fields === null ? t("psa.tickets.priorityReading") : t("psa.assets.nowhere")}</option>
              <option value={TAG}>{t("psa.assets.tag")}</option>
              {fields?.map((f) => (
                <option key={f.id} value={String(f.id)}>
                  {f.name}
                </option>
              ))}
            </select>
          </label>
        ))}
    </fieldset>
  );
}

function AssetLinks() {
  const fetcher = useCallback((signal: AbortSignal) => api.psaAssetLinks(signal), []);
  const { data, error, refresh } = usePolledData(fetcher);
  const [failure, setFailure] = useState<string | null>(null);
  const [queued, setQueued] = useState<number | null>(null);
  if (!data) {
    return error ? (
      <ErrorState what={t("psa.assets.linksWhat")} error={psaError(error)} onRetry={refresh} />
    ) : (
      <LoadingBlock />
    );
  }
  const act = (work: Promise<unknown>) => {
    setFailure(null);
    work.then(refresh).catch((e: unknown) => setFailure(psaError(e)));
  };
  const now = new Date();
  const columns: Column<PsaAssetLink>[] = [
    {
      key: "server",
      header: t("psa.column.server"),
      cell: (row) => (
        <Link href={serverHref(row.server_id)}>
          {t("psa.assets.serverAt", { server: row.hostname, site: row.site_name })}
        </Link>
      ),
    },
    {
      key: "asset",
      header: t("psa.column.asset"),
      cell: (row) =>
        row.asset_id === null ? "" : t("psa.assets.asset", { name: row.asset_name ?? "", id: String(row.asset_id) }),
    },
    {
      key: "state",
      header: t("psa.column.state"),
      cell: (row) => (
        <span>
          <Label tone={LINK_TONES[row.state]}>{t(`psa.assets.state.${row.state}`)}</Label>
          {row.matched_by && row.state === "proposed" && (
            <span className={styles.fieldNote}> {t(`psa.assets.by.${row.matched_by}`)}</span>
          )}
          {row.deactivated && <span className={styles.fieldNote}> {t("psa.assets.deactivated")}</span>}
          {row.error && <span className={styles.fieldNote}> {row.error}</span>}
        </span>
      ),
    },
    {
      key: "when",
      header: t("psa.column.pushed"),
      cell: (row) => (row.pushed_at ? <RelativeTime value={row.pushed_at} now={now} /> : t("psa.assets.never")),
    },
  ];
  return (
    <div className={styles.stack}>
      <div className={styles.buttonRow}>
        <span className={styles.dim}>
          {t("psa.assets.summary", { linked: data.linked, proposed: data.proposed, failed: data.failed })}
        </span>
        <Button size="small" onClick={() => act(api.syncPsaAssets().then((r) => setQueued(r.queued)))}>
          {t("psa.assets.syncNow")}
        </Button>
        {queued !== null && <span className={styles.dim}>{t("psa.assets.queued", { count: queued })}</span>}
      </div>
      {failure && (
        <p className={styles.formError} role="alert">
          {failure}
        </p>
      )}
      {data.servers.length === 0 ? (
        <EmptyState>{t("psa.assets.none")}</EmptyState>
      ) : (
        <DataTable
          dense
          label={t("psa.assets.links")}
          columns={columns}
          rows={data.servers}
          rowKey={(row) => row.server_id}
          rowActions={(row) =>
            row.state === "proposed" ? (
              <span className={styles.buttonRow}>
                <Button
                  size="small"
                  variant="k"
                  aria-label={t("psa.assets.confirmName", { server: row.hostname })}
                  onClick={() => act(api.linkPsaAsset(row.server_id, row.asset_id))}
                >
                  {t("psa.assets.confirm")}
                </Button>
                <Button
                  size="small"
                  aria-label={t("psa.assets.createName", { server: row.hostname })}
                  onClick={() => act(api.linkPsaAsset(row.server_id, null))}
                >
                  {t("psa.assets.create")}
                </Button>
              </span>
            ) : (
              <Button
                size="small"
                aria-label={t("psa.assets.forgetName", { server: row.hostname })}
                onClick={() => act(api.unlinkPsaAsset(row.server_id))}
              >
                {t("psa.assets.forget")}
              </Button>
            )
          }
        />
      )}
    </div>
  );
}
