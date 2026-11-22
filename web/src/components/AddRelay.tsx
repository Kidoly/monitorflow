"use client";

import Link from "next/link";
import { type FormEvent, useEffect, useId, useState } from "react";

import { Button, fieldClass } from "@/components/ui/Button";
import { cx } from "@/components/ui/cx";
import { rich, t } from "@/i18n";
import { type CreatedInstallKey, type Me, type Relay, type Site, api, describeError } from "@/lib/api";
import { placeLabel } from "@/lib/customers";
import { administersCustomer, administersEverything, multiCustomer } from "@/lib/me";
import { SITES_PATH, relayHref } from "@/lib/routes";

import styles from "./AdminPages.module.css";
import { NewRelayKey } from "./TokensAdmin";

/** Relays › Add relay: pick the site, get the one-line command (ADR-0066). */
export function AddRelay({ me, onDone }: { me: Me; onDone: () => void }) {
  const titleId = useId();
  const [sites, setSites] = useState<Site[] | null>(null);
  const [siteId, setSiteId] = useState("");
  const [created, setCreated] = useState<CreatedInstallKey | null>(null);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    const controller = new AbortController();
    api
      .sites(controller.signal)
      .then(({ sites }) => setSites(sites.filter((site) => administersCustomer(me, site.customer_id))))
      .catch((e: unknown) => {
        if (!controller.signal.aborted) setError(t("relays.add.loadFailed", { error: describeError(e) }));
      });
    return () => controller.abort();
  }, [me]);

  async function submit(event: FormEvent) {
    event.preventDefault();
    setBusy(true);
    setError(null);
    try {
      const site = siteId || (sites?.[0]?.id ?? "");
      setCreated(await api.createInstallKey({ role: "relay", site_id: site, require_approval: false }));
    } catch (e) {
      setError(t("relays.add.createFailed", { error: describeError(e) }));
    } finally {
      setBusy(false);
    }
  }

  if (created) {
    return <NewRelayKey installKey={created} editable={administersEverything(me)} onDone={onDone} />;
  }
  return (
    <form className={styles.panel} onSubmit={submit} aria-labelledby={titleId}>
      <h2 id={titleId} className={styles.sectionTitle}>
        {t("relays.add.title")}
      </h2>
      {sites?.length === 0 ? (
        <p className={styles.sectionNote}>
          {rich("relays.add.noSite", { link: <Link href={SITES_PATH}>{t("relays.add.createSite")}</Link> })}
        </p>
      ) : (
        <>
          <p className={styles.sectionNote}>{t("relays.add.note")}</p>
          <label className={styles.field}>
            {t("relays.add.site")}
            <select className={fieldClass} value={siteId || sites?.[0]?.id} onChange={(e) => setSiteId(e.target.value)}>
              {sites?.map((site) => (
                <option key={site.id} value={site.id}>
                  {placeLabel(site.customer_name, site.name, multiCustomer(me))}
                </option>
              ))}
            </select>
          </label>
        </>
      )}
      <div className={styles.formFoot}>
        {sites && sites.length > 0 && (
          <Button variant="k" type="submit" disabled={busy}>
            {t("relays.add.show")}
          </Button>
        )}
        <Button onClick={onDone}>{t("relays.cancel")}</Button>
      </div>
      {error && (
        <p className={styles.formError} role="alert">
          {error}
        </p>
      )}
    </form>
  );
}

/**
 * What a waiting relay reported, for an administrator to confirm. A name
 * goes on its certificate only from here, and agents then trust whoever
 * holds that name (ADR-0066), so the address it enrolled from stands next
 * to them, and a name can be left out, or added when agents use another.
 */
export function ConfirmRelayNames({ relay, onConfirmed }: { relay: Relay; onConfirmed: () => void }) {
  const titleId = useId();
  const reported = relay.pending_names ?? [];
  const [left, setLeft] = useState<string[]>([]);
  const [other, setOther] = useState("");
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const names = [...reported.filter((name) => !left.includes(name)), ...other.split(/[\s,]+/).filter(Boolean)];

  async function submit(event: FormEvent) {
    event.preventDefault();
    setBusy(true);
    setError(null);
    try {
      await api.confirmRelayNames(relay.id, names);
      onConfirmed();
    } catch (e) {
      setError(t("relays.confirm.failed", { error: describeError(e) }));
    } finally {
      setBusy(false);
    }
  }

  return (
    <form className={cx(styles.panel, styles.created)} onSubmit={submit} aria-labelledby={titleId}>
      <h2 id={titleId} className={styles.sectionTitle}>
        {t("relays.confirm.title", { name: relay.peer_id })}
      </h2>
      <p className={styles.sectionNote}>
        {relay.pending_source
          ? rich("relays.confirm.joinedFrom", {
              site: relay.site_name,
              source: <code>{relay.pending_source}</code>,
              link: <Link href={relayHref(relay.id)}>{t("relays.confirm.remove")}</Link>,
            })
          : rich("relays.confirm.joined", {
              site: relay.site_name,
              link: <Link href={relayHref(relay.id)}>{t("relays.confirm.remove")}</Link>,
            })}
      </p>
      {reported.map((name) => (
        <label key={name} className={styles.check}>
          <input
            type="checkbox"
            checked={!left.includes(name)}
            onChange={(e) => setLeft(e.target.checked ? left.filter((n) => n !== name) : [...left, name])}
          />{" "}
          <span className={styles.mono}>{name}</span>
        </label>
      ))}
      <label className={styles.field}>
        <span>
          {reported.length === 0 ? t("relays.confirm.addresses") : t("relays.confirm.another")}{" "}
          <span className={styles.fieldNote}>{t("relays.confirm.onlyIf")}</span>
        </span>
        <input
          className={fieldClass}
          value={other}
          onChange={(e) => setOther(e.target.value)}
          placeholder="relay1.paris.example"
          spellCheck={false}
          autoCapitalize="none"
        />
      </label>
      <div className={styles.formFoot}>
        <Button variant="k" type="submit" disabled={busy || names.length === 0}>
          {t("relays.confirm.submit")}
        </Button>
      </div>
      {error && (
        <p className={styles.formError} role="alert">
          {error}
        </p>
      )}
    </form>
  );
}
