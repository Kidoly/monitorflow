"use client";

import Link from "next/link";
import { type FormEvent, useEffect, useId, useState } from "react";

import { Button, fieldClass } from "@/components/ui/Button";
import { cx } from "@/components/ui/cx";
import { type CreatedInstallKey, type Me, type Relay, type Site, api, describeError } from "@/lib/api";
import { administersCustomer, administersEverything } from "@/lib/me";
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
        if (!controller.signal.aborted) setError(`Loading the sites failed. ${describeError(e)}`);
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
      setError(`Creating the install command failed. ${describeError(e)}`);
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
        Add a relay
      </h2>
      {sites?.length === 0 ? (
        <p className={styles.sectionNote}>
          A relay belongs to a site: <Link href={SITES_PATH}>create one first</Link>.
        </p>
      ) : (
        <>
          <p className={styles.sectionNote}>
            One command on one host of the site. The relay finds its own names and appears here for you to confirm them.
          </p>
          <label className={styles.field}>
            Site
            <select className={fieldClass} value={siteId || sites?.[0]?.id} onChange={(e) => setSiteId(e.target.value)}>
              {sites?.map((site) => (
                <option key={site.id} value={site.id}>
                  {site.customer_name} › {site.name}
                </option>
              ))}
            </select>
          </label>
        </>
      )}
      <div className={styles.formFoot}>
        {sites && sites.length > 0 && (
          <Button variant="k" type="submit" disabled={busy}>
            Show the install command
          </Button>
        )}
        <Button onClick={onDone}>Cancel</Button>
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
      setError(`Confirming the names failed. ${describeError(e)}`);
    } finally {
      setBusy(false);
    }
  }

  return (
    <form className={cx(styles.panel, styles.created)} onSubmit={submit} aria-labelledby={titleId}>
      <h2 id={titleId} className={styles.sectionTitle}>
        Confirm {relay.peer_id}
      </h2>
      <p className={styles.sectionNote}>
        This relay joined {relay.site_name}
        {relay.pending_source && (
          <>
            {" "}
            from <code>{relay.pending_source}</code>
          </>
        )}
        . If you did not install it, <Link href={relayHref(relay.id)}>remove it</Link>. Keep the names servers reach it
        at: the first one goes in their install command. Until you confirm, it carries no server.
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
          {reported.length === 0 ? "Names" : "Another name"}{" "}
          <span className={styles.fieldNote}>— only if servers reach it some other way</span>
        </span>
        <input
          className={cx(fieldClass, styles.mono)}
          value={other}
          onChange={(e) => setOther(e.target.value)}
          placeholder="relay1.paris.example"
          spellCheck={false}
          autoCapitalize="none"
        />
      </label>
      <div className={styles.formFoot}>
        <Button variant="k" type="submit" disabled={busy || names.length === 0}>
          Confirm names
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
