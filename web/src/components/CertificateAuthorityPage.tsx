"use client";

import Link from "next/link";
import { useCallback, useState } from "react";

import { rich, t } from "@/i18n";
import { ApiError, type CaPeerState, type CaRotation, api, describeError } from "@/lib/api";
import {
  authorityStatus,
  expiryWarning,
  formatDate,
  machineWord,
  nextStep,
  progressSentence,
  reinstallHint,
  stepExplanation,
} from "@/lib/certificateAuthority";
import { administersEverything, multiCustomer, useMe } from "@/lib/me";
import { TOKENS_PATH } from "@/lib/routes";
import { usePolledData } from "@/lib/usePolledData";

import styles from "./CertificateAuthority.module.css";
import { type Column, DataTable } from "./ui/DataTable";
import { FactsBox } from "./ui/FactsBox";
import { InlineConfirm } from "./ui/InlineConfirm";
import { Banner, ErrorState, LoadingBlock, NoPermission, PageHeader, RefreshLine } from "./ui/PageState";
import { RelativeTime } from "./ui/RelativeTime";
import { Label } from "./ui/StatusLabel";
import { formatDay } from "./ui/time";

/** Polling while a change is under way. */
const ACTIVE_MS = 10_000;

/** `/settings/certificate-authority/`: global administrators only, like the API (ADR-0088). */
export function CertificateAuthorityPage() {
  const me = useMe();
  if (!administersEverything(me)) {
    return (
      <NoPermission title={t("authority.noPermission")}>
        {multiCustomer(me) ? t("authority.noPermissionNoteCustomers") : t("authority.noPermissionNote")}
      </NoPermission>
    );
  }
  return <CertificateAuthority />;
}

/** What a refused start or step says. */
function failureText(e: unknown, what: string): string {
  return e instanceof ApiError && e.status < 500 && e.status !== 0 && e.status !== 401 && e.status !== 403
    ? t("authority.failure", { what, message: e.message })
    : describeError(e);
}

function CertificateAuthority() {
  const [active, setActive] = useState(false);
  const fetcher = useCallback(async (signal: AbortSignal) => {
    const view = await api.certificateAuthority(signal);
    setActive(view.rotation !== null);
    return view;
  }, []);
  const intervalMs = active ? ACTIVE_MS : undefined;
  const { data, error, stale, updatedAt, refresh } = usePolledData(fetcher, { intervalMs });

  const header = (
    <PageHeader
      title={t("authority.title")}
      meta={updatedAt !== null || error !== null ? <RefreshLine updatedAt={updatedAt} stale={stale} intervalMs={intervalMs} /> : undefined}
    />
  );
  if (error instanceof ApiError && error.status === 403) {
    return (
      <div className={styles.page}>
        {header}
        <NoPermission title={t("authority.noPermission")} />
      </div>
    );
  }
  if (!data) {
    return (
      <div className={styles.page}>
        {header}
        {error ? (
          <ErrorState what={t("authority.what")} error={describeError(error)} onRetry={refresh} />
        ) : (
          <LoadingBlock />
        )}
      </div>
    );
  }

  const now = updatedAt ?? new Date();
  const [status, tone] = authorityStatus(data);
  const warning = expiryWarning(data, now);
  const changed = () => {
    setActive(true);
    refresh();
  };
  return (
    <div className={styles.page}>
      {header}
      {data.restored_retired_at && (
        <Banner tone="bad">
          {t("authority.restoredBanner", { date: formatDate(data.restored_retired_at) })}
        </Banner>
      )}
      {warning && <Banner tone="warn">{warning}</Banner>}
      <section className={styles.box} aria-labelledby="status-title">
        <h2 id="status-title" className={styles.title}>
          <Label tone={tone}>{status}</Label>
        </h2>
        <p className={styles.note}>{t("authority.about")}</p>
        <FactsBox
          columns={2}
          facts={[
            { label: t("authority.facts.created"), value: formatDate(data.created_at) },
            { label: t("authority.facts.expires"), value: formatDate(data.expires_at) },
          ]}
          details={[
            { label: t("authority.facts.fingerprint"), value: data.fingerprint, mono: true },
            ...(data.rotation?.other_fingerprint
              ? [
                  {
                    label:
                      data.rotation.stage === "introduced" ? t("authority.facts.new") : t("authority.facts.old"),
                    value: data.rotation.other_fingerprint,
                    mono: true,
                  },
                ]
              : []),
          ]}
        />
        {!data.rotation && <StartChange onChanged={changed} />}
      </section>
      {data.rotation && <Change rotation={data.rotation} now={now} onChanged={changed} />}
      {data.cut_off.length > 0 && <CutOff peers={data.cut_off} now={now} />}
    </div>
  );
}

/** The two ways to start a change: planned, or an emergency. */
function StartChange({ onChanged }: { onChanged: () => void }) {
  const start = (emergency: boolean) => async () => {
    await api.startCertificateAuthorityChange(emergency);
    onChanged();
  };
  return (
    <div className={styles.start}>
      <h3 className={styles.subtitle}>{t("authority.start.title")}</h3>
      <p className={styles.note}>{t("authority.start.note")}</p>
      <div className={styles.actions}>
        <InlineConfirm
          trigger={t("authority.start.trigger")}
          triggerVariant="k"
          confirmVariant="k"
          title={t("authority.start.confirmTitle")}
          consequence={t("authority.start.consequence")}
          confirmLabel={t("authority.start.confirm")}
          pendingLabel={t("authority.starting")}
          onConfirm={start(false)}
          describeError={(e) => failureText(e, t("authority.start.failed"))}
        />
        <InlineConfirm
          trigger={t("authority.emergency.trigger")}
          triggerVariant="default"
          title={t("authority.emergency.title")}
          consequence={t("authority.emergency.consequence")}
          acknowledge={t("authority.emergency.acknowledge")}
          confirmLabel={t("authority.emergency.confirm")}
          pendingLabel={t("authority.starting")}
          onConfirm={start(true)}
          describeError={(e) => failureText(e, t("authority.start.failed"))}
        />
      </div>
    </div>
  );
}

/** The change under way: its step, who has moved, who is waited for, and the next step. */
function Change({ rotation, now, onChanged }: { rotation: CaRotation; now: Date; onChanged: () => void }) {
  const step = nextStep(rotation);
  const percent = rotation.total > 0 ? Math.floor((rotation.done / rotation.total) * 100) : 100;
  return (
    <section className={styles.box} aria-labelledby="change-title">
      <h2 id="change-title" className={styles.title}>
        {rotation.emergency ? t("authority.change.emergencyTitle") : t("authority.change.title")}
      </h2>
      <p className={styles.note}>{stepExplanation(rotation, now)}</p>
      <div className={styles.progress}>
        <progress max={Math.max(rotation.total, 1)} value={rotation.total > 0 ? rotation.done : 1} aria-label={t("authority.change.progress")} />
        <span>{percent}&nbsp;%</span>
      </div>
      <p className={styles.sentence}>{progressSentence(rotation)}</p>
      <FactsBox
        columns={2}
        facts={[
          { label: t("authority.change.started"), value: formatDay(new Date(rotation.started_at), now) },
          ...(rotation.switched_at
            ? [{ label: t("authority.change.switched"), value: formatDay(new Date(rotation.switched_at), now) }]
            : []),
          ...(rotation.distrust_at
            ? [{ label: t("authority.change.distrust"), value: formatDay(new Date(rotation.distrust_at), now) }]
            : []),
        ]}
      />
      {rotation.waiting.length > 0 && <Waiting peers={rotation.waiting} now={now} />}
      <div className={styles.actions}>
        <InlineConfirm
          trigger={step.label}
          triggerVariant="default"
          title={step.title}
          consequence={step.consequence}
          confirmLabel={step.confirm}
          pendingLabel={t("authority.change.working")}
          onConfirm={async () => {
            await api.nextCertificateAuthorityStep();
            onChanged();
          }}
          describeError={(e) => failureText(e, t("authority.change.failed"))}
        />
      </div>
    </section>
  );
}

function lastContact(peer: CaPeerState, now: Date) {
  return peer.last_seen_at ? <RelativeTime value={peer.last_seen_at} now={now} /> : t("authority.never");
}

function Waiting({ peers, now }: { peers: CaPeerState[]; now: Date }) {
  const columns: Column<CaPeerState>[] = [
    {
      key: "name",
      header: t("authority.column.name"),
      cell: (p) => (
        <span className={styles.name}>
          {p.name}
          {p.too_old && (
            <Label tone="warn" small>
              {t("authority.tooOld")}
            </Label>
          )}
        </span>
      ),
    },
    { key: "kind", header: t("authority.column.kind"), cell: (p) => machineWord(p.role) },
    { key: "seen", header: t("authority.column.seen"), cell: (p) => lastContact(p, now) },
    { key: "until", header: t("authority.column.until"), cell: (p) => formatDate(p.not_after) },
  ];
  return <DataTable label={t("authority.waitedFor")} columns={columns} rows={peers} rowKey={(p) => `${p.role}:${p.peer_id}`} />;
}

/** Machines that missed an emergency change. */
function CutOff({ peers, now }: { peers: CaPeerState[]; now: Date }) {
  const columns: Column<CaPeerState>[] = [
    { key: "name", header: t("authority.column.name"), cell: (p) => p.name },
    { key: "kind", header: t("authority.column.kind"), cell: (p) => machineWord(p.role) },
    { key: "seen", header: t("authority.column.seen"), cell: (p) => lastContact(p, now) },
  ];
  return (
    <section className={styles.box} aria-labelledby="cutoff-title">
      <h2 id="cutoff-title" className={styles.title}>
        <Label tone="bad">{t("authority.needsInstall")}</Label>
      </h2>
      <p className={styles.note}>
        {rich("authority.cutOffNote", {
          hint: reinstallHint(),
          link: <Link href={TOKENS_PATH}>{t("authority.addServers")}</Link>,
        })}
      </p>
      <DataTable label={t("authority.needsInstall")} columns={columns} rows={peers} rowKey={(p) => `${p.role}:${p.peer_id}`} />
    </section>
  );
}
