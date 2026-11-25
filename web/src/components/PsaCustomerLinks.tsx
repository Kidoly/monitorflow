"use client";

import { useEffect, useState } from "react";

import { t } from "@/i18n";
import { type PsaConnection, type PsaCustomerLink, type PsaSuggestion, api, describeError, isAbortError } from "@/lib/api";
import { multiCustomer, useMe } from "@/lib/me";
import { usePolledData } from "@/lib/usePolledData";

import styles from "./AdminPages.module.css";
import { ClientMatchForm, psaError } from "./PsaSiteLinks";
import { Button } from "./ui/Button";
import { type Column, DataTable } from "./ui/DataTable";
import { InlineConfirm } from "./ui/InlineConfirm";
import { ErrorState, LoadingBlock } from "./ui/PageState";
import { Label } from "./ui/StatusLabel";

/**
 * The customers step of the HaloPSA page (ADR-0164, ADR-0191): each customer's HaloPSA
 * client, with suggestions by name, and the default client of sites matched to nothing.
 * A single company sees one choice: the client for this installation.
 */
export function PsaCustomerLinks({ connection, onSaved }: { connection: PsaConnection; onSaved: () => void }) {
  const me = useMe();
  const several = multiCustomer(me);
  const { data, error, refresh } = usePolledData(api.psaCustomerLinks);
  const [editing, setEditing] = useState<string | null>(null);
  const title = several ? t("psa.step.customers") : t("psa.step.company");
  return (
    <section className={styles.section} aria-label={title}>
      <div className={styles.sectionHead}>
        <h2 className={styles.sectionTitle}>{title}</h2>
      </div>
      <p className={styles.sectionNote}>{several ? t("psa.customers.intro") : t("psa.customers.introCompany")}</p>
      {!data ? (
        error ? (
          <ErrorState what={t("psa.customers.what")} error={describeError(error)} onRetry={refresh} />
        ) : (
          <LoadingBlock />
        )
      ) : (
        <div className={styles.sectionBody}>
          {several && <Suggestions customers={data.customers} onSaved={refresh} />}
          <DataTable
            bare
            label={title}
            columns={columns(several)}
            rows={several ? data.customers : data.customers.slice(0, 1)}
            rowKey={(c) => c.customer_id}
            rowActions={(c) => (
              <span className={styles.buttonRow}>
                <Button
                  size="small"
                  aria-label={t("psa.customers.matchName", { customer: c.customer_name })}
                  onClick={() => setEditing(c.customer_id)}
                >
                  {c.client_id === null ? t("psa.sites.match") : t("psa.sites.change")}
                </Button>
                {c.client_id !== null && (
                  <InlineConfirm
                    layout="row"
                    trigger={t("psa.sites.unmatch")}
                    triggerName={t("psa.customers.unmatchName", { customer: c.customer_name })}
                    title={t("psa.customers.unmatchName", { customer: c.customer_name })}
                    consequence={t("psa.customers.unmatchConsequence")}
                    confirmLabel={t("psa.sites.unmatch")}
                    onConfirm={async () => {
                      await api.unlinkPsaCustomer(c.customer_id);
                      refresh();
                    }}
                    describeError={describeError}
                  />
                )}
              </span>
            )}
            rowDetail={(c) =>
              editing === c.customer_id ? (
                <ClientMatchForm
                  label={t("psa.customers.matchName", { customer: c.customer_name })}
                  initialClient={c.client_id === null ? null : { id: c.client_id, name: c.client_name ?? "" }}
                  onCancel={() => setEditing(null)}
                  onSave={async (client) => {
                    await api.linkPsaCustomer(c.customer_id, client.id);
                    setEditing(null);
                    refresh();
                  }}
                />
              ) : null
            }
          />
          {several && <DefaultClient connection={connection} onSaved={onSaved} />}
        </div>
      )}
    </section>
  );
}

function columns(several: boolean): Column<PsaCustomerLink>[] {
  const all: Column<PsaCustomerLink>[] = [
    { key: "customer", header: t("psa.column.customer"), cell: (c) => c.customer_name },
    {
      key: "client",
      header: t("psa.column.client"),
      cell: (c) => c.client_name ?? <Label tone="neutral">{t("psa.sites.notMatched")}</Label>,
    },
    {
      key: "sites",
      header: t("psa.column.ownMatch"),
      cell: (c) => t("psa.customers.sites", { matched: c.matched_sites, count: c.site_count }),
    },
  ];
  return several ? all : all.slice(1, 2);
}

/** Suggestions by name, confirmed one by one or all at once; nothing is matched before. */
function Suggestions({ customers, onSaved }: { customers: PsaCustomerLink[]; onSaved: () => void }) {
  const unmatched = customers.some((c) => c.client_id === null);
  const [asked, setAsked] = useState(false);
  if (!unmatched) return null;
  if (!asked) {
    return (
      <div className={styles.buttonRow}>
        <Button onClick={() => setAsked(true)}>{t("psa.customers.suggest")}</Button>
        <span className={styles.fieldNote}>{t("psa.customers.suggestNote")}</span>
      </div>
    );
  }
  return <SuggestionList onSaved={onSaved} />;
}

/** Read once, and again after a confirmation: each read lists every HaloPSA client. */
function SuggestionList({ onSaved }: { onSaved: () => void }) {
  // Keyed by the round they were read in, so a new round shows as loading.
  const [answer, setAnswer] = useState<{ round: number; data?: { suggestions: PsaSuggestion[] }; error?: unknown } | null>(
    null,
  );
  const [round, setRound] = useState(0);
  const current = answer?.round === round ? answer : null;
  const data = current?.data ?? null;
  const error = current?.error ?? null;
  const [busy, setBusy] = useState(false);
  const [failure, setFailure] = useState<string | null>(null);
  const refresh = () => setRound((r) => r + 1);
  useEffect(() => {
    const controller = new AbortController();
    api
      .psaSuggestions(controller.signal)
      .then((found) => setAnswer({ round, data: found }))
      .catch((e: unknown) => {
        if (!isAbortError(e)) setAnswer({ round, error: e });
      });
    return () => controller.abort();
  }, [round]);
  if (!data) {
    return error ? (
      <ErrorState what={t("psa.customers.suggestionsWhat")} error={psaError(error)} onRetry={refresh} />
    ) : (
      <span className={styles.dim}>{t("psa.customers.suggesting")}</span>
    );
  }
  if (data.suggestions.length === 0) return <span className={styles.dim}>{t("psa.customers.noSuggestion")}</span>;

  async function confirm(list: PsaSuggestion[]) {
    setBusy(true);
    setFailure(null);
    try {
      for (const s of list) await api.linkPsaCustomer(s.customer_id, s.client_id);
    } catch (e) {
      setFailure(psaError(e));
    } finally {
      setBusy(false);
      refresh();
      onSaved();
    }
  }

  const cols: Column<PsaSuggestion>[] = [
    { key: "customer", header: t("psa.column.customer"), cell: (s) => s.customer_name },
    { key: "client", header: t("psa.column.suggested"), cell: (s) => s.client_name },
  ];
  return (
    <div className={styles.stack}>
      {failure && (
        <p className={styles.formError} role="alert">
          {failure}
        </p>
      )}
      <DataTable
        dense
        label={t("psa.customers.suggestions")}
        columns={cols}
        rows={data.suggestions}
        rowKey={(s) => s.customer_id}
        rowActions={(s) => (
          <Button
            size="small"
            disabled={busy}
            aria-label={t("psa.customers.confirmName", { customer: s.customer_name })}
            onClick={() => void confirm([s])}
          >
            {t("psa.customers.confirm")}
          </Button>
        )}
      />
      <div className={styles.buttonRow}>
        <Button variant="k" disabled={busy} onClick={() => void confirm(data.suggestions)}>
          {t("psa.customers.confirmAll", { count: data.suggestions.length })}
        </Button>
      </div>
    </div>
  );
}

/** The client, and optionally its site, of sites matched to nothing. */
function DefaultClient({ connection, onSaved }: { connection: PsaConnection; onSaved: () => void }) {
  const saved = connection.default_client;
  const [editing, setEditing] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const current =
    saved.default_client_name === null
      ? t("psa.customers.defaultNone")
      : saved.default_site_name === null
        ? saved.default_client_name
        : `${saved.default_client_name} / ${saved.default_site_name}`;
  return (
    <div className={styles.stack} role="group" aria-label={t("psa.customers.default")}>
      <div className={styles.buttonRow}>
        <span>
          <strong>{t("psa.customers.default")}</strong> {current}
        </span>
        <Button size="small" onClick={() => setEditing(!editing)}>
          {t("psa.sites.change")}
        </Button>
        {saved.default_client_id !== null && (
          <Button
            size="small"
            onClick={() => {
              setError(null);
              api
                .savePsaDefaultClient({ client_id: null, psa_site_id: null })
                .then(onSaved)
                .catch((e: unknown) => setError(psaError(e)));
            }}
          >
            {t("psa.customers.defaultClear")}
          </Button>
        )}
      </div>
      <span className={styles.fieldNote}>{t("psa.customers.defaultNote")}</span>
      {error && (
        <p className={styles.formError} role="alert">
          {error}
        </p>
      )}
      {editing && (
        <ClientMatchForm
          label={t("psa.customers.default")}
          initialClient={
            saved.default_client_id === null ? null : { id: saved.default_client_id, name: saved.default_client_name ?? "" }
          }
          initialSite={saved.default_site_id}
          withSite
          onCancel={() => setEditing(false)}
          onSave={async (client, site) => {
            await api.savePsaDefaultClient({ client_id: client.id, psa_site_id: site });
            setEditing(false);
            onSaved();
          }}
        />
      )}
    </div>
  );
}
