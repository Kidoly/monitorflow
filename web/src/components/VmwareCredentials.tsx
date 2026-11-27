"use client";

import { type FormEvent, useState } from "react";

import { t } from "@/i18n";
import {
  type ServerDetail,
  type VmwareCredentials,
  type VmwareCredentialsRequest,
  type VmwareTested,
  api,
  describeError,
} from "@/lib/api";
import { Button, fieldClass } from "@/components/ui/Button";
import { Switch } from "@/components/ui/Switch";

import styles from "./AdminPages.module.css";

/** New credentials: port 443. */
export const NEW_VMWARE: VmwareCredentialsRequest = { port: 443 };

/** The fields of vCenter or ESXi credentials (ADR-0156). */
export function VmwareFields({
  value,
  onChange,
  keep = false,
}: {
  value: VmwareCredentialsRequest;
  onChange: (value: VmwareCredentialsRequest) => void;
  /** Editing stored credentials: an empty password keeps the stored one. */
  keep?: boolean;
}) {
  const set = (patch: Partial<VmwareCredentialsRequest>) => onChange({ ...value, ...patch });
  return (
    <>
      <p className={styles.sectionNote}>{t("virtualization.vmware.note")}</p>
      <label className={styles.field}>
        {t("virtualization.vmware.user")}
        <input
          className={fieldClass}
          autoComplete="off"
          spellCheck={false}
          placeholder={t("virtualization.vmware.userPlaceholder")}
          value={value.user ?? ""}
          onChange={(e) => set({ user: e.target.value })}
        />
      </label>
      <label className={styles.field}>
        {t("virtualization.vmware.password")}
        <input
          className={fieldClass}
          type="password"
          autoComplete="new-password"
          value={value.password ?? ""}
          onChange={(e) => set({ password: e.target.value })}
        />
        {keep && <span className={styles.sectionNote}>{t("virtualization.vmware.passwordKeep")}</span>}
      </label>
      <label className={styles.field}>
        {t("virtualization.vmware.port")}
        <input
          className={fieldClass}
          inputMode="numeric"
          value={String(value.port ?? 443)}
          onChange={(e) => set({ port: Number(e.target.value.replace(/\D/g, "")) || 443 })}
        />
      </label>
    </>
  );
}

/** What is missing before credentials can be sent, or null. */
export function vmwareProblem(value: VmwareCredentialsRequest, keep: boolean): string | null {
  if (!value.user?.trim() || (!value.password && !keep)) return t("virtualization.vmware.needUser");
  return null;
}

/** The request for `value`: an empty password left out, so a stored one is kept. */
export function vmwareRequest(value: VmwareCredentialsRequest): VmwareCredentialsRequest {
  return {
    port: value.port ?? 443,
    ...(value.user?.trim() && { user: value.user.trim() }),
    ...(value.password && { password: value.password }),
    ...(value.fingerprint !== undefined && { fingerprint: value.fingerprint }),
  };
}

/** "monitor@vsphere.local, port 443", or "Not set". */
export function vmwareSummary(stored: VmwareCredentials | null): string {
  if (!stored) return t("virtualization.vmware.factNone");
  const values = { user: stored.user, port: String(stored.port) };
  return stored.fingerprint ? t("virtualization.vmware.factPinned", values) : t("virtualization.vmware.factValue", values);
}

/**
 * Tests a connection from the probe, shows the certificate and lets a person
 * trust it by its fingerprint: `onTrust` gets it, or null to take it back.
 */
export function VmwareTest({
  siteId,
  address,
  probe,
  value,
  onTrust,
  onTested,
}: {
  siteId: string;
  address: string;
  probe?: string;
  value: VmwareCredentialsRequest;
  onTrust: (fingerprint: string | null) => void;
  onTested?: (tested: VmwareTested | null) => void;
}) {
  const [tested, setTested] = useState<VmwareTested | null>(null);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);

  async function test() {
    setBusy(true);
    setError(null);
    try {
      const result = await api.testVmware({
        site_id: siteId,
        address: address.trim(),
        ...(probe && { probe }),
        vmware: vmwareRequest(value),
      });
      setTested(result);
      onTested?.(result);
    } catch (e) {
      setTested(null);
      onTested?.(null);
      setError(t("virtualization.vmware.testFailed", { error: describeError(e) }));
    } finally {
      setBusy(false);
    }
  }

  const certificate = tested?.certificate;
  const pinned = Boolean(certificate && value.fingerprint === certificate.fingerprint);
  return (
    <>
      <div className={styles.formFoot}>
        <Button onClick={() => void test()} disabled={busy || !siteId || !address.trim()}>
          {busy ? t("virtualization.vmware.testing") : t("virtualization.vmware.test")}
        </Button>
      </div>
      {error && (
        <p className={styles.formError} role="alert">
          {error}
        </p>
      )}
      {tested && !tested.answers && (
        <p className={styles.formError} role="status">
          {t("virtualization.vmware.noAnswer", {
            probe: tested.probe === "central" ? t("devices.from.short.central") : tested.probe,
            error: tested.error ?? "",
          })}
        </p>
      )}
      {tested?.answers && (
        <div className={styles.sectionNote} role="status">
          <p>{t("virtualization.vmware.answers", { product: tested.product ?? "vSphere" })}</p>
          {certificate &&
            (certificate.by_roots ? (
              <p>{t("virtualization.vmware.certificateTrusted", { subject: certificate.subject })}</p>
            ) : (
              <>
                <p>{t("virtualization.vmware.certificate", { subject: certificate.subject })}</p>
                <p>
                  <code className={styles.fingerprint}>{certificate.fingerprint}</code>
                </p>
                <p>{t("virtualization.vmware.certificateUntrusted")}</p>
                <Switch
                  checked={pinned}
                  onChange={(on) => onTrust(on ? certificate.fingerprint : null)}
                  label={t("virtualization.vmware.trust")}
                />
              </>
            ))}
          {tested.login === true && (
            <p>
              {t("virtualization.vmware.loginOk", {
                vms: String(tested.objects.vm ?? 0),
                hosts: String(tested.objects.host ?? 0),
                datastores: String(tested.objects.datastore ?? 0),
              })}
            </p>
          )}
          {tested.login === false && (
            <p className={styles.formError}>{t("virtualization.vmware.loginFailed", { error: tested.error ?? "" })}</p>
          )}
        </div>
      )}
    </>
  );
}

/** Whether a test showed a certificate nobody trusted yet. */
export function needsTrust(tested: VmwareTested | null, value: VmwareCredentialsRequest): boolean {
  const certificate = tested?.certificate;
  return Boolean(certificate && !certificate.by_roots && value.fingerprint !== certificate.fingerprint);
}

/** Change a device's vCenter credentials, or confirm a new certificate. */
export function EditVmware({
  server,
  stored,
  onClose,
  onSaved,
}: {
  server: ServerDetail;
  stored: VmwareCredentials | null;
  onClose: () => void;
  onSaved: () => void;
}) {
  const [value, setValue] = useState<VmwareCredentialsRequest>(
    stored ? { port: stored.port, user: stored.user } : NEW_VMWARE,
  );
  const [tested, setTested] = useState<VmwareTested | null>(null);
  const [problem, setProblem] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  const keep = stored !== null;
  // What the probe will trust: the fingerprint chosen here, else the stored one.
  const effective = { ...value, fingerprint: value.fingerprint ?? stored?.fingerprint ?? undefined };

  async function submit(event: FormEvent) {
    event.preventDefault();
    const found = vmwareProblem(value, keep) ?? (needsTrust(tested, effective) ? t("virtualization.vmware.needTrust") : null);
    if (found) return setProblem(found);
    setBusy(true);
    setProblem(null);
    try {
      await api.setDeviceVmware(server.id, vmwareRequest(value));
      onSaved();
    } catch (e) {
      setProblem(t("virtualization.vmware.failed", { error: describeError(e) }));
      setBusy(false);
    }
  }

  return (
    <form className={styles.panel} onSubmit={(e) => void submit(e)} aria-label={t("virtualization.vmware.title")} noValidate>
      <VmwareFields value={value} onChange={setValue} keep={keep} />
      <VmwareTest
        siteId={server.site_id}
        address={server.address ?? ""}
        value={effective}
        onTrust={(fingerprint) => setValue({ ...value, fingerprint: fingerprint ?? "" })}
        onTested={setTested}
      />
      {problem && (
        <p className={styles.formError} role="alert">
          {problem}
        </p>
      )}
      <div className={styles.formFoot}>
        <Button variant="k" type="submit" disabled={busy}>
          {busy ? t("virtualization.vmware.saving") : t("virtualization.vmware.save")}
        </Button>
        <Button onClick={onClose} disabled={busy}>
          {t("virtualization.vmware.cancel")}
        </Button>
      </div>
    </form>
  );
}
