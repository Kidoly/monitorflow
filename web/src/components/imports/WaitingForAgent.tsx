"use client";

import { useState } from "react";

import { t } from "@/i18n";
import { ApiError, type CreatedToken, type ServerDetail, api, describeError } from "@/lib/api";
import { administersCustomer, administersEverything, useMe } from "@/lib/me";

import { NewToken } from "../TokensAdmin";
import { Button } from "../ui/Button";
import { Banner } from "../ui/PageState";
import styles from "./Imports.module.css";

/** A week: the longest a single-use token lasts. */
const TOKEN_TTL_S = 7 * 24 * 60 * 60;

/**
 * On the page of a server an import made (ADR-0146): it waits for its
 * agent, and an administrator gets the install command of a token for its
 * name in one click.
 */
export function WaitingForAgent({ server }: { server: ServerDetail }) {
  const me = useMe();
  const [token, setToken] = useState<CreatedToken | null>(null);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const canMake = administersCustomer(me, server.customer_id);

  async function make() {
    setBusy(true);
    setError(null);
    try {
      setToken(
        await api.createToken({ role: "agent", peer_id: server.peer_id, site_id: server.site_id, ttl_s: TOKEN_TTL_S, server_names: [] }),
      );
    } catch (e) {
      setError(e instanceof ApiError && e.status < 500 ? e.message : describeError(e));
    } finally {
      setBusy(false);
    }
  }

  if (token) {
    return <NewToken token={token} editable={administersEverything(me)} onDone={() => setToken(null)} onAnother={() => void make()} />;
  }
  return (
    <section className={styles.box} aria-labelledby="waiting-title">
      <h2 id="waiting-title" className={styles.title}>
        {t("imports.waiting.title")}
      </h2>
      <p className={styles.note}>{t("imports.waiting.note", { name: server.peer_id })}</p>
      {error && <Banner tone="bad">{error}</Banner>}
      {canMake && (
        <div className={styles.buttons}>
          <Button variant="k" disabled={busy} onClick={() => void make()}>
            {busy ? t("imports.waiting.making") : t("imports.waiting.make")}
          </Button>
        </div>
      )}
    </section>
  );
}
