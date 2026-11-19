"use client";

import Link from "next/link";
import { useRouter } from "next/navigation";
import { type FormEvent, useEffect, useRef, useState } from "react";

import { Button } from "@/components/ui/Button";
import { Banner } from "@/components/ui/PageState";
import { Logo } from "@/components/ui/Logo";
import { rich, t } from "@/i18n";
import {
  ApiError,
  LOGIN_PATH,
  type LinkOwner,
  api,
  describeError,
} from "@/lib/api";

import { takeFragmentSecret } from "./fragmentSecret";
import { NewPasswordFields } from "./NewPasswordFields";
import styles from "./LoginForm.module.css";

type Failure = { kind: "mismatch" } | { kind: "other"; message: string };

/**
 * `/welcome/#<token>` (ADR-0059): the link an administrator gives to invite
 * a user or reset their password. The user chooses a password and is
 * signed in.
 */
export function WelcomeForm() {
  const router = useRouter();
  // `undefined` while checking the link, `null` when it is missing or dead.
  const [owner, setOwner] = useState<LinkOwner | null | undefined>(undefined);
  const [password, setPassword] = useState("");
  const [confirm, setConfirm] = useState("");
  const [pending, setPending] = useState(false);
  const [failure, setFailure] = useState<Failure | null>(null);

  const secret = useRef<string | null>(null);

  useEffect(() => {
    takeFragmentSecret(secret);
    if (!secret.current) {
      setOwner(null);
      return;
    }
    let current = true;
    api.inspectPasswordLink(secret.current).then(
      (found) => current && setOwner(found),
      (e) => {
        if (!current) return;
        if (e instanceof ApiError && e.status === 410) setOwner(null);
        // Unknown: the form says what is wrong when it is sent.
        else setFailure({ kind: "other", message: describeError(e) });
      },
    );
    return () => {
      current = false;
    };
  }, []);

  async function onSubmit(event: FormEvent<HTMLFormElement>) {
    event.preventDefault();
    if (password !== confirm) {
      setFailure({ kind: "mismatch" });
      return;
    }
    setPending(true);
    setFailure(null);
    try {
      await api.usePasswordLink(secret.current ?? "", password);
      router.replace("/");
    } catch (e) {
      if (e instanceof ApiError && e.status === 410) {
        setOwner(null);
      } else {
        setFailure({
          kind: "other",
          message:
            e instanceof ApiError && e.status === 400
              ? `${e.message.charAt(0).toUpperCase()}${e.message.slice(1)}.`
              : describeError(e),
        });
      }
      setPending(false);
    }
  }

  return (
    <div className={styles.page}>
      <main id="main" className={styles.main}>
        <h1 className={styles.brand}>
          <Logo size={32} />
          MonitorFlow
        </h1>
        {owner === null ? (
          <div className={styles.box}>
            <h2 className={styles.title}>{t("auth.welcome.invalidTitle")}</h2>
            <Banner tone="warn">
              <strong>{t("auth.welcome.invalid")}</strong> {t("auth.welcome.invalidText")}
            </Banner>
            <p className={styles.text}>
              {rich("auth.welcome.askNew", {
                signIn: <Link href={LOGIN_PATH}>{t("auth.welcome.signIn")}</Link>,
              })}
            </p>
          </div>
        ) : owner === undefined && !failure ? null : (
          <form className={styles.box} onSubmit={onSubmit} aria-labelledby="welcome-title">
            <h2 id="welcome-title" className={styles.title}>
              {owner?.invited === false ? t("auth.welcome.newPassword") : t("auth.welcome.title")}
            </h2>
            {owner && (
              <p className={styles.text}>
                {rich(owner.invited ? "auth.welcome.invited" : "auth.welcome.reset", {
                  username: <strong>{owner.username}</strong>,
                })}
              </p>
            )}
            {failure?.kind === "mismatch" && (
              <Banner tone="bad">
                <strong>{t("auth.mismatch.title")}</strong> {t("auth.mismatch.text")}
              </Banner>
            )}
            {failure?.kind === "other" && <Banner tone="bad">{failure.message}</Banner>}
            {/* Lets password managers save the username with the password. */}
            <input type="hidden" name="username" autoComplete="username" value={owner?.username ?? ""} />
            <NewPasswordFields
              password={password}
              confirm={confirm}
              mismatch={failure?.kind === "mismatch"}
              onPassword={setPassword}
              onConfirm={setConfirm}
            />
            <Button className={styles.submit} variant="k" size="large" type="submit" disabled={pending}>
              {pending ? t("auth.welcome.saving") : t("auth.welcome.save")}
            </Button>
          </form>
        )}
      </main>
    </div>
  );
}
