"use client";

import { type ChangeEvent, type FormEvent, useCallback, useState } from "react";

import { t } from "@/i18n";
import { type AddedServer, ApiError, type LicenseView, api, describeError } from "@/lib/api";
import { formatDateTime } from "@/lib/format";
import { centralServers, formatDay, licenseSummary } from "@/lib/license";
import { administersEverything, useMe } from "@/lib/me";
import { usePolledData } from "@/lib/usePolledData";

import styles from "./AdminPages.module.css";
import own from "./License.module.css";
import { Button } from "./ui/Button";
import { FactsBox } from "./ui/FactsBox";
import { InlineConfirm } from "./ui/InlineConfirm";
import { Banner, ErrorState, LoadingBlock, NoPermission, PageHeader } from "./ui/PageState";
import { missingText } from "./ui/Missing";
import { Label } from "./ui/StatusLabel";

/** `/settings/license/`: global administrators only, like the API (ADR-0100). */
export function LicensePage() {
  const me = useMe();
  if (!administersEverything(me)) {
    return <NoPermission title={t("license.noPermission")} />;
  }
  return <LicenseAdmin />;
}

function LicenseAdmin() {
  const fetcher = useCallback((signal: AbortSignal) => api.license(signal), []);
  const { data, error, refresh } = usePolledData(fetcher);
  const header = <PageHeader title={t("license.title")} meta={t("license.meta")} />;
  if (!data) {
    return (
      <div className={styles.page}>
        {header}
        {error ? <ErrorState what={t("license.what")} error={describeError(error)} onRetry={refresh} /> : <LoadingBlock />}
      </div>
    );
  }
  if (!data.available) {
    return (
      <div className={styles.page}>
        {header}
        <Banner tone="neutral">
          {t("license.notIncluded")}
        </Banner>
      </div>
    );
  }
  return (
    <div className={styles.page}>
      {header}
      <Status view={data} onRemoved={refresh} />
      {(data.added?.length ?? 0) > 0 && <AddedServers view={data} onRemoved={refresh} />}
      <InstallForm replacing={data.state !== "none"} onInstalled={refresh} />
    </div>
  );
}

function Status({ view, onRemoved }: { view: LicenseView; onRemoved: () => void }) {
  const [summary, tone] = licenseSummary(view);
  const license = view.license;
  if (view.state === "none") {
    return (
      <section className={styles.section} aria-labelledby="license-status">
        <div className={styles.sectionHead}>
          <h2 id="license-status" className={styles.sectionTitle}>
            {summary}
          </h2>
        </div>
        <p className={styles.sectionNote}>
          {t("license.noneNote")}
        </p>
      </section>
    );
  }
  return (
    <>
      <section className={styles.section} aria-labelledby="license-status">
        <div className={styles.sectionHead}>
          <h2 id="license-status" className={styles.sectionTitle}>
            <Label tone={tone}>{summary}</Label>
          </h2>
        </div>
      </section>
      {license && (
        <FactsBox
          title={t("license.facts.title")}
          facts={[
            { label: t("license.facts.for"), value: license.customer },
            {
              label: t("license.facts.allows"),
              value: t("license.facts.allowsValue", { servers: centralServers(license.server_instances) }),
            },
            { label: t("license.facts.validUntil"), value: formatDay(license.expires) },
            {
              label: t("license.facts.installed"),
              value: view.installed_by
                ? t("license.facts.installedBy", {
                    when: view.installed_at ? formatDateTime(view.installed_at) : missingText("unknown"),
                    name: view.installed_by,
                  })
                : view.installed_at
                  ? formatDateTime(view.installed_at)
                  : missingText("unknown"),
            },
          ]}
          details={[
            { label: t("license.facts.id"), value: license.id, mono: true },
            { label: t("license.facts.issued"), value: formatDay(license.issued) },
          ]}
        />
      )}
      <div>
        <InlineConfirm
          trigger={t("license.remove.trigger")}
          title={t("license.remove.title")}
          consequence={t("license.remove.consequence")}
          confirmLabel={t("license.remove.confirm")}
          onConfirm={async () => {
            await api.removeLicense();
            onRemoved();
          }}
          describeError={describeError}
        />
      </div>
    </>
  );
}

/** The central servers added with `install-server.sh --join`, and freeing a place. */
function AddedServers({ view, onRemoved }: { view: LicenseView; onRemoved: () => void }) {
  const added: AddedServer[] = view.added ?? [];
  const allowed = view.license?.server_instances;
  return (
    <section className={styles.section} aria-labelledby="license-added">
      <div className={styles.sectionHead}>
        <h2 id="license-added" className={styles.sectionTitle}>
          {t("license.added.title")}
        </h2>
        <span className={styles.dim}>
          {allowed
            ? t("license.added.countOf", { count: added.length + 1, allowed })
            : t("license.added.count", { count: added.length + 1 })}
        </span>
      </div>
      <ul className={own.added}>
        {added.map((server) => (
          <li key={server.name} className={own.addedRow}>
            <strong>{server.name}</strong>
            <Label tone={server.running ? "ok" : "neutral"} small>
              {server.running ? t("license.added.running") : t("license.added.notRunning")}
            </Label>
            <span className={styles.dim}>{t("license.added.when", { when: formatDateTime(server.admitted_at) })}</span>
            <InlineConfirm
              layout="row"
              trigger={t("license.added.removeTrigger")}
              triggerName={t("license.added.removeName", { name: server.name })}
              title={t("license.added.removeTitle", { name: server.name })}
              consequence={t("license.added.removeConsequence")}
              confirmLabel={t("license.added.removeName", { name: server.name })}
              onConfirm={async () => {
                await api.removeAddedServer(server.name);
                onRemoved();
              }}
              describeError={describeError}
            />
          </li>
        ))}
      </ul>
    </section>
  );
}

function InstallForm({ replacing, onInstalled }: { replacing: boolean; onInstalled: () => void }) {
  const [text, setText] = useState("");
  const [saving, setSaving] = useState(false);
  const [failure, setFailure] = useState<string | null>(null);

  async function readFile(event: ChangeEvent<HTMLInputElement>) {
    const file = event.target.files?.[0];
    if (file) setText(await file.text());
  }

  async function install(event: FormEvent) {
    event.preventDefault();
    setSaving(true);
    setFailure(null);
    try {
      await api.installLicense(text);
      setText("");
      onInstalled();
    } catch (e) {
      setFailure(e instanceof ApiError && e.status === 400 ? `${e.message}.` : describeError(e));
    } finally {
      setSaving(false);
    }
  }

  return (
    <section className={styles.section} aria-labelledby="license-install">
      <div className={styles.sectionHead}>
        <h2 id="license-install" className={styles.sectionTitle}>
          {replacing ? t("license.install.replaceTitle") : t("license.install.title")}
        </h2>
      </div>
      <form className={own.form} onSubmit={(e) => void install(e)}>
        <label className={styles.field}>
          {t("license.install.key")}
          <textarea
            className={own.textarea}
            value={text}
            onChange={(e) => setText(e.target.value)}
            placeholder="-----BEGIN MONITORFLOW LICENSE-----"
            spellCheck={false}
          />
        </label>
        <label className={styles.field}>
          {t("license.install.file")}
          <input type="file" accept=".txt,.lic,.license,text/plain" onChange={(e) => void readFile(e)} />
        </label>
        {failure && (
          <p className={own.error} role="alert">
            {failure}
          </p>
        )}
        <div className={own.actions}>
          <Button type="submit" variant="k" disabled={saving || !text.trim()}>
            {saving ? t("license.install.checking") : replacing ? t("license.install.replace") : t("license.install.install")}
          </Button>
        </div>
      </form>
    </section>
  );
}
