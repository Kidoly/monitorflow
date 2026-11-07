"use client";

import Link from "next/link";
import { useCallback, useState } from "react";

import { ApiError, type CaPeerState, type CaRotation, api, describeError } from "@/lib/api";
import {
  REINSTALL_HINT,
  authorityStatus,
  expiryWarning,
  formatDate,
  machineWord,
  nextStep,
  progressSentence,
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

const TITLE = "The certificate authority needs a global administrator";

/** `/admin/certificate-authority/`: global administrators only, like the API (ADR-0088). */
export function CertificateAuthorityPage() {
  const me = useMe();
  if (!administersEverything(me)) {
    return (
      <NoPermission title={TITLE}>
        It gives every server and relay{multiCustomer(me) && " of every customer"} its certificate. Ask a global administrator for a
        role.
      </NoPermission>
    );
  }
  return <CertificateAuthority />;
}

/** What a refused start or step says. */
function failureText(e: unknown, what: string): string {
  return e instanceof ApiError && e.status < 500 && e.status !== 0 && e.status !== 401 && e.status !== 403
    ? `${what}: ${e.message}.`
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
      title="Certificate authority"
      meta={
        <>
          Global administrators only
          {(updatedAt !== null || error !== null) && (
            <>
              {" · "}
              <RefreshLine updatedAt={updatedAt} stale={stale} intervalMs={intervalMs} />
            </>
          )}
        </>
      }
    />
  );
  if (error instanceof ApiError && error.status === 403) {
    return (
      <div className={styles.page}>
        {header}
        <NoPermission title={TITLE} />
      </div>
    );
  }
  if (!data) {
    return (
      <div className={styles.page}>
        {header}
        {error ? (
          <ErrorState what="the certificate authority" error={describeError(error)} onRetry={refresh} />
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
          A restore brought back a certificate authority this server retired on {formatDate(data.restored_retired_at)}.
          Servers and relays that moved to the newer one cannot connect until they are installed again. Restore a
          backup taken after that date if there is one, or start a change now.
        </Banner>
      )}
      {warning && <Banner tone="warn">{warning}</Banner>}
      <section className={styles.box} aria-labelledby="status-title">
        <h2 id="status-title" className={styles.title}>
          <Label tone={tone}>{status}</Label>
        </h2>
        <p className={styles.note}>
          The certificate authority of this server gives every server and relay the certificate it uses to connect
          securely.
        </p>
        <FactsBox
          columns={2}
          facts={[
            { label: "Created", value: formatDate(data.created_at) },
            { label: "Expires", value: formatDate(data.expires_at) },
          ]}
          details={[
            { label: "Fingerprint (SHA-256)", value: data.fingerprint, mono: true },
            ...(data.rotation?.other_fingerprint
              ? [
                  {
                    label:
                      data.rotation.stage === "introduced"
                        ? "New certificate authority"
                        : "Old certificate authority",
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
      <h3 className={styles.subtitle}>Change the certificate authority</h3>
      <p className={styles.note}>
        A planned change takes two steps. First every server and relay learns the new certificate authority over its
        secure connection, within about an hour. Then the new one gives every machine a new certificate, within about
        an hour, and the old one is retired. Each step goes on by itself once every machine is done; a machine that is
        offline is waited for until its certificate expires, at most 7 days. The whole change takes from a couple of
        hours to about two weeks, with nothing to do on each machine. Install commands made before the switch stop
        working after it: copy a new one from the Add servers page.
      </p>
      <div className={styles.actions}>
        <InlineConfirm
          trigger="Change the certificate authority…"
          triggerVariant="k"
          confirmVariant="k"
          title="Change the certificate authority?"
          consequence="Servers and relays follow by themselves; nothing to do on each machine."
          confirmLabel="Start the change"
          pendingLabel="Starting…"
          onConfirm={start(false)}
          describeError={(e) => failureText(e, "Cannot start the change")}
        />
        <InlineConfirm
          trigger="Emergency change…"
          triggerVariant="default"
          title="Start an emergency change?"
          consequence={
            <>
              Only if the key of the certificate authority may have leaked. The new one gives every server and relay a
              new certificate within the hour, and the old one stops being trusted 24 hours later, or as soon as every
              machine moved. Machines offline until then cannot connect: each must be installed again. Every install
              command made before stops working.
            </>
          }
          acknowledge="I understand that machines that miss it must be installed again."
          confirmLabel="Start the emergency change"
          pendingLabel="Starting…"
          onConfirm={start(true)}
          describeError={(e) => failureText(e, "Cannot start the change")}
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
        {rotation.emergency ? "Emergency change" : "Change"} of the certificate authority
      </h2>
      <p className={styles.note}>{stepExplanation(rotation, now)}</p>
      <div className={styles.progress}>
        <progress max={Math.max(rotation.total, 1)} value={rotation.total > 0 ? rotation.done : 1} aria-label="Progress" />
        <span>{percent} %</span>
      </div>
      <p className={styles.sentence}>{progressSentence(rotation)}</p>
      <FactsBox
        columns={2}
        facts={[
          { label: "Started", value: formatDay(new Date(rotation.started_at), now) },
          ...(rotation.switched_at
            ? [{ label: "Switched", value: formatDay(new Date(rotation.switched_at), now) }]
            : []),
          ...(rotation.distrust_at
            ? [{ label: "Old one stops being trusted", value: formatDay(new Date(rotation.distrust_at), now) }]
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
          pendingLabel="Working…"
          onConfirm={async () => {
            await api.nextCertificateAuthorityStep();
            onChanged();
          }}
          describeError={(e) => failureText(e, "Cannot take the next step")}
        />
      </div>
    </section>
  );
}

function lastContact(peer: CaPeerState, now: Date) {
  return peer.last_seen_at ? <RelativeTime value={peer.last_seen_at} now={now} /> : "never";
}

function Waiting({ peers, now }: { peers: CaPeerState[]; now: Date }) {
  const columns: Column<CaPeerState>[] = [
    {
      key: "name",
      header: "Name",
      cell: (p) => (
        <span className={styles.name}>
          {p.name}
          {p.too_old && (
            <Label tone="warn" small>
              Too old to follow: updates itself
            </Label>
          )}
        </span>
      ),
    },
    { key: "kind", header: "Kind", cell: (p) => machineWord(p.role) },
    { key: "seen", header: "Last contact", cell: (p) => lastContact(p, now) },
    { key: "until", header: "Stops being waited for", cell: (p) => formatDate(p.not_after) },
  ];
  return <DataTable label="Waited for" columns={columns} rows={peers} rowKey={(p) => `${p.role}:${p.peer_id}`} />;
}

/** Machines that missed an emergency change. */
function CutOff({ peers, now }: { peers: CaPeerState[]; now: Date }) {
  const columns: Column<CaPeerState>[] = [
    { key: "name", header: "Name", cell: (p) => p.name },
    { key: "kind", header: "Kind", cell: (p) => machineWord(p.role) },
    { key: "seen", header: "Last contact", cell: (p) => lastContact(p, now) },
  ];
  return (
    <section className={styles.box} aria-labelledby="cutoff-title">
      <h2 id="cutoff-title" className={styles.title}>
        <Label tone="bad">Needs a new install</Label>
      </h2>
      <p className={styles.note}>
        These servers and relays missed an emergency change: their certificate comes from a certificate authority this
        server no longer trusts, so they cannot connect. {REINSTALL_HINT}{" "}
        <Link href={TOKENS_PATH}>Add servers</Link>
      </p>
      <DataTable label="Needs a new install" columns={columns} rows={peers} rowKey={(p) => `${p.role}:${p.peer_id}`} />
    </section>
  );
}
