"use client";

import { type FormEvent, useState } from "react";

import { t } from "@/i18n";
import { api, describeError } from "@/lib/api";
import { formatBytes } from "@/lib/format";
import { usePolledData } from "@/lib/usePolledData";

import styles from "./Mirrors.module.css";
import { Button, fieldClass } from "./ui/Button";
import { Banner } from "./ui/PageState";

const GIB = 1024 ** 3;

/**
 * What on-demand mirrors keep against their cap, and the cap itself
 * (ADR-0142). Nothing is shown until the server answers: the mirror list
 * above is what the page is for.
 */
export function MirrorCache() {
  const { data, refresh } = usePolledData(api.mirrorCache);
  const [draft, setDraft] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  const [message, setMessage] = useState<{ tone: "ok" | "bad"; text: string } | null>(null);
  if (!data) return null;

  const save = async (capBytes: number | null) => {
    setBusy(true);
    setMessage(null);
    try {
      await api.setMirrorCache(capBytes);
      setDraft(null);
      setMessage({ tone: "ok", text: t("mirrors.cache.saved") });
      refresh();
    } catch (e) {
      setMessage({ tone: "bad", text: t("mirrors.cache.notSaved", { error: describeError(e) }) });
    } finally {
      setBusy(false);
    }
  };
  const value = draft ?? String(Math.round((data.cap_bytes / GIB) * 10) / 10);
  const gib = Number(value.replace(",", "."));
  const valid = Number.isFinite(gib) && gib >= 1;
  const submit = (event: FormEvent) => {
    event.preventDefault();
    if (valid) void save(Math.round(gib * GIB));
  };

  return (
    <section className={styles.cache} aria-labelledby="mirror-cache-title">
      <h2 id="mirror-cache-title">{t("mirrors.cache.title")}</h2>
      <p>
        {t("mirrors.cache.used", { used: formatBytes(data.used_bytes), cap: formatBytes(data.cap_bytes) })}{" "}
        {t("mirrors.cache.packages", { count: data.packages })}
      </p>
      {data.over_cap && <Banner tone="warn">{t("mirrors.cache.over")}</Banner>}
      <p className={styles.note}>{t("mirrors.cache.help")}</p>
      <form className={styles.capForm} onSubmit={submit}>
        <label>
          {t("mirrors.cache.cap")}
          <input
            className={fieldClass}
            inputMode="decimal"
            value={value}
            aria-invalid={!valid}
            onChange={(e) => setDraft(e.target.value)}
          />
        </label>
        <Button type="submit" size="small" disabled={busy || !valid}>
          {t("mirrors.cache.save")}
        </Button>
        {data.cap_set && (
          <Button size="small" disabled={busy} onClick={() => void save(null)}>
            {t("mirrors.cache.useDefault", { size: formatBytes(data.default_cap_bytes) })}
          </Button>
        )}
      </form>
      {!valid && <p className={styles.note}>{t("mirrors.cache.tooSmall")}</p>}
      {message && (
        <Banner tone={message.tone} role={message.tone === "ok" ? "status" : undefined}>
          {message.text}
        </Banner>
      )}
    </section>
  );
}
