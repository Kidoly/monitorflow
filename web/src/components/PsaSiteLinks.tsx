"use client";

import { useEffect, useId, useState } from "react";

import { t } from "@/i18n";
import { ApiError, type PsaRecord, type PsaSiteLink, api, describeError, isAbortError } from "@/lib/api";
import { multiCustomer, useMe } from "@/lib/me";
import { usePolledData } from "@/lib/usePolledData";

import styles from "./AdminPages.module.css";
import own from "./IntegrationsCatalogue.module.css";
import { Button, fieldClass } from "./ui/Button";
import { type Column, DataTable } from "./ui/DataTable";
import { InlineConfirm } from "./ui/InlineConfirm";
import { ErrorState, LoadingBlock } from "./ui/PageState";
import { Label } from "./ui/StatusLabel";

/** Clients a search lists at most, as the server says. */
const CLIENT_PAGE = 50;
/** Wait after the last key before searching. */
const SEARCH_DELAY_MS = 300;

/** HaloPSA's own words when it refused (502 `psa_unavailable`), else the usual sentence. */
export function psaError(error: unknown): string {
  return error instanceof ApiError && error.status === 502 && error.message ? error.message : describeError(error);
}

/** "Sites matched: 3 of 5". */
export function matchedSummary(sites: PsaSiteLink[]): string {
  const matched = sites.filter((s) => s.client_id !== null).length;
  return t("psa.sites.summary", { count: sites.length, matched });
}

/** Step 2 of the HaloPSA page: each MonitorFlow site and its HaloPSA client (ADR-0166). */
export function PsaSiteLinks() {
  const { data, error, refresh } = usePolledData(api.psaSiteLinks);
  const me = useMe();
  const [editing, setEditing] = useState<string | null>(null);
  const [actionError, setActionError] = useState<string | null>(null);
  if (!data) {
    return error ? (
      <ErrorState what={t("psa.sites.what")} error={describeError(error)} onRetry={refresh} />
    ) : (
      <LoadingBlock />
    );
  }
  const several = multiCustomer(me);
  const columns: Column<PsaSiteLink>[] = [
    {
      key: "site",
      header: t("psa.column.site"),
      cell: (s) => (several ? `${s.customer_name} / ${s.site_name}` : s.site_name),
    },
    {
      key: "client",
      header: t("psa.column.client"),
      cell: (s) => s.client_name ?? <Label tone="neutral">{t("psa.sites.notMatched")}</Label>,
    },
    {
      key: "psaSite",
      header: t("psa.column.psaSite"),
      cell: (s) => (s.client_id === null ? "" : (s.psa_site_name ?? <span className={styles.dim}>{t("psa.sites.anySite")}</span>)),
    },
  ];
  return (
    <section className={styles.section} aria-label={t("psa.step.sites")}>
      <div className={styles.sectionHead}>
        <h2 className={styles.sectionTitle}>{t("psa.step.sites")}</h2>
        <span className={styles.dim}>{matchedSummary(data.sites)}</span>
      </div>
      <p className={styles.sectionNote}>{several ? t("psa.sites.intro") : t("psa.sites.introCompany")}</p>
      {actionError && (
        <p className={styles.formError} role="alert">
          {actionError}
        </p>
      )}
      <DataTable
        bare
        label={t("psa.sites.label")}
        columns={columns}
        rows={data.sites}
        rowKey={(s) => s.site_id}
        rowActions={(s) => (
          <span className={styles.buttonRow}>
            <Button
              size="small"
              aria-label={t(s.client_id === null ? "psa.sites.matchName" : "psa.sites.changeName", { site: s.site_name })}
              onClick={() => setEditing(s.site_id)}
            >
              {s.client_id === null ? t("psa.sites.match") : t("psa.sites.change")}
            </Button>
            {s.client_id !== null && (
              <InlineConfirm
                layout="row"
                trigger={t("psa.sites.unmatch")}
                triggerName={t("psa.sites.unmatchName", { site: s.site_name })}
                title={t("psa.sites.unmatchName", { site: s.site_name })}
                consequence={several ? t("psa.sites.intro") : t("psa.sites.introCompany")}
                confirmLabel={t("psa.sites.unmatch")}
                onConfirm={async () => {
                  await api.unlinkPsaSite(s.site_id);
                  refresh();
                }}
                describeError={describeError}
              />
            )}
          </span>
        )}
        rowDetail={(s) =>
          editing === s.site_id ? (
            <ClientMatchForm
              label={t("psa.sites.matchName", { site: s.site_name })}
              initialClient={s.client_id === null ? null : { id: s.client_id, name: s.client_name ?? "" }}
              initialSite={s.psa_site_id}
              withSite
              onCancel={() => setEditing(null)}
              onSave={async (client, psaSite) => {
                await api.linkPsaSite(s.site_id, { client_id: client.id, psa_site_id: psaSite });
                setEditing(null);
                setActionError(null);
                refresh();
              }}
            />
          ) : null
        }
      />
    </section>
  );
}

type Found = { records: PsaRecord[] } | { error: string } | "searching";

interface ClientMatchProps {
  /** Names the group for assistive technologies. */
  label: string;
  initialClient: PsaRecord | null;
  initialSite?: number | null;
  /** Also offers one of the client's sites. */
  withSite?: boolean;
  onSave: (client: PsaRecord, psaSite: number | null) => Promise<void>;
  onCancel: () => void;
}

/** Searches HaloPSA's clients as the user types, then lists the chosen client's sites when asked. */
export function ClientMatchForm({ label, initialClient, initialSite = null, withSite = false, onSave, onCancel }: ClientMatchProps) {
  const id = useId();
  const [search, setSearch] = useState(initialClient?.name ?? "");
  const [found, setFound] = useState<Found>("searching");
  const [client, setClient] = useState<PsaRecord | null>(initialClient);
  const [sites, setSites] = useState<PsaRecord[] | null>(null);
  const [psaSite, setPsaSite] = useState<string>(initialSite === null ? "" : String(initialSite));
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    const controller = new AbortController();
    const timer = setTimeout(() => {
      setFound("searching");
      api
        .psaClients(search.trim(), controller.signal)
        .then((list) => setFound({ records: list.records }))
        .catch((e: unknown) => {
          if (!isAbortError(e)) setFound({ error: psaError(e) });
        });
    }, SEARCH_DELAY_MS);
    return () => {
      clearTimeout(timer);
      controller.abort();
    };
  }, [search]);

  useEffect(() => {
    if (!client || !withSite) return;
    const controller = new AbortController();
    api
      .psaClientSites(client.id, controller.signal)
      .then((list) => setSites(list.records))
      .catch((e: unknown) => {
        if (!isAbortError(e)) setError(psaError(e));
      });
    return () => controller.abort();
  }, [client, withSite]);

  async function save() {
    if (!client) return;
    setBusy(true);
    setError(null);
    try {
      await onSave(client, psaSite ? Number(psaSite) : null);
    } catch (e) {
      setError(psaError(e));
    } finally {
      setBusy(false);
    }
  }

  return (
    <div className={styles.stack} role="group" aria-label={label}>
      <label className={styles.field}>
        {t("psa.sites.search")}
        <input
          className={fieldClass}
          type="search"
          value={search}
          onChange={(e) => setSearch(e.target.value)}
          placeholder={t("psa.sites.searchPlaceholder")}
          autoComplete="off"
        />
      </label>
      {found === "searching" ? (
        <span className={styles.dim}>{t("psa.sites.searching")}</span>
      ) : "error" in found ? (
        <span role="alert" className={styles.formError}>
          {found.error}
        </span>
      ) : found.records.length === 0 ? (
        <span className={styles.dim}>{t("psa.sites.noClient")}</span>
      ) : (
        <fieldset className={styles.fieldset}>
          <legend className={styles.legend}>{t("psa.sites.client")}</legend>
          <ul className={own.choices}>
            {found.records.map((r) => (
              <li key={r.id}>
                <label>
                  <input
                    type="radio"
                    name={`${id}-client`}
                    checked={client?.id === r.id}
                    onChange={() => {
                      setClient(r);
                      setSites(null);
                      setPsaSite("");
                    }}
                  />
                  {r.name}
                </label>
              </li>
            ))}
          </ul>
          {found.records.length >= CLIENT_PAGE && <span className={styles.fieldNote}>{t("psa.sites.firstOnly")}</span>}
        </fieldset>
      )}
      {client && withSite && (
        <label className={styles.field}>
          {t("psa.sites.psaSite")}
          <select
            className={fieldClass}
            value={psaSite}
            onChange={(e) => setPsaSite(e.target.value)}
            disabled={sites === null}
          >
            <option value="">{sites === null ? t("psa.sites.loadingSites") : t("psa.sites.anySite")}</option>
            {sites?.map((s) => (
              <option key={s.id} value={String(s.id)}>
                {s.name}
              </option>
            ))}
          </select>
        </label>
      )}
      {error && (
        <p className={styles.formError} role="alert">
          {error}
        </p>
      )}
      <div className={styles.formFoot}>
        <Button variant="k" disabled={!client || busy} onClick={() => void save()}>
          {t("psa.sites.save")}
        </Button>
        <Button onClick={onCancel}>{t("psa.sites.cancel")}</Button>
      </div>
    </div>
  );
}
