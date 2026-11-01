"use client";

import Link from "next/link";
import { type FormEvent, type ReactNode, useCallback, useState } from "react";

import { type RetentionSettings, type ServerSettings, api, describeError } from "@/lib/api";
import { formatDuration } from "@/lib/format";
import { administersEverything, useMe } from "@/lib/me";
import {
  AI_SETTINGS_PATH,
  AUTO_UPDATE_PATH,
  BACKUPS_PATH,
  MIRRORS_PATH,
  NOTIFICATIONS_PATH,
  SERVER_UPDATE_PATH,
  SITES_PATH,
  USERS_PATH,
} from "@/lib/routes";
import { usePolledData } from "@/lib/usePolledData";

import styles from "./AdminPages.module.css";
import { DefaultInstallSource, ServerAddress } from "./TokensAdmin";
import { Button, fieldClass } from "./ui/Button";
import { cx } from "./ui/cx";
import { InlineConfirm } from "./ui/InlineConfirm";
import { Banner, ErrorState, LoadingBlock, NoPermission, PageHeader } from "./ui/PageState";

/** While a retention change runs. */
const APPLYING_REFRESH_MS = 3_000;

const OTHER_SETTINGS: { label: string; href: string; note: string }[] = [
  { label: "Users and roles", href: USERS_PATH, note: "Who signs in, and what they can do." },
  { label: "Sites", href: SITES_PATH, note: "Customers and the sites their servers belong to." },
  { label: "Notifications", href: NOTIFICATIONS_PATH, note: "Email and webhooks for incidents, approvals and failed updates." },
  { label: "AI", href: AI_SETTINGS_PATH, note: "An API key and a model to diagnose incidents and propose fixes." },
  { label: "Automatic updates", href: AUTO_UPDATE_PATH, note: "Keep agents and relays on the newest release." },
  { label: "Server update", href: SERVER_UPDATE_PATH, note: "Update this server, or import a release without Internet." },
  { label: "Backups", href: BACKUPS_PATH, note: "Daily backups, Back up now, and downloads." },
  { label: "Mirrors", href: MIRRORS_PATH, note: "Package repositories served from this server." },
];

/** `/admin/settings/`: global administrators only, like the API (ADR-0069). */
export function SettingsPage() {
  const me = useMe();
  if (!administersEverything(me)) {
    return (
      <NoPermission title="Settings need a global administrator">
        Ask one to change how this server runs.
      </NoPermission>
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
      <PageHeader title="Settings" meta="How this server runs; changes apply without a restart" />
      <AddressSection />
      <DefaultInstallSource editable />
      {settings ? (
        <>
          <SessionsSection settings={settings} onSaved={refresh} />
          <MirrorSection settings={settings} onSaved={refresh} />
          <RetentionSection retention={settings.retention} onSaved={refresh} />
        </>
      ) : error ? (
        <ErrorState what="the settings" error={describeError(error)} onRetry={refresh} />
      ) : (
        <LoadingBlock />
      )}
      <section className={styles.section} aria-labelledby="other-settings">
        <div className={styles.sectionHead}>
          <h2 id="other-settings" className={styles.sectionTitle}>
            More settings
          </h2>
        </div>
        <ul className={styles.linkList}>
          {OTHER_SETTINGS.map((item) => (
            <li key={item.href}>
              <Link href={item.href}>{item.label}</Link> <span className={styles.dim}>{item.note}</span>
            </li>
          ))}
        </ul>
      </section>
    </div>
  );
}

/** Why a value cannot be changed here. */
function Locked({ name }: { name: string }) {
  return (
    <span className={styles.fieldNote}>
      Locked by <code>{name}</code> in the server&apos;s <code>.env</code>, which takes precedence. To change it here,
      remove that line and run the installer again.
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
    <Section title="Server address" note="The name or address servers and relays use to reach this server.">
      <div className={styles.sectionBody}>
        {address ? (
          <ServerAddress value={address} editable onChange={refresh} note="— used in every install command" />
        ) : error ? (
          <ErrorState what="the address" error={describeError(error)} onRetry={refresh} />
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
        Save
      </Button>
      {saved && <span className={styles.footnote}>Saved.</span>}
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
      setError(`Saving ${what} failed. ${describeError(e)}`);
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
  const { busy, error, saved, save } = useSave("the sign-in settings", onSaved);
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
      title="Sign-in"
      note="How long people stay signed in. A shorter limit applies to everyone at once, and may sign you out; a longer one applies from the next sign-in."
    >
      <form className={styles.sectionBody} onSubmit={submit}>
        <div className={styles.formGrid}>
          <label className={styles.field}>
            <span>
              Sign out after, in hours{" "}
              <span className={styles.fieldNote}>— however active; default {formatDuration(lifetime.default)}</span>
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
              Sign out when idle, in minutes{" "}
              <span className={styles.fieldNote}>— default {formatDuration(idle.default)}</span>
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
  const { busy, error, saved, save } = useSave("the mirror address", onSaved);
  const trimmed = url.trim();
  const valid = trimmed === "" || /^https?:\/\/[^\s/?#@]+(\/[^\s?#]*)?$/.test(trimmed);

  function submit(event: FormEvent) {
    event.preventDefault();
    void save({ ...unchanged(settings), mirror_public_url: trimmed || null });
  }

  return (
    <Section
      title="Package address"
      note="Where hosts download packages and agent updates from this server. Change it when they reach it through another name or port."
    >
      <form className={styles.sectionBody} onSubmit={submit}>
        <label className={styles.field}>
          <span>
            Address{" "}
            <span className={styles.fieldNote}>
              {mirror.default ? `— default ${mirror.default}; leave empty to use it` : "— http or https URL"}
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
        {label} <span className={styles.fieldNote}>— default {retentionDays(fallback)} days</span>
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
  const note = "How long metrics and logs stay on disk. Longer needs more disk.";
  if (!retention) {
    return (
      <Section title="Data retention" note={note}>
        <p className={styles.sectionNote}>
          This server has no updater to apply it, so the metrics and log stores keep their own settings.
        </p>
      </Section>
    );
  }
  return (
    <Section title="Data retention" note={note}>
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
      setError(`Changing retention failed. ${describeError(e)}`);
    }
  }

  return (
    <div className={styles.sectionBody}>
      <div className={styles.formGrid}>
        <DaysField
          label="Keep metrics, in days"
          value={metricsDays}
          fallback={retention.default_metrics}
          locked={metrics.locked ? metrics : null}
          envName="METRICS_RETENTION"
          max={max}
          disabled={applying}
          onChange={setMetricsDays}
        />
        <DaysField
          label="Keep logs, in days"
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
          Applying: the metrics and log stores restart for a few seconds.
        </Banner>
      )}
      {!alive && (
        <Banner tone="warn">
          <strong>The updater is not running,</strong> so retention cannot change now. Run the installer again to bring
          it back.
        </Banner>
      )}
      {retention.error && (
        <Banner tone="bad">
          <strong>The last change failed</strong> and the previous values still apply: {retention.error}
        </Banner>
      )}
      {!(metrics.locked && logs.locked) &&
        (shorter && canSave ? (
          <InlineConfirm
            trigger="Save…"
            triggerVariant="k"
            title="Delete older data?"
            consequence="Metrics and logs older than the new limits are deleted and cannot be recovered. The stores restart for a few seconds."
            confirmLabel="Save and delete older data"
            onConfirm={apply}
            describeError={describeError}
          />
        ) : (
          <div className={styles.formFoot}>
            <Button variant="k" disabled={!canSave} onClick={() => void save()}>
              Save
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
