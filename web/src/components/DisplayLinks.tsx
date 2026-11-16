"use client";

import { type FormEvent, useCallback, useEffect, useState } from "react";

import { type DashboardSummary, type DisplayLink, type DisplayLinkExpiry, api, describeError } from "@/lib/api";
import { t } from "@/i18n";
import { MAX_NAME } from "@/lib/dashboards";
import { usePolledData } from "@/lib/usePolledData";
import { Button, fieldClass } from "@/components/ui/Button";
import { InlineConfirm } from "@/components/ui/InlineConfirm";
import { Banner } from "@/components/ui/PageState";
import { SecretField } from "@/components/ui/SecretField";
import { formatDay, formatDuration } from "@/components/ui/time";

import admin from "./AdminPages.module.css";

const EXPIRIES = [
  { value: "1y", label: "display.links.inAYear" },
  { value: "30d", label: "display.links.in30Days" },
  { value: "never", label: "display.links.never" },
] as const satisfies { value: DisplayLinkExpiry; label: string }[];

const PAGE_TIMES = [15, 30, 60, 120, 300];

/** Most dashboards one link shows in turn. */
const MAX_DASHBOARDS = 12;

/** "Shows NOC, then Disks, 30 s each" in plain words. */
export function linkSummary(link: DisplayLink): string {
  const names = link.dashboards.map((d) => d.name);
  if (link.other_dashboards > 0) names.push(t("display.links.more", { count: link.other_dashboards }));
  return names.length > 1
    ? t("display.links.showsInTurn", {
        names: names.join(t("display.links.then")),
        duration: formatDuration(link.seconds_per_page),
      })
    : t("display.links.shows", { name: names[0] ?? t("display.links.nothing") });
}

/**
 * Display links of a dashboard (ADR-0099), on its page, for those who may
 * make them: the links that show it, with when a screen last used each,
 * "Turn off", and a form for a new one whose address is shown once.
 */
export function DisplayLinks({ dashboard }: { dashboard: { id: string; name: string } }) {
  const fetcher = useCallback((signal: AbortSignal) => api.displayLinks(dashboard.id, signal), [dashboard.id]);
  const { data, error, refresh } = usePolledData(fetcher, { intervalMs: 60_000 });
  const [creating, setCreating] = useState(false);
  const [created, setCreated] = useState<{ name: string; url: string } | null>(null);
  const now = new Date();

  return (
    <section className={admin.section} aria-labelledby="display-links">
      <div className={admin.sectionHead}>
        <h2 id="display-links" className={admin.sectionTitle}>
          {t("display.links.title")}
        </h2>
        <span className={admin.dim}>{t("display.links.note")}</span>
      </div>
      {created && (
        <>
          <Banner tone="warn">{t("display.links.created", { name: created.name })}</Banner>
          <SecretField label={t("display.links.address")} value={created.url} copyName={t("display.links.copy")} />
        </>
      )}
      {error !== null && !data && <Banner tone="bad">{t("display.links.failed", { error: describeError(error) })}</Banner>}
      {data && data.links.length > 0 && (
        <ul className={admin.linkList}>
          {data.links.map((link) => (
            <li key={link.id}>
              <b>{link.name}</b> <span className={admin.dim}>{linkSummary(link)}.</span>{" "}
              <span className={admin.dim}>
                {t("display.links.madeBy", { name: link.created_by_username })}{" "}
                {link.expires_at
                  ? t("display.links.expires", { day: formatDay(new Date(link.expires_at), now) })
                  : t("display.links.neverExpires")}{" "}
                {link.last_used_at
                  ? t("display.links.lastUsed", { day: formatDay(new Date(link.last_used_at), now) })
                  : t("display.links.notUsed")}
              </span>{" "}
              <InlineConfirm
                layout="row"
                trigger={t("display.links.turnOff")}
                triggerVariant="default"
                triggerName={t("display.links.turnOffName", { name: link.name })}
                title={t("display.links.turnOffTitle", { name: link.name })}
                consequence={t("display.links.turnOffText")}
                confirmLabel={t("display.links.turnOff")}
                pendingLabel={t("display.links.turningOff")}
                describeError={describeError}
                onConfirm={async () => {
                  await api.revokeDisplayLink(link.id);
                  refresh();
                }}
              />
            </li>
          ))}
        </ul>
      )}
      {creating ? (
        <NewLink
          dashboard={dashboard}
          onCancel={() => setCreating(false)}
          onCreated={(link) => {
            setCreated(link);
            setCreating(false);
            refresh();
          }}
        />
      ) : (
        <div>
          <Button onClick={() => setCreating(true)}>{t("display.links.new")}</Button>
        </div>
      )}
    </section>
  );
}

function NewLink({
  dashboard,
  onCancel,
  onCreated,
}: {
  dashboard: { id: string; name: string };
  onCancel: () => void;
  onCreated: (link: { name: string; url: string }) => void;
}) {
  const [name, setName] = useState("");
  const [others, setOthers] = useState<DashboardSummary[]>([]);
  const [chosen, setChosen] = useState<string[]>([]);
  const [seconds, setSeconds] = useState(30);
  const [expires, setExpires] = useState<DisplayLinkExpiry>("1y");
  const [busy, setBusy] = useState(false);
  const [failure, setFailure] = useState<string | null>(null);

  // Other dashboards the user may show in turn with this one.
  useEffect(() => {
    const controller = new AbortController();
    api.dashboards(controller.signal).then(
      (list) => setOthers(list.dashboards.filter((d) => d.can_link && d.id !== dashboard.id)),
      () => undefined,
    );
    return () => controller.abort();
  }, [dashboard.id]);

  async function submit(event: FormEvent) {
    event.preventDefault();
    if (!name.trim()) return setFailure(t("display.links.nameRequired"));
    setBusy(true);
    setFailure(null);
    try {
      const link = await api.createDisplayLink({
        name: name.trim(),
        dashboard_ids: [dashboard.id, ...chosen],
        seconds_per_page: seconds,
        expires,
      });
      onCreated({ name: link.name, url: link.url });
    } catch (e) {
      setFailure(describeError(e));
      setBusy(false);
    }
  }

  return (
    <form className={`${admin.panel} ${admin.stack}`} onSubmit={(e) => void submit(e)} aria-label={t("display.links.new")}
    >
      <label className={admin.field}>
        {t("display.links.where")}
        <input
          className={fieldClass}
          required
          maxLength={MAX_NAME}
          value={name}
          placeholder={t("display.links.placeholder")}
          onChange={(e) => setName(e.target.value)}
        />
      </label>
      {others.length > 0 && (
        <fieldset className={admin.fieldset}>
          <legend className={admin.legend}>{t("display.links.alsoShow")}</legend>
          {others.map((d) => (
            <label key={d.id}>
              <input
                type="checkbox"
                checked={chosen.includes(d.id)}
                disabled={!chosen.includes(d.id) && chosen.length >= MAX_DASHBOARDS - 1}
                onChange={(e) => setChosen(e.target.checked ? [...chosen, d.id] : chosen.filter((id) => id !== d.id))}
              />{" "}
              {d.name}
            </label>
          ))}
        </fieldset>
      )}
      {chosen.length > 0 && (
        <label className={admin.field}>
          {t("display.links.stays")}
          <select className={fieldClass} value={seconds} onChange={(e) => setSeconds(Number(e.target.value))}>
            {PAGE_TIMES.map((s) => (
              <option key={s} value={s}>
                {formatDuration(s)}
              </option>
            ))}
          </select>
        </label>
      )}
      <label className={admin.field}>
        {t("display.links.stops")}
        <select className={fieldClass} value={expires} onChange={(e) => setExpires(e.target.value as DisplayLinkExpiry)}>
          {EXPIRIES.map((x) => (
            <option key={x.value} value={x.value}>
              {t(x.label)}
            </option>
          ))}
        </select>
        <span className={admin.fieldNote}>{t("display.links.stopsNote")}</span>
      </label>
      {failure && <Banner tone="bad">{failure}</Banner>}
      <div className={admin.formFoot}>
        <Button type="submit" variant="k" disabled={busy}>
          {busy ? t("display.links.making") : t("display.links.make")}
        </Button>
        <Button onClick={onCancel} disabled={busy}>
          {t("display.links.cancel")}
        </Button>
      </div>
    </form>
  );
}
