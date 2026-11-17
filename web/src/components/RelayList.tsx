"use client";

import Link from "next/link";
import { useCallback, useState } from "react";

import { Button } from "@/components/ui/Button";
import { type Column, DataTable, PrimaryCell } from "@/components/ui/DataTable";
import { Missing } from "@/components/ui/Missing";
import { Banner, EmptyState, ErrorState, PageHeader, RefreshLine, StaleBanner } from "@/components/ui/PageState";
import { RelativeTime } from "@/components/ui/RelativeTime";
import { Label, StatusLabel } from "@/components/ui/StatusLabel";
import { t } from "@/i18n";
import { type Me, type Relay, api, describeError } from "@/lib/api";
import { certificateProblem, certificateState } from "@/lib/certificates";
import { administersCustomer, multiCustomer } from "@/lib/me";
import { relayHref } from "@/lib/routes";
import { usePolledData } from "@/lib/usePolledData";

import { AddRelay, ConfirmRelayNames } from "./AddRelay";
import { AdminLayout } from "./AdminLayout";
import styles from "./AdminPages.module.css";

/** `/settings/relays/`: every relay the user reads. */
export function RelayListPage() {
  return <AdminLayout>{(me) => <RelayList me={me} />}</AdminLayout>;
}

/** Online, Offline, or Revoked once revoked whatever the stream says. */
export function relayStatus(relay: Pick<Relay, "status" | "revoked_at">) {
  return relay.revoked_at ? "revoked" : relay.status;
}

/** "Valid", or a tinted problem once renewal is late or failed; "None" without an unrevoked certificate. */
export function CertificateCell({ notAfter, now }: { notAfter: string | null; now: Date }) {
  if (!notAfter) return <span className={styles.dim}>{t("relays.certificate.none")}</span>;
  const problem = certificateProblem(notAfter, now);
  if (!problem) return <>{t("relays.certificate.valid")}</>;
  return <Label tone={certificateState(notAfter, now) === "failed" ? "bad" : "warn"}>{problem}</Label>;
}

export function RelayList({ me }: { me: Me }) {
  const [adding, setAdding] = useState(false);
  const [confirmed, setConfirmed] = useState<Relay | null>(null);
  const fetcher = useCallback(async (signal: AbortSignal) => (await api.relays(signal)).relays, []);
  const { data: relays, error, stale, failures, updatedAt, refresh } = usePolledData(fetcher);
  const now = updatedAt ?? new Date();

  const columns: Column<Relay>[] = [
    {
      key: "relay",
      header: t("relays.column.relay"),
      cell: (relay) => (
        <PrimaryCell
          title={relay.peer_id}
          href={relayHref(relay.id)}
          sub={[relay.site_name, multiCustomer(me) && relay.customer_name, relay.relay_version && t("relays.version", { version: relay.relay_version })]
            .filter(Boolean)
            .join(" · ")}
        />
      ),
    },
    { key: "status", header: t("relays.column.status"), cell: (relay) => <StatusLabel kind="server" value={relayStatus(relay)} /> },
    {
      key: "agents",
      header: t("relays.column.agents"),
      cell: (relay) => (
        <>
          <strong>{relay.connected_agents}</strong>{" "}
          <span className={styles.dim}>{t("relays.agents", { count: relay.server_count })}</span>
        </>
      ),
    },
    {
      key: "names",
      header: t("relays.column.addresses"),
      cell: (relay) =>
        relay.pending_names ? (
          <Label tone="warn">{t("relays.waitingConfirmation")}</Label>
        ) : relay.server_names.length > 0 ? (
          <span className={styles.mono}>{relay.server_names.join(" · ")}</span>
        ) : (
          <Missing word="none" className={styles.dim} />
        ),
    },
    {
      key: "certificate",
      header: t("relays.column.certificate"),
      cell: (relay) => <CertificateCell notAfter={relay.cert_not_after} now={now} />,
    },
    {
      key: "seen",
      header: t("relays.column.seen"),
      className: styles.dim,
      cell: (relay) => (relay.last_heartbeat ? <RelativeTime value={relay.last_heartbeat} now={now} /> : t("relays.never")),
    },
  ];

  const canEnroll = administersCustomer(me);
  return (
    <div className={styles.page}>
      <PageHeader
        title={t("relays.title")}
        meta={
          relays || error ? <RefreshLine updatedAt={updatedAt} stale={stale} /> : undefined
        }
        // With no relay yet, the empty state below has the button.
        actions={
          canEnroll &&
          !adding &&
          relays?.length !== 0 && (
            <Button variant="k" onClick={() => setAdding(true)}>
              {t("relays.add")}
            </Button>
          )
        }
      />
      {stale && updatedAt && (
        <StaleBanner error={describeError(error)} updatedAt={updatedAt} failures={failures} onRetry={refresh} />
      )}
      {adding && <AddRelay me={me} onDone={() => setAdding(false)} />}
      {relays
        ?.filter((relay) => relay.pending_names && administersCustomer(me, relay.customer_id))
        .map((relay) => (
          <ConfirmRelayNames
            key={relay.id}
            relay={relay}
            onConfirmed={() => {
              setConfirmed(relay);
              refresh();
            }}
          />
        ))}
      {confirmed && (
        <Banner tone="info">
          {t("relays.ready", { name: confirmed.peer_id })}{" "}
          <Link href={relayHref(confirmed.id)}>{t("relays.addServersBehind")}</Link>
        </Banner>
      )}
      {!relays && error ? (
        <ErrorState what={t("relays.what")} error={describeError(error)} onRetry={refresh} />
      ) : relays?.length === 0 ? (
        !adding && (
          <EmptyState
            title={t("relays.empty.title")}
            actions={
              canEnroll && (
                <Button variant="k" onClick={() => setAdding(true)}>
                  {t("relays.add")}
                </Button>
              )
            }
          >
            {t("relays.empty.note")}
          </EmptyState>
        )
      ) : (
        <DataTable
          label={t("relays.title")}
          columns={columns}
          rows={relays ?? []}
          rowKey={(relay) => relay.id}
          loading={!relays}
        />
      )}
    </div>
  );
}
