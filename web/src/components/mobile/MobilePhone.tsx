"use client";

import { useRouter } from "next/navigation";
import { useCallback, useEffect, useState } from "react";

import { Button } from "@/components/ui/Button";
import { Banner, ErrorState, LoadingBlock } from "@/components/ui/PageState";
import { Label } from "@/components/ui/StatusLabel";
import { formatSince } from "@/components/ui/time";
import { t } from "@/i18n";
import { ApiError, LOGIN_PATH, type Phone, type PhoneList, api, describeError } from "@/lib/api";
import { type PushSupport, base64UrlToBytes, isStandalone, phoneName, pushSupport } from "@/lib/mobile";

import { registerServiceWorker } from "./MobileShell";
import styles from "./Mobile.module.css";

function currentSupport(): PushSupport {
  if (typeof window === "undefined") return "unsupported";
  return pushSupport({
    userAgent: navigator.userAgent,
    standalone: isStandalone(),
    hasPushManager: "PushManager" in window,
    hasServiceWorker: "serviceWorker" in navigator,
    permission: "Notification" in window ? Notification.permission : null,
  });
}

/** The browser's subscription for `key`, made when there is none. */
async function subscribe(key: string): Promise<PushSubscriptionJSON> {
  if ((await Notification.requestPermission()) !== "granted") throw new Error("denied");
  const registration = (await registerServiceWorker()) ?? (await navigator.serviceWorker.ready);
  const existing = await registration.pushManager.getSubscription();
  const subscription =
    existing ??
    (await registration.pushManager.subscribe({ userVisibleOnly: true, applicationServerKey: base64UrlToBytes(key) }));
  return subscription.toJSON();
}

async function unsubscribe(): Promise<void> {
  const registration = await navigator.serviceWorker?.getRegistration();
  const subscription = await registration?.pushManager.getSubscription();
  await subscription?.unsubscribe();
}

/**
 * This phone and the person's others (ADR-0186): adding it keeps the
 * session for weeks, notifications come by Web Push, and any phone can be
 * removed, which signs it out.
 */
export function MobilePhone() {
  const router = useRouter();
  const [list, setList] = useState<PhoneList | null>(null);
  const [error, setError] = useState<unknown>(null);
  // Rendered only once the shell knows who is signed in, so in the browser.
  const [support, setSupport] = useState<PushSupport>(currentSupport);
  const [name, setName] = useState(() => (typeof navigator === "undefined" ? "" : phoneName(navigator.userAgent)));
  const [busy, setBusy] = useState(false);
  const [result, setResult] = useState<{ tone: "ok" | "bad"; text: string } | null>(null);
  const [removing, setRemoving] = useState<string | null>(null);

  const load = useCallback(() => {
    api.phones().then(
      (l) => {
        setList(l);
        setError(null);
      },
      (e: unknown) => setError(e),
    );
  }, []);

  useEffect(load, [load]);

  async function run(action: () => Promise<string | null>) {
    setBusy(true);
    setResult(null);
    try {
      const text = await action();
      if (text) setResult({ tone: "ok", text });
      load();
    } catch (e) {
      const denied = e instanceof Error && e.message === "denied";
      // A test's own error is a sentence from the server already.
      const own = e instanceof Error && !(e instanceof ApiError) && e.message !== "" ? e.message : null;
      setResult({ tone: "bad", text: denied ? t("mobile.push.denied") : (own ?? describeError(e)) });
      setSupport(currentSupport());
    } finally {
      setBusy(false);
    }
  }

  if (!list) return error ? <ErrorState what={t("mobile.phonesWhat")} error={describeError(error)} /> : <LoadingBlock />;
  const mine = list.phones.find((p) => p.this_phone) ?? null;
  const others = list.phones.filter((p) => !p.this_phone);

  const add = () => run(async () => (await api.addPhone(name), t("mobile.added")));
  const pushOn = (phone: Phone) =>
    run(async () => {
      await api.setPhonePush(phone.id, await subscribe(list.push_key));
      return t("mobile.push.on");
    });
  const pushOff = (phone: Phone) =>
    run(async () => {
      await unsubscribe();
      await api.deletePhonePush(phone.id);
      return t("mobile.push.off");
    });
  const test = (phone: Phone) =>
    run(async () => {
      const outcome = await api.testPhone(phone.id);
      if (!outcome.ok) throw new Error(outcome.error ?? "");
      return t("mobile.push.testSent");
    });
  const remove = (phone: Phone) =>
    run(async () => {
      if (phone.this_phone) await unsubscribe().catch(() => {});
      await api.removePhone(phone.id);
      setRemoving(null);
      if (phone.this_phone) router.replace(LOGIN_PATH);
      return t("mobile.removed", { name: phone.name });
    });

  const supportNote =
    support === "install"
      ? t("mobile.push.install")
      : support === "denied"
        ? t("mobile.push.denied")
        : support === "unsupported"
          ? t("mobile.push.unsupported")
          : null;

  return (
    <>
      <h1 className={styles.title}>{t("mobile.phoneTitle")}</h1>
      {result && <Banner tone={result.tone}>{result.text}</Banner>}
      {!mine ? (
        <section className={styles.section} aria-labelledby="add-title">
          <h2 id="add-title" className={styles.sectionTitle}>
            {t("mobile.addTitle")}
          </h2>
          <p className={styles.muted}>{t("mobile.addHint", { days: list.session_days })}</p>
          <label className={styles.field}>
            {t("mobile.name")}
            <input value={name} maxLength={80} onChange={(e) => setName(e.target.value)} autoComplete="off" />
          </label>
          <div className={styles.actions}>
            <Button variant="k" size="large" disabled={busy || name.trim() === ""} onClick={() => void add()}>
              {t("mobile.add")}
            </Button>
          </div>
        </section>
      ) : (
        <section className={styles.section} aria-labelledby="mine-title">
          <h2 id="mine-title" className={styles.sectionTitle}>
            {mine.name}
          </h2>
          <p className={styles.muted}>{t("mobile.sessionNote", { days: list.session_days, idle: list.idle_days })}</p>
          <span className={styles.cardTop}>
            <Label tone={mine.push ? "ok" : "neutral"} small>
              {mine.push ? t("mobile.push.stateOn") : t("mobile.push.stateOff")}
            </Label>
            {mine.push_sent_at && (
              <span className={styles.muted}>{t("mobile.push.lastSent", { time: formatSince(new Date(mine.push_sent_at), new Date()) })}</span>
            )}
          </span>
          {mine.push_error && <Banner tone="bad">{mine.push_error}</Banner>}
          {supportNote && !mine.push && <Banner tone="warn">{supportNote}</Banner>}
          <p className={styles.muted}>{t("mobile.push.when")}</p>
          <div className={styles.actions}>
            {!mine.push && support === "ready" && (
              <Button variant="k" size="large" disabled={busy} onClick={() => void pushOn(mine)}>
                {t("mobile.push.turnOn")}
              </Button>
            )}
            {mine.push && (
              <>
                <Button size="large" disabled={busy} onClick={() => void test(mine)}>
                  {t("mobile.push.test")}
                </Button>
                <Button size="large" disabled={busy} onClick={() => void pushOff(mine)}>
                  {t("mobile.push.turnOff")}
                </Button>
              </>
            )}
          </div>
        </section>
      )}
      <section className={styles.section} aria-labelledby="phones-title">
        <h2 id="phones-title" className={styles.sectionTitle}>
          {t("mobile.yourPhones")}
        </h2>
        {list.phones.length === 0 && <p className={styles.muted}>{t("mobile.noPhones")}</p>}
        <ul className={styles.list}>
          {[...(mine ? [mine] : []), ...others].map((phone) => (
            <li key={phone.id} className={styles.card}>
              <span className={styles.cardTop}>
                <span className={styles.cardTitle}>{phone.name}</span>
                {phone.this_phone && (
                  <Label tone="info" small>
                    {t("mobile.thisPhone")}
                  </Label>
                )}
              </span>
              <span className={styles.muted}>
                {t("mobile.lastUsed", { time: formatSince(new Date(phone.last_seen_at), new Date()) })} ·{" "}
                {phone.push ? t("mobile.push.stateOn") : t("mobile.push.stateOff")}
              </span>
              <div className={styles.actions}>
                {removing === phone.id ? (
                  <>
                    <p className={styles.mark}>{t("mobile.removeConfirm", { name: phone.name })}</p>
                    <Button variant="danger" size="large" disabled={busy} onClick={() => void remove(phone)}>
                      {t("mobile.remove")}
                    </Button>
                    <Button size="large" onClick={() => setRemoving(null)}>
                      {t("mobile.cancel")}
                    </Button>
                  </>
                ) : (
                  <Button size="large" onClick={() => setRemoving(phone.id)}>
                    {t("mobile.removeName", { name: phone.name })}
                  </Button>
                )}
              </div>
            </li>
          ))}
        </ul>
      </section>
    </>
  );
}
