"use client";

import { type ChangeEvent, type FormEvent, useCallback, useState } from "react";

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
import { Label } from "./ui/StatusLabel";

/** `/admin/license/`: global administrators only, like the API (ADR-0100). */
export function LicensePage() {
  const me = useMe();
  if (!administersEverything(me)) {
    return <NoPermission title="The license needs a global administrator" />;
  }
  return <LicenseAdmin />;
}

function LicenseAdmin() {
  const fetcher = useCallback((signal: AbortSignal) => api.license(signal), []);
  const { data, error, refresh } = usePolledData(fetcher);
  const header = <PageHeader title="License" meta="Global administrators only" />;
  if (!data) {
    return (
      <div className={styles.page}>
        {header}
        {error ? <ErrorState what="the license" error={describeError(error)} onRetry={refresh} /> : <LoadingBlock />}
      </div>
    );
  }
  if (!data.available) {
    return (
      <div className={styles.page}>
        {header}
        <Banner tone="neutral">
          This build of MonitorFlow does not include the Enterprise edition. Everything else works as usual.
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
          A single central server needs none. An Enterprise subscription lets you add central servers on the same
          database, so MonitorFlow keeps working when one machine stops. Its license key goes here.
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
          title="Enterprise license"
          facts={[
            { label: "For", value: license.customer },
            { label: "Allows", value: `${centralServers(license.server_instances)}, the first one included` },
            { label: "Valid until", value: formatDay(license.expires) },
            {
              label: "Installed",
              value: `${view.installed_at ? formatDateTime(view.installed_at) : "—"}${view.installed_by ? ` by ${view.installed_by}` : ""}`,
            },
          ]}
          details={[
            { label: "License ID", value: license.id, mono: true },
            { label: "Issued", value: formatDay(license.issued) },
          ]}
        />
      )}
      <div>
        <InlineConfirm
          trigger="Remove the license key…"
          title="Remove the license key?"
          consequence="Central servers already added keep running and monitoring goes on, but no other central server can be added until a license key is installed."
          confirmLabel="Remove the license key"
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
          Central servers added
        </h2>
        <span className={styles.dim}>
          {added.length + 1}
          {allowed ? ` of ${allowed}` : ""} with the first one
        </span>
      </div>
      <ul className={own.added}>
        {added.map((server) => (
          <li key={server.name} className={own.addedRow}>
            <strong>{server.name}</strong>
            <Label tone={server.running ? "ok" : "neutral"} small>
              {server.running ? "Running" : "Not running"}
            </Label>
            <span className={styles.dim}>Added {formatDateTime(server.admitted_at)}</span>
            <InlineConfirm
              layout="row"
              trigger="Remove…"
              triggerName={`Remove ${server.name}`}
              title={`Remove ${server.name}?`}
              consequence="Its place is freed for another central server. If it still runs, it keeps running until it restarts, then needs a place again."
              confirmLabel={`Remove ${server.name}`}
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
          {replacing ? "Replace the license key" : "Install a license key"}
        </h2>
      </div>
      <form className={own.form} onSubmit={(e) => void install(e)}>
        <label className={styles.field}>
          License key
          <span className={styles.fieldNote}>Paste the text you received, or choose the file.</span>
          <textarea
            className={own.textarea}
            value={text}
            onChange={(e) => setText(e.target.value)}
            placeholder="-----BEGIN MONITORFLOW LICENSE-----"
            spellCheck={false}
          />
        </label>
        <label className={styles.field}>
          File
          <input type="file" accept=".txt,.lic,.license,text/plain" onChange={(e) => void readFile(e)} />
        </label>
        {failure && (
          <p className={own.error} role="alert">
            {failure}
          </p>
        )}
        <div className={own.actions}>
          <Button type="submit" variant="k" disabled={saving || !text.trim()}>
            {saving ? "Checking…" : replacing ? "Replace" : "Install"}
          </Button>
        </div>
      </form>
    </section>
  );
}
