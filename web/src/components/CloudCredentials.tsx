"use client";

import { type FormEvent, useState } from "react";

import { t } from "@/i18n";
import {
  type CloudCredentials,
  type CloudCredentialsRequest,
  type CloudProvider,
  type CloudTested,
  type ServerDetail,
  api,
  describeError,
} from "@/lib/api";
import { Button, fieldClass } from "@/components/ui/Button";

import styles from "./AdminPages.module.css";

/** What the form holds; regions as typed. */
export interface CloudForm {
  provider: CloudProvider;
  key_id: string;
  secret: string;
  regions: string;
  tenant: string;
  subscription: string;
  project: string;
}

export const NEW_CLOUD: CloudForm = { provider: "aws", key_id: "", secret: "", regions: "", tenant: "", subscription: "", project: "" };

/** Largest key file read, as the server takes. */
const MAX_KEY = 16 * 1024;

/** The service account and project of a Google JSON key, or null when it is not one. */
export function readGcpKey(text: string): { email: string; project: string } | null {
  try {
    const key = JSON.parse(text) as Record<string, unknown>;
    if (key.type !== "service_account" || typeof key.client_email !== "string" || typeof key.private_key !== "string") return null;
    return { email: key.client_email, project: typeof key.project_id === "string" ? key.project_id : "" };
  } catch {
    return null;
  }
}

/** The form of stored credentials: everything but the secret. */
export function cloudForm(stored: CloudCredentials): CloudForm {
  return {
    provider: stored.provider,
    key_id: stored.key_id,
    secret: "",
    regions: stored.regions.join(", "),
    tenant: stored.tenant ?? "",
    subscription: stored.subscription ?? "",
    project: stored.project ?? "",
  };
}

/** Regions as typed: "eu-west-3, us-east-1". */
function regions(text: string): string[] {
  return text
    .split(/[\s,]+/)
    .map((r) => r.trim())
    .filter(Boolean);
}

/** The fields of an AWS account, an Azure subscription or a Google Cloud project (ADR-0183). */
export function CloudFields({
  value,
  onChange,
  keep = false,
}: {
  value: CloudForm;
  onChange: (value: CloudForm) => void;
  /** Editing stored credentials: the provider is fixed, an empty secret keeps the stored one. */
  keep?: boolean;
}) {
  const set = (patch: Partial<CloudForm>) => onChange({ ...value, ...patch });
  const aws = value.provider === "aws";
  const gcp = value.provider === "gcp";
  const [keyError, setKeyError] = useState(false);
  async function chooseKey(file: File | null) {
    setKeyError(false);
    if (!file) return;
    const text = file.size <= MAX_KEY ? await file.text() : "";
    const key = readGcpKey(text);
    if (!key) {
      setKeyError(true);
      return set({ secret: "" });
    }
    set({ secret: text, key_id: key.email, project: value.project.trim() || key.project });
  }
  const text = (key: "key_id" | "tenant" | "subscription" | "regions" | "project", label: string, placeholder?: string) => (
    <label className={styles.field}>
      {label}
      <input
        className={fieldClass}
        autoComplete="off"
        spellCheck={false}
        autoCapitalize="none"
        placeholder={placeholder}
        value={value[key]}
        onChange={(e) => set({ [key]: e.target.value } as Partial<CloudForm>)}
      />
      {key === "regions" && <span className={styles.sectionNote}>{t("cloud.regionsHint")}</span>}
    </label>
  );
  return (
    <>
      <label className={styles.field}>
        {t("cloud.provider")}
        <select
          className={fieldClass}
          value={value.provider}
          disabled={keep}
          onChange={(e) => {
            // Another provider's identity and secret never carry over.
            setKeyError(false);
            set({ provider: e.target.value as CloudProvider, key_id: "", secret: "" });
          }}
        >
          <option value="aws">{t("cloud.provider.aws")}</option>
          <option value="azure">{t("cloud.provider.azure")}</option>
          <option value="gcp">{t("cloud.provider.gcp")}</option>
        </select>
      </label>
      <p className={styles.sectionNote}>{aws ? t("cloud.note.aws") : gcp ? t("cloud.note.gcp") : t("cloud.note.azure")}</p>
      {gcp && (
        <>
          <label className={styles.field}>
            {t("cloud.secret.gcp")}
            <input type="file" accept=".json,application/json" onChange={(e) => void chooseKey(e.target.files?.[0] ?? null)} />
            {keyError && (
              <span className={styles.formError} role="alert">
                {t("cloud.keyBad")}
              </span>
            )}
            {!keyError && value.secret && value.key_id && <span className={styles.sectionNote}>{t("cloud.keyRead", { account: value.key_id })}</span>}
            {!keyError && !value.secret && keep && value.key_id && (
              <span className={styles.sectionNote}>{t("cloud.keyKeep", { account: value.key_id })}</span>
            )}
          </label>
          {text("project", t("cloud.project"), "my-project-123")}
        </>
      )}
      {!gcp && (
        <>
          {!aws && text("tenant", t("cloud.tenant"))}
          {text("key_id", aws ? t("cloud.keyId.aws") : t("cloud.keyId.azure"), aws ? "AKIA…" : undefined)}
          <label className={styles.field}>
            {aws ? t("cloud.secret.aws") : t("cloud.secret.azure")}
            <input
              className={fieldClass}
              type="password"
              autoComplete="new-password"
              value={value.secret}
              onChange={(e) => set({ secret: e.target.value })}
            />
            {keep && <span className={styles.sectionNote}>{t("cloud.secretKeep")}</span>}
          </label>
          {aws ? text("regions", t("cloud.regions"), "eu-west-3") : text("subscription", t("cloud.subscription"))}
        </>
      )}
    </>
  );
}

/** What is missing before credentials can be sent, or null. */
export function cloudProblem(value: CloudForm, keep: boolean): string | null {
  const aws = value.provider === "aws";
  if (value.provider === "gcp") return (!value.secret && !keep) || !value.project.trim() ? t("cloud.need") : null;
  const missing =
    !value.key_id.trim() ||
    (!value.secret && !keep) ||
    (aws ? regions(value.regions).length === 0 : !value.tenant.trim() || !value.subscription.trim());
  return missing ? t("cloud.need") : null;
}

/** The request for `value`: an empty secret left out, so a stored one is kept. */
export function cloudRequest(value: CloudForm): CloudCredentialsRequest {
  const aws = value.provider === "aws";
  if (value.provider === "gcp") {
    return {
      provider: "gcp",
      ...(value.key_id.trim() && { key_id: value.key_id.trim() }),
      ...(value.secret && { secret: value.secret }),
      project: value.project.trim(),
    };
  }
  return {
    provider: value.provider,
    key_id: value.key_id.trim(),
    ...(value.secret && { secret: value.secret }),
    ...(aws ? { regions: regions(value.regions) } : { tenant: value.tenant.trim(), subscription: value.subscription.trim() }),
  };
}

/** "AWS, key AKIA…, eu-west-3, us-east-1", or "Not set". */
export function cloudSummary(stored: CloudCredentials | null): string {
  if (!stored) return t("cloud.factNone");
  if (stored.provider === "gcp") return t("cloud.factGcp", { project: stored.project ?? "" });
  return stored.provider === "aws"
    ? t("cloud.factAws", { key: stored.key_id, regions: stored.regions.join(", ") })
    : t("cloud.factAzure", { subscription: stored.subscription ?? "" });
}

/** Signs in once from the central server and says what it sees. Only for new credentials, as a test needs the secret. */
export function CloudTest({ siteId, value }: { siteId: string; value: CloudForm }) {
  const [tested, setTested] = useState<CloudTested | null>(null);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);

  async function test() {
    const problem = cloudProblem(value, false);
    if (problem) return setError(problem);
    setBusy(true);
    setError(null);
    try {
      setTested(await api.testCloud({ site_id: siteId, cloud: cloudRequest(value) }));
    } catch (e) {
      setTested(null);
      setError(t("cloud.testFailed", { error: describeError(e) }));
    } finally {
      setBusy(false);
    }
  }

  return (
    <>
      <div className={styles.formFoot}>
        <Button onClick={() => void test()} disabled={busy || !siteId}>
          {busy ? t("cloud.testing") : t("cloud.test")}
        </Button>
      </div>
      {error && (
        <p className={styles.formError} role="alert">
          {error}
        </p>
      )}
      {tested && !tested.login && (
        <p className={styles.formError} role="status">
          {t("cloud.loginFailed", { error: tested.error ?? "" })}
        </p>
      )}
      {tested?.login && (
        <div className={styles.sectionNote} role="status">
          <p>{t("cloud.loginOk", { vms: String(tested.vms), running: String(tested.running) })}</p>
          {tested.metrics === true && <p>{t("cloud.metricsOk")}</p>}
          {tested.metrics === false && <p className={styles.formError}>{t("cloud.metricsFailed", { error: tested.warning ?? "" })}</p>}
          {tested.metrics !== false && tested.warning && <p className={styles.formError}>{t("cloud.partial", { error: tested.warning })}</p>}
        </div>
      )}
    </>
  );
}

/** Change a device's cloud credentials. */
export function EditCloud({
  server,
  stored,
  onClose,
  onSaved,
}: {
  server: ServerDetail;
  stored: CloudCredentials | null;
  onClose: () => void;
  onSaved: () => void;
}) {
  const [value, setValue] = useState<CloudForm>(stored ? cloudForm(stored) : NEW_CLOUD);
  const [problem, setProblem] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  const keep = stored !== null;

  async function submit(event: FormEvent) {
    event.preventDefault();
    const found = cloudProblem(value, keep);
    if (found) return setProblem(found);
    setBusy(true);
    setProblem(null);
    try {
      await api.setDeviceCloud(server.id, cloudRequest(value));
      onSaved();
    } catch (e) {
      setProblem(t("cloud.failed", { error: describeError(e) }));
      setBusy(false);
    }
  }

  return (
    <form className={styles.panel} onSubmit={(e) => void submit(e)} aria-label={t("cloud.title")} noValidate>
      <CloudFields value={value} onChange={setValue} keep={keep} />
      {value.secret && <CloudTest siteId={server.site_id} value={value} />}
      {problem && (
        <p className={styles.formError} role="alert">
          {problem}
        </p>
      )}
      <div className={styles.formFoot}>
        <Button variant="k" type="submit" disabled={busy}>
          {busy ? t("cloud.saving") : t("cloud.save")}
        </Button>
        <Button onClick={onClose} disabled={busy}>
          {t("cloud.cancel")}
        </Button>
      </div>
    </form>
  );
}
