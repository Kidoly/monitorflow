"use client";

import { type FormEvent, useState } from "react";

import { t } from "@/i18n";
import { type ServerDetail, type SnmpCredentials, type SnmpCredentialsRequest, api, describeError } from "@/lib/api";
import { Button, fieldClass } from "@/components/ui/Button";

import styles from "./AdminPages.module.css";

const AUTH = ["sha", "sha256", "sha384", "sha512", "md5"] as const;
const PRIVACY = ["aes128", "aes256", "aes256c"] as const;
const AUTH_NAMES: Record<string, string> = { sha: "SHA-1", sha256: "SHA-256", sha384: "SHA-384", sha512: "SHA-512", md5: "MD5" };
const PRIVACY_NAMES: Record<string, string> = { aes128: "AES-128", aes256: "AES-256", aes256c: "AES-256 (Cisco)" };

/** New credentials: v2c, port 161. */
export const NEW_CREDENTIALS: SnmpCredentialsRequest = { version: "v2c", port: 161 };

/** The form's state for stored credentials, secrets left empty. */
export function fromStored(stored: SnmpCredentials): SnmpCredentialsRequest {
  return {
    version: stored.version,
    port: stored.port,
    ...(stored.user && { user: stored.user }),
    ...(stored.auth_protocol && { auth_protocol: stored.auth_protocol }),
    ...(stored.priv_protocol && { priv_protocol: stored.priv_protocol }),
  };
}

/**
 * What is wrong with `value`, or null; `keep` when secrets left empty keep
 * the stored ones. The API checks again.
 */
export function snmpProblem(value: SnmpCredentialsRequest, keep: boolean): string | null {
  const short = (p?: string) => p !== undefined && p !== "" && p.length < 8;
  if (value.version === "v2c") return !value.community && !keep ? t("devices.snmp.needCommunity") : null;
  if (!value.user?.trim()) return t("devices.snmp.needUser");
  if (value.auth_protocol && (short(value.auth_password) || (!value.auth_password && !keep))) return t("devices.snmp.shortPassword");
  if (value.priv_protocol && (short(value.priv_password) || (!value.priv_password && !keep))) return t("devices.snmp.shortPassword");
  return null;
}

/** The request for `value`: what its version uses, empty secrets left out. */
export function snmpRequest(value: SnmpCredentialsRequest): SnmpCredentialsRequest {
  const keep = (v?: string) => (v ? v : undefined);
  const base = { version: value.version, port: value.port ?? 161 };
  if (value.version === "v2c") return { ...base, ...(value.community && { community: value.community }) };
  return Object.fromEntries(
    Object.entries({
      ...base,
      user: value.user?.trim(),
      auth_protocol: keep(value.auth_protocol),
      auth_password: value.auth_protocol ? keep(value.auth_password) : undefined,
      priv_protocol: value.auth_protocol ? keep(value.priv_protocol) : undefined,
      priv_password: value.auth_protocol && value.priv_protocol ? keep(value.priv_password) : undefined,
    }).filter(([, v]) => v !== undefined),
  ) as unknown as SnmpCredentialsRequest;
}

/** The fields of SNMP credentials. */
export function SnmpFields({
  value,
  onChange,
  keep = false,
}: {
  value: SnmpCredentialsRequest;
  onChange: (value: SnmpCredentialsRequest) => void;
  /** Editing stored credentials: empty secrets keep theirs. */
  keep?: boolean;
}) {
  const set = (patch: Partial<SnmpCredentialsRequest>) => onChange({ ...value, ...patch });
  const secretHint = keep ? t("devices.snmp.keepHint") : t("devices.snmp.passwordHint");
  return (
    <>
      <label className={styles.field}>
        {t("devices.snmp.version")}
        <select className={fieldClass} value={value.version} onChange={(e) => set({ version: e.target.value as "v2c" | "v3" })}>
          <option value="v2c">{t("devices.snmp.v2c")}</option>
          <option value="v3">{t("devices.snmp.v3")}</option>
        </select>
      </label>
      {value.version === "v2c" ? (
        <label className={styles.field}>
          {t("devices.snmp.community")}
          <input
            className={fieldClass}
            type="password"
            autoComplete="off"
            value={value.community ?? ""}
            onChange={(e) => set({ community: e.target.value })}
          />
          <span className={styles.sectionNote}>{keep ? t("devices.snmp.keepHint") : t("devices.snmp.communityHint")}</span>
        </label>
      ) : (
        <>
          <label className={styles.field}>
            {t("devices.snmp.user")}
            <input className={fieldClass} autoComplete="off" value={value.user ?? ""} onChange={(e) => set({ user: e.target.value })} />
          </label>
          <label className={styles.field}>
            {t("devices.snmp.auth")}
            <select className={fieldClass} value={value.auth_protocol ?? ""} onChange={(e) => set({ auth_protocol: e.target.value || undefined })}>
              <option value="">{t("devices.snmp.none")}</option>
              {AUTH.map((p) => (
                <option key={p} value={p}>
                  {AUTH_NAMES[p]}
                </option>
              ))}
            </select>
          </label>
          {value.auth_protocol && (
            <>
              <label className={styles.field}>
                {t("devices.snmp.authPassword")}
                <input
                  className={fieldClass}
                  type="password"
                  autoComplete="off"
                  value={value.auth_password ?? ""}
                  onChange={(e) => set({ auth_password: e.target.value })}
                />
                <span className={styles.sectionNote}>{secretHint}</span>
              </label>
              <label className={styles.field}>
                {t("devices.snmp.privacy")}
                <select className={fieldClass} value={value.priv_protocol ?? ""} onChange={(e) => set({ priv_protocol: e.target.value || undefined })}>
                  <option value="">{t("devices.snmp.none")}</option>
                  {PRIVACY.map((p) => (
                    <option key={p} value={p}>
                      {PRIVACY_NAMES[p]}
                    </option>
                  ))}
                </select>
              </label>
              {value.priv_protocol && (
                <label className={styles.field}>
                  {t("devices.snmp.privPassword")}
                  <input
                    className={fieldClass}
                    type="password"
                    autoComplete="off"
                    value={value.priv_password ?? ""}
                    onChange={(e) => set({ priv_password: e.target.value })}
                  />
                  <span className={styles.sectionNote}>{secretHint}</span>
                </label>
              )}
            </>
          )}
        </>
      )}
      <label className={styles.field}>
        {t("devices.snmp.port")}
        <input
          className={fieldClass}
          inputMode="numeric"
          value={String(value.port ?? 161)}
          onChange={(e) => set({ port: Number(e.target.value.replace(/\D/g, "")) || 161 })}
        />
      </label>
    </>
  );
}

/** "v3, user monitor, port 161", or "Not set". */
export function snmpSummary(stored: SnmpCredentials | null): string {
  if (!stored) return t("devices.snmp.factNone");
  return stored.version === "v2c"
    ? t("devices.snmp.factV2c", { port: String(stored.port) })
    : t("devices.snmp.factV3", { user: stored.user ?? "", port: String(stored.port) });
}

/** Change a device's SNMP credentials. */
export function EditSnmp({
  server,
  stored,
  onClose,
  onSaved,
}: {
  server: ServerDetail;
  stored: SnmpCredentials | null;
  onClose: () => void;
  onSaved: () => void;
}) {
  const [value, setValue] = useState<SnmpCredentialsRequest>(stored ? fromStored(stored) : NEW_CREDENTIALS);
  const [problem, setProblem] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  const keep = stored !== null && stored.version === value.version;

  async function submit(event: FormEvent) {
    event.preventDefault();
    const found = snmpProblem(value, keep);
    if (found) return setProblem(found);
    setBusy(true);
    setProblem(null);
    try {
      await api.setDeviceSnmp(server.id, snmpRequest(value));
      onSaved();
    } catch (e) {
      setProblem(t("devices.snmp.failed", { error: describeError(e) }));
      setBusy(false);
    }
  }

  return (
    <form className={styles.panel} onSubmit={(e) => void submit(e)} aria-label={t("devices.snmp.editTitle", { name: server.hostname })} noValidate>
      <h2 className={styles.sectionTitle}>{t("devices.snmp.editTitle", { name: server.hostname })}</h2>
      <SnmpFields value={value} onChange={setValue} keep={keep} />
      <div className={styles.formFoot}>
        <Button variant="k" type="submit" disabled={busy}>
          {busy ? t("devices.snmp.saving") : t("devices.snmp.save")}
        </Button>
        <Button onClick={onClose} disabled={busy}>
          {t("devices.add.cancel")}
        </Button>
      </div>
      {problem && (
        <p className={styles.formError} role="alert">
          {problem}
        </p>
      )}
    </form>
  );
}
