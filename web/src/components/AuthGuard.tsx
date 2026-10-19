"use client";

import Link from "next/link";
import { useRouter } from "next/navigation";
import { type ReactNode, useEffect, useState } from "react";

import {
  ApiError,
  LOGIN_PATH,
  type Me,
  api,
  describeError,
  isAbortError,
  setUnauthorizedHandler,
} from "@/lib/api";
import { MeContext, isAdministrator } from "@/lib/me";
import { RELAYS_PATH } from "@/lib/routes";

import styles from "./AuthGuard.module.css";

/**
 * Renders `children` once `/api/auth/me` confirms a session, and sends the
 * browser to the login page on any 401. This is UX only: the API checks the
 * session on every request.
 */
export function AuthGuard({ children }: { children: ReactNode }) {
  const router = useRouter();
  const [user, setUser] = useState<Me | null>(null);
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    const restore = setUnauthorizedHandler(() => router.replace(LOGIN_PATH));
    const controller = new AbortController();
    api.me(controller.signal).then(setUser, (e: unknown) => {
      // A 401 has already been routed to the login page.
      if (!isAbortError(e) && !(e instanceof ApiError && e.status === 401)) {
        setError(describeError(e));
      }
    });
    return () => {
      controller.abort();
      restore();
    };
  }, [router]);

  if (error) {
    return (
      <main className={styles.main}>
        <p className={styles.error} role="alert">
          {error}
        </p>
      </main>
    );
  }
  if (!user) {
    return (
      <main className={styles.main}>
        <p role="status">Loading…</p>
      </main>
    );
  }
  return (
    <MeContext.Provider value={user}>
      <a className="visually-hidden" href="#main">
        Skip to content
      </a>
      <AppHeader user={user} />
      <main id="main" className={styles.main}>
        {children}
      </main>
    </MeContext.Provider>
  );
}

function AppHeader({ user }: { user: Me }) {
  const router = useRouter();
  const [error, setError] = useState<string | null>(null);
  const roles = [...new Set(user.roles.map((grant) => grant.role))].join(", ") || "no role";

  async function logout() {
    setError(null);
    try {
      await api.logout();
      router.replace(LOGIN_PATH);
    } catch (e) {
      setError(`Sign-out failed. ${describeError(e)}`);
    }
  }

  return (
    <header className={styles.header}>
      <span className={styles.brand}>MonitorFlow</span>
      <nav aria-label="Main">
        <Link href="/">Fleet</Link>
        <Link href="/incidents/">Incidents</Link>
        {isAdministrator(user) && <Link href={RELAYS_PATH}>Administration</Link>}
      </nav>
      <div className={styles.user}>
        <span>
          Signed in as <strong>{user.username}</strong> <span className={styles.roles}>({roles})</span>
        </span>
        <button className={styles.logout} type="button" onClick={logout}>
          Sign out
        </button>
      </div>
      {error && (
        <p className={styles.error} role="alert">
          {error}
        </p>
      )}
    </header>
  );
}
