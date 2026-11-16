"use client";

import { type FormEvent, type ReactNode, useCallback, useState } from "react";

import { rich, t } from "@/i18n";
import { type RetentionSettings, type ServerSettings, api, describeError } from "@/lib/api";
import { formatDuration } from "@/lib/format";
import { administersEverything, useMe } from "@/lib/me";
import { usePolledData } from "@/lib/usePolledData";

import styles from "./AdminPages.module.css";
import { DefaultInstallSource, ServerAddress } from "./TokensAdmin";
import { Button, fieldClass } from "./ui/Button";
import { cx } from "./ui/cx";
import { InlineConfirm } from "./ui/InlineConfirm";
import { Banner, ErrorState, LoadingBlock, NoPermission, PageHeader } from "./ui/PageState";

/** While a retention change runs. */
const APPLYING_REFRESH_MS = 3_000;

/** `/settings/general/`: global administrators only, like the API (ADR-0069). */
export function SettingsPage() {
  const me = useMe();
  if (!administersEverything(me)) {
    return (
      <NoPermission title={t("settings.page.noPermission")}>{t("settings.page.askOne")}</NoPermission>
    );
  }
  return <Settings />;
}

function Settings() {
  const fetcher = useCallback((signal: AbortSignal) => api.serverSettings(signal), []);
  const [applying, setApplying] = useState(false);
  const { data: settings, error, refresh } = usePolledData(
    fetcher,
    applying ? { intervalMs: APPLYING_REFRESH_MS } : undefined,
  );
  const nowApplying = settings?.retention?.applying ?? false;
  if (nowApplying !== applying) setApplying(nowApplying);

  return (
    <div className={styles.page}>
      <PageHeader title={t("settings.general.label")} meta={t("settings.page.meta")} />
      <AddressSection />
      <DefaultInstallSource editable />
      {settings ? (
        <>
          <SessionsSection settings={settings} onSaved={refresh} />
          <MirrorSection settings={settings} onSaved={refresh} />
          <RetentionSection retention={settings.retention} onSaved={refresh} />
        </>
      ) : error ? (
        <ErrorState what={t("settings.page.theSettings")} error={describeError(error)} onRetry={refresh} />
      ) : (
        <LoadingBlock />
      )}
    </div>
  );
}

/** Why a value cannot be changed here. */
function Locked({ name }: { name: string }) {
  return (
    <span className={styles.fieldNote}>
      {rich("settings.page.locked", { name: <code>{name}</code>, file: <code>.env</code> })}
    </span>
  );
}

function Section({ title, note, children }: { title: string; note: ReactNode; children: ReactNode }) {
  return (
    <section className={styles.section} aria-label={title}>
      <div className={styles.sectionHead}>
        <h2 className={styles.sectionTitle}>{title}</h2>
      </div>
      <p className={styles.sectionNote}>{note}</p>
      {children}
    </section>
  );
}

function AddressSection() {
  const fetcher = useCallback((signal: AbortSignal) => api.publicAddress(signal), []);
  const { data: address, error, refresh } = usePolledData(fetcher);
  return (
    <Section title={t("settings.page.address")} note={t("settings.page.addressFieldNote")}>
      <div className={styles.sectionBody}>
        {address ? (
          <ServerAddress value={address} editable onChange={refresh} note={t("settings.page.addressFieldNote")} />
        ) : error ? (
          <ErrorState what={t("settings.page.theAddress")} error={describeError(error)} onRetry={refresh} />
        ) : (
          <LoadingBlock />
        )}
      </div>
    </Section>
  );
}

/** The body of `PUT /api/settings/server` keeping what a form does not edit. */
function unchanged(settings: ServerSettings) {
  const value = <T,>(setting: { value: T; locked: boolean }) => (setting.locked ? null : setting.value);
  return {
    session_lifetime: value(settings.session_lifetime),
    session_idle_timeout: value(settings.session_idle_timeout),
    mirror_public_url: value(settings.mirror_public_url),
  };
}

function SaveLine({ busy, disabled, saved }: { busy: boolean; disabled: boolean; saved: boolean }) {
  return (
    <div className={styles.formFoot}>
      <Button variant="k" type="submit" disabled={busy || disabled}>
        {t("settings.page.save")}
      </Button>
      {saved && <span className={styles.footnote}>{t("settings.page.saved")}</span>}
    </div>
  );
}

function useSave(what: string, onSaved: () => void) {
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [saved, setSaved] = useState(false);
  async function save(body: Parameters<typeof api.setServerSettings>[0]) {
    setBusy(true);
    setError(null);
    setSaved(false);
    try {
      await api.setServerSettings(body);
      onSaved();
      setSaved(true);
    } catch (e) {
      setError(t("settings.page.saveFailed", { what, error: describeError(e) }));
    } finally {
      setBusy(false);
    }
  }
  return { busy, error, saved, save };
}

function SessionsSection({ settings, onSaved }: { settings: ServerSettings; onSaved: () => void }) {
  const { session_lifetime: lifetime, session_idle_timeout: idle } = settings;
  const [hours, setHours] = useState(String(lifetime.value / 3600));
  const [minutes, setMinutes] = useState(String(idle.value / 60));
  const { busy, error, saved, save } = useSave(t("settings.page.theSignIn"), onSaved);
  const hoursValue = Number(hours);
  const minutesValue = Number(minutes);
  const valid =
    (lifetime.locked || (Number.isInteger(hoursValue) && hoursValue >= 1 && hoursValue <= 720)) &&
    (idle.locked || (Number.isInteger(minutesValue) && minutesValue >= 5 && minutesValue <= 1440));

  function submit(event: FormEvent) {
    event.preventDefault();
    void save({
      ...unchanged(settings),
      session_lifetime: lifetime.locked ? null : hoursValue * 3600,
      session_idle_timeout: idle.locked ? null : minutesValue * 60,
    });
  }

  return (
    <Section
      title={t("settings.page.signIn")}
      note={t("settings.page.signInNote")}
    >
      <form className={styles.sectionBody} onSubmit={submit}>
        <div className={styles.formGrid}>
          <label className={styles.field}>
            <span>
              {t("settings.page.lifetime")}{" "}
              <span className={styles.fieldNote}>
                {t("settings.page.lifetimeNote", { duration: formatDuration(lifetime.default) })}
              </span>
            </span>
            {lifetime.locked ? (
              <>
                <strong>{formatDuration(lifetime.value)}</strong>
                <Locked name="SESSION_LIFETIME" />
              </>
            ) : (
              <input
                className={fieldClass}
                type="number"
                min={1}
                max={720}
                value={hours}
                onChange={(e) => setHours(e.target.value)}
              />
            )}
          </label>
          <label className={styles.field}>
            <span>
              {t("settings.page.idle")}{" "}
              <span className={styles.fieldNote}>{t("settings.page.default", { value: formatDuration(idle.default) })}</span>
            </span>
            {idle.locked ? (
              <>
                <strong>{formatDuration(idle.value)}</strong>
                <Locked name="SESSION_IDLE_TIMEOUT" />
              </>
            ) : (
              <input
                className={fieldClass}
                type="number"
                min={5}
                max={1440}
                value={minutes}
                onChange={(e) => setMinutes(e.target.value)}
              />
            )}
          </label>
        </div>
        {!(lifetime.locked && idle.locked) && <SaveLine busy={busy} disabled={!valid} saved={saved} />}
        {error && (
          <p className={styles.formError} role="alert">
            {error}
          </p>
        )}
      </form>
    </Section>
  );
}

function MirrorSection({ settings, onSaved }: { settings: ServerSettings; onSaved: () => void }) {
  const mirror = settings.mirror_public_url;
  const [url, setUrl] = useState(mirror.value ?? "");
  const { busy, error, saved, save } = useSave(t("settings.page.theMirror"), onSaved);
  const trimmed = url.trim();
  const valid = trimmed === "" || /^https?:\/\/[^\s/?#@]+(\/[^\s?#]*)?$/.test(trimmed);

  function submit(event: FormEvent) {
    event.preventDefault();
    void save({ ...unchanged(settings), mirror_public_url: trimmed || null });
  }

  return (
    <Section
      title={t("settings.page.packages")}
      note={t("settings.page.packagesNote")}
    >
      <form className={styles.sectionBody} onSubmit={submit}>
        <label className={styles.field}>
          <span>
            {t("settings.page.packagesField")}{" "}
            <span className={styles.fieldNote}>
              {mirror.default ? t("settings.page.packagesDefault", { url: mirror.default }) : t("settings.page.packagesUrl")}
            </span>
          </span>
          {mirror.locked ? (
            <>
              <code>{mirror.value}</code>
              <Locked name="MIRROR_PUBLIC_URL" />
            </>
          ) : (
            <input
              className={cx(fieldClass, styles.mono)}
              value={url}
              onChange={(e) => setUrl(e.target.value)}
              placeholder={mirror.default ?? "http://monitor.example.com:7480"}
              aria-invalid={!valid}
              spellCheck={false}
              autoCapitalize="none"
            />
          )}
        </label>
        {!mirror.locked && <SaveLine busy={busy} disabled={!valid} saved={saved} />}
        {error && (
          <p className={styles.formError} role="alert">
            {error}
          </p>
        )}
      </form>
    </Section>
  );
}

/** Days of a `-retentionPeriod` value in days, like `90d`. */
export function retentionDays(value: string): number | null {
  const match = /^(\d+)d$/.exec(value);
  return match ? Number(match[1]) : null;
}

function DaysField({
  label,
  value,
  fallback,
  locked,
  envName,
  max,
  disabled,
  onChange,
}: {
  label: string;
  value: string;
  fallback: string;
  locked: { value: string } | null;
  envName: string;
  max: number;
  disabled: boolean;
  onChange: (value: string) => void;
}) {
  return (
    <label className={styles.field}>
      <span>
        {label}{" "}
        <span className={styles.fieldNote}>
          {t("settings.page.default", { value: t("format.days", { count: retentionDays(fallback) ?? 0 }) })}
        </span>
      </span>
      {locked ? (
        <>
          <strong>{locked.value}</strong>
          <Locked name={envName} />
        </>
      ) : (
        <input
          className={fieldClass}
          type="number"
          min={1}
          max={max}
          value={value}
          disabled={disabled}
          onChange={(e) => onChange(e.target.value)}
        />
      )}
    </label>
  );
}

function RetentionSection({
  retention,
  onSaved,
}: {
  retention: RetentionSettings | null;
  onSaved: () => void;
}) {
  const note = t("settings.page.retentionNote");
  if (!retention) {
    return (
      <Section title={t("settings.page.retention")} note={note}>
        <p className={styles.sectionNote}>{t("settings.page.noUpdater")}</p>
      </Section>
    );
  }
  return (
    <Section title={t("settings.page.retention")} note={note}>
      <RetentionForm retention={retention} onSaved={onSaved} />
    </Section>
  );
}

function RetentionForm({
  retention,
  onSaved,
}: {
  retention: RetentionSettings;
  onSaved: () => void;
}) {
  const { metrics, logs, applying, updater_alive: alive, max_days: max } = retention;
  const shown = (value: string) => String(retentionDays(value) ?? "");
  const [metricsDays, setMetricsDays] = useState(shown(metrics.value));
  const [logsDays, setLogsDays] = useState(shown(logs.value));
  const [error, setError] = useState<string | null>(null);
  const days = (text: string, locked: boolean) => {
    const value = Number(text);
    return !locked && Number.isInteger(value) && value >= 1 && value <= max ? value : null;
  };
  const newMetrics = days(metricsDays, metrics.locked);
  const newLogs = days(logsDays, logs.locked);
  const changed = (value: number | null, kept: { value: string }) =>
    value !== null && value !== retentionDays(kept.value) ? value : null;
  const metricsChange = changed(newMetrics, metrics);
  const logsChange = changed(newLogs, logs);
  const shorter =
    (metricsChange !== null && metricsChange < (retentionDays(metrics.value) ?? 0)) ||
    (logsChange !== null && logsChange < (retentionDays(logs.value) ?? 0));
  const canSave =
    alive &&
    !applying &&
    (metricsChange !== null || logsChange !== null) &&
    (metrics.locked || newMetrics !== null) &&
    (logs.locked || newLogs !== null);

  async function apply() {
    await api.setRetention({ metrics_days: metricsChange, logs_days: logsChange });
    onSaved();
  }

  async function save() {
    setError(null);
    try {
      await apply();
    } catch (e) {
      setError(t("settings.page.retentionFailed", { error: describeError(e) }));
    }
  }

  return (
    <div className={styles.sectionBody}>
      <div className={styles.formGrid}>
        <DaysField
          label={t("settings.page.keepMetrics")}
          value={metricsDays}
          fallback={retention.default_metrics}
          locked={metrics.locked ? metrics : null}
          envName="METRICS_RETENTION"
          max={max}
          disabled={applying}
          onChange={setMetricsDays}
        />
        <DaysField
          label={t("settings.page.keepLogs")}
          value={logsDays}
          fallback={retention.default_logs}
          locked={logs.locked ? logs : null}
          envName="LOGS_RETENTION"
          max={max}
          disabled={applying}
          onChange={setLogsDays}
        />
      </div>
      {applying && (
        <Banner tone="plain" role="status">
          {t("settings.page.applying")}
        </Banner>
      )}
      {!alive && (
        <Banner tone="warn">
          {rich("settings.page.updaterDown", { strong: <strong>{t("settings.page.updaterDownStrong")}</strong> })}
        </Banner>
      )}
      {retention.error && (
        <Banner tone="bad">
          {rich("settings.page.lastFailed", {
            strong: <strong>{t("settings.page.lastFailedStrong")}</strong>,
            error: retention.error,
          })}
        </Banner>
      )}
      {!(metrics.locked && logs.locked) &&
        (shorter && canSave ? (
          <InlineConfirm
            trigger={t("settings.page.saveConfirm")}
            triggerVariant="k"
            title={t("settings.page.deleteTitle")}
            consequence={t("settings.page.deleteConsequence")}
            confirmLabel={t("settings.page.deleteConfirm")}
            onConfirm={apply}
            describeError={describeError}
          />
        ) : (
          <div className={styles.formFoot}>
            <Button variant="k" disabled={!canSave} onClick={() => void save()}>
              {t("settings.page.save")}
            </Button>
          </div>
        ))}
      {error && (
        <p className={styles.formError} role="alert">
          {error}
        </p>
      )}
    </div>
  );
}
