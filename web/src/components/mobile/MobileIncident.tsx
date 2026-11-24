"use client";

import Link from "next/link";
import { useSearchParams } from "next/navigation";
import { useCallback, useEffect, useRef, useState } from "react";

import { Button } from "@/components/ui/Button";
import { Banner, ErrorState, LoadingBlock } from "@/components/ui/PageState";
import { Label } from "@/components/ui/StatusLabel";
import { formatDuration, formatSince } from "@/components/ui/time";
import { t } from "@/i18n";
import { type IncidentDetail, api, describeError } from "@/lib/api";
import { incidentTitle, isOffline } from "@/lib/incidents";
import { MOBILE_PATH, SILENCE_MINUTES, silenceFor } from "@/lib/mobile";
import { incidentHref } from "@/lib/routes";
import { sensorOf } from "@/lib/sensors";
import { usePolledData } from "@/lib/usePolledData";
import { UUID } from "@/lib/uuid";

import styles from "./Mobile.module.css";

type Target = "check" | "server";

/**
 * One incident on a phone: what it is, and the two things one does from a
 * phone at night, acknowledge and silence for a while. `?silence=1`, from
 * a notification's button, opens the silence choices.
 */
export function MobileIncident() {
  const params = useSearchParams();
  const raw = params.get("id");
  const id = raw && UUID.test(raw) ? raw : null;
  const fetcher = useCallback(
    (signal: AbortSignal) => (id ? api.incident(id, signal) : Promise.reject(new Error("no id"))),
    [id],
  );
  const { data, error, refresh } = usePolledData<IncidentDetail>(fetcher, { enabled: id !== null });
  const [busy, setBusy] = useState(false);
  const [result, setResult] = useState<{ tone: "ok" | "bad"; text: string } | null>(null);
  const [silencing, setSilencing] = useState(params.get("silence") === "1");
  const [target, setTarget] = useState<Target>("check");
  const silenceRef = useRef<HTMLElement>(null);

  useEffect(() => {
    if (silencing) silenceRef.current?.scrollIntoView?.({ block: "start" });
  }, [silencing, data]);

  if (!id) return <Banner tone="bad">{t("mobile.notFound")}</Banner>;
  if (!data) return error ? <ErrorState what={t("mobile.alertWhat")} error={describeError(error)} /> : <LoadingBlock />;

  const now = new Date();
  const open = data.state === "open";
  const offline = isOffline(data);
  const canAcknowledge = open && data.can_acknowledge === true && !data.acknowledged_at;

  async function acknowledge() {
    setBusy(true);
    setResult(null);
    try {
      await api.acknowledgeIncidents([data!.id], "");
      setResult({ tone: "ok", text: t("mobile.acknowledged") });
      refresh();
    } catch (e) {
      setResult({ tone: "bad", text: describeError(e) });
    } finally {
      setBusy(false);
    }
  }

  async function silence(minutes: number) {
    const incident = data!;
    setBusy(true);
    setResult(null);
    try {
      let check: { name: string; instance: string } | null = null;
      if (target === "check" && incident.server_id && !offline) {
        const sensor = sensorOf((await api.serverSensors(incident.server_id)).sensors, incident.metric);
        check = sensor ? { name: sensor.name, instance: sensor.instance } : null;
      }
      await api.createSilence(silenceFor(incident, check, minutes, new Date(), t("mobile.silenceReason")));
      setResult({
        tone: "ok",
        text: t(check ? "mobile.silencedCheck" : "mobile.silencedServer", {
          name: incident.hostname,
          duration: formatDuration(minutes * 60),
        }),
      });
      setSilencing(false);
      refresh();
    } catch (e) {
      setResult({ tone: "bad", text: describeError(e) });
    } finally {
      setBusy(false);
    }
  }

  return (
    <>
      <Link href={MOBILE_PATH} className={styles.link}>
        {t("mobile.back")}
      </Link>
      <div className={styles.section}>
        <span className={styles.cardTop}>
          <Label tone={!open ? "ok" : data.severity === "critical" ? "bad" : "warn"} small>
            {open ? t(`sensors.severity.${data.severity}`) : t("mobile.resolved")}
          </Label>
        </span>
        <h1 className={styles.title}>{incidentTitle(data)}</h1>
        {data.reason && <p className={styles.muted}>{data.reason}</p>}
      </div>
      <dl className={styles.facts}>
        <dt>{t("mobile.where")}</dt>
        <dd>
          {data.hostname} · {data.site_name}
        </dd>
        <dt>{t("mobile.opened")}</dt>
        <dd>{formatSince(new Date(data.opened_at), now)}</dd>
        {data.resolved_at && (
          <>
            <dt>{t("mobile.resolvedAt")}</dt>
            <dd>{formatSince(new Date(data.resolved_at), now)}</dd>
          </>
        )}
        <dt>{t("mobile.onIt")}</dt>
        <dd>{data.acknowledged_by ? t("mobile.acknowledgedBy", { name: data.acknowledged_by }) : t("mobile.nobody")}</dd>
        {open && data.silenced_until && (
          <>
            <dt>{t("mobile.silencedUntil")}</dt>
            <dd>{formatSince(new Date(data.silenced_until), now)}</dd>
          </>
        )}
      </dl>
      {result && <Banner tone={result.tone}>{result.text}</Banner>}
      {open && (
        <div className={styles.actions}>
          {canAcknowledge && (
            <Button variant="k" size="large" disabled={busy} onClick={() => void acknowledge()}>
              {t("mobile.acknowledge")}
            </Button>
          )}
          {!silencing && (
            <Button size="large" disabled={busy} onClick={() => setSilencing(true)}>
              {t("mobile.silence")}
            </Button>
          )}
        </div>
      )}
      {open && silencing && (
        <section ref={silenceRef} className={styles.section} aria-labelledby="silence-title">
          <h2 id="silence-title" className={styles.sectionTitle}>
            {t("mobile.silenceTitle")}
          </h2>
          {!offline && data.server_id && (
            <div className={styles.segmented} role="group" aria-label={t("mobile.silenceWhat")}>
              <button type="button" aria-pressed={target === "check"} onClick={() => setTarget("check")}>
                {t("mobile.thisCheck")}
              </button>
              <button type="button" aria-pressed={target === "server"} onClick={() => setTarget("server")}>
                {t("mobile.wholeServer")}
              </button>
            </div>
          )}
          <p className={styles.muted}>{t("mobile.silenceHint")}</p>
          <div className={styles.choices}>
            {SILENCE_MINUTES.map((minutes) => (
              <Button key={minutes} size="large" disabled={busy} onClick={() => void silence(minutes)}>
                {formatDuration(minutes * 60)}
              </Button>
            ))}
          </div>
          <div className={styles.actions}>
            <Button variant="ghost" size="large" onClick={() => setSilencing(false)}>
              {t("mobile.cancel")}
            </Button>
          </div>
        </section>
      )}
      <Link href={incidentHref(data.id)} className={styles.link}>
        {t("mobile.fullPage")}
      </Link>
    </>
  );
}
