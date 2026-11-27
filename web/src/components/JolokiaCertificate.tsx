"use client";

import { useState } from "react";

import { t } from "@/i18n";
import { type JmxTestRequest, type JmxTested, type SensorConfigEntry, api, describeError } from "@/lib/api";
import { Button } from "@/components/ui/Button";
import { Switch } from "@/components/ui/Switch";

import styles from "./AdminPages.module.css";

/** The digits of a fingerprint, however it is written. */
const digits = (value: string | null | undefined) => (value ?? "").replace(/[^0-9a-f]/gi, "").toLowerCase();

/** Whether two fingerprints are the same one. */
export const sameFingerprint = (a: string | null | undefined, b: string | null | undefined) =>
  digits(a) !== "" && digits(a) === digits(b);

/** A test of a Java check's settings: empty values left out. */
export function jmxTestRequest(values: Record<string, string>, probe?: string): JmxTestRequest {
  const text = (key: string) => values[key]?.trim() || undefined;
  const port = Number(text("port"));
  return {
    ...(probe && { probe }),
    ...(text("host") && { host: text("host") }),
    ...(port > 0 && { port }),
    ...(text("path") && { path: text("path") }),
    ...(text("user") && { user: text("user") }),
    ...(values.password && { password: values.password }),
    ...(text("fingerprint") && { fingerprint: text("fingerprint") }),
  };
}

/** Whether a test showed a certificate nobody trusted yet. */
export function needsJolokiaTrust(tested: JmxTested | null, pinned: string | undefined): boolean {
  const certificate = tested?.certificate;
  return Boolean(certificate && !certificate.by_roots && !sameFingerprint(pinned, certificate.fingerprint));
}

/**
 * Tests a Jolokia's HTTPS certificate from the probe of a Java check
 * (ADR-0183), shows it and lets a person trust it by its fingerprint, as
 * for vCenter: `onTrust` gets it, or null to take it back.
 */
export function JolokiaCertificate({
  serverId,
  request,
  pinned,
  onTrust,
  onTested,
  reads = true,
}: {
  serverId: string;
  request: JmxTestRequest;
  pinned: string | undefined;
  onTrust: (fingerprint: string | null) => void;
  onTested?: (tested: JmxTested | null) => void;
  /** Whether to say what Jolokia answered: not without the password. */
  reads?: boolean;
}) {
  const [tested, setTested] = useState<JmxTested | null>(null);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);

  async function test() {
    setBusy(true);
    setError(null);
    try {
      const result = await api.testJmx(serverId, request);
      setTested(result);
      onTested?.(result);
    } catch (e) {
      setTested(null);
      onTested?.(null);
      setError(t("checks.jolokia.testFailed", { error: describeError(e) }));
    } finally {
      setBusy(false);
    }
  }

  const certificate = tested?.certificate;
  return (
    <div className={styles.field}>
      <div className={styles.formFoot}>
        <Button onClick={() => void test()} disabled={busy}>
          {busy ? t("checks.jolokia.testing") : t("checks.jolokia.test")}
        </Button>
      </div>
      {error && (
        <p className={styles.formError} role="alert">
          {error}
        </p>
      )}
      {tested && !tested.answers && (
        <p className={styles.formError} role="status">
          {t("checks.jolokia.noAnswer", {
            probe: tested.probe === "central" ? t("devices.from.short.central") : tested.probe,
            error: tested.error ?? "",
          })}
        </p>
      )}
      {tested?.answers && certificate && (
        <div className={styles.sectionNote} role="status">
          {certificate.by_roots ? (
            <p>{t("checks.jolokia.certificateTrusted", { subject: certificate.subject })}</p>
          ) : (
            <>
              <p>{t("checks.jolokia.certificate", { subject: certificate.subject })}</p>
              <p>
                <code className={styles.fingerprint}>{certificate.fingerprint}</code>
              </p>
              <p>{t("checks.jolokia.certificateUntrusted")}</p>
              <Switch
                checked={sameFingerprint(pinned, certificate.fingerprint)}
                onChange={(on) => onTrust(on ? certificate.fingerprint : null)}
                label={t("checks.jolokia.trust")}
              />
            </>
          )}
          {reads && tested.reads === true && (
            <p>{t("checks.jolokia.reads", { vm: tested.vm ?? "Java", version: tested.version ?? "" })}</p>
          )}
          {reads && tested.reads === false && (
            <p className={styles.formError}>{t("checks.jolokia.readsFailed", { error: tested.error ?? "" })}</p>
          )}
        </div>
      )}
    </div>
  );
}

/**
 * A Java check's certificate seen again, for when it changed: trusting the
 * new one saves its fingerprint in the check, the password kept.
 */
export function NewJolokiaCertificate({
  serverId,
  row,
  onClose,
  onSaved,
}: {
  serverId: string;
  row: SensorConfigEntry;
  onClose: () => void;
  onSaved: () => void;
}) {
  const params = Object.fromEntries(Object.entries(row.params).map(([k, v]) => [k, String(v)]));
  const [pinned, setPinned] = useState<string | undefined>(params.fingerprint);
  const [tested, setTested] = useState<JmxTested | null>(null);
  const [state, setState] = useState<"idle" | "saving" | "saved">("idle");
  const [error, setError] = useState<string | null>(null);
  const probe = row.probe === "relay" ? (row.probe_relay_id ?? undefined) : (row.probe ?? undefined);
  const chosen = tested?.certificate && !tested.certificate.by_roots && sameFingerprint(pinned, tested.certificate.fingerprint);
  const changed = chosen && !sameFingerprint(pinned, params.fingerprint);

  async function save() {
    setState("saving");
    setError(null);
    try {
      const { kind, name, instance, interval_ms, timeout_ms, enabled } = row;
      await api.updateSensor(row.id, {
        kind,
        name,
        instance,
        interval_ms,
        timeout_ms,
        enabled,
        params: { ...row.params, fingerprint: pinned ?? "" },
        ...(probe && { probe }),
      });
      setState("saved");
      onSaved();
    } catch (e) {
      setState("idle");
      setError(t("checks.jolokia.failed", { error: describeError(e) }));
    }
  }

  return (
    <div className={styles.panel}>
      <JolokiaCertificate
        serverId={serverId}
        request={jmxTestRequest({ ...params, password: "", fingerprint: params.fingerprint ?? "" }, probe)}
        pinned={pinned}
        onTrust={(fingerprint) => setPinned(fingerprint ?? params.fingerprint)}
        onTested={setTested}
        reads={false}
      />
      {error && (
        <p className={styles.formError} role="alert">
          {error}
        </p>
      )}
      {state === "saved" && <p className={styles.sectionNote}>{t("checks.jolokia.saved")}</p>}
      <div className={styles.formFoot}>
        {changed && state !== "saved" && (
          <Button variant="k" onClick={() => void save()} disabled={state === "saving"}>
            {state === "saving" ? t("checks.jolokia.saving") : t("checks.jolokia.save")}
          </Button>
        )}
        <Button onClick={onClose} disabled={state === "saving"}>
          {t("checks.jolokia.close")}
        </Button>
      </div>
    </div>
  );
}
