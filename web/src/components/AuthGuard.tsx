"use client";

import { useRouter } from "next/navigation";
import { type ReactNode, Suspense, useEffect, useState } from "react";

import { AppShell } from "@/components/shell/AppShell";
import { Banner, LoadingBlock } from "@/components/ui/PageState";
import {
  ApiError,
  LOGIN_PATH,
  type Me,
  api,
  describeError,
  isAbortError,
  setUnauthorizedHandler,
} from "@/lib/api";
import { MeContext } from "@/lib/me";

import styles from "./AuthGuard.module.css";

/**
 * Renders `children` in the app shell once `/api/auth/me` confirms a
 * session, and sends the browser to the login page on any 401. This is UX
 * only: the API checks the session on every request.
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
      <main id="main" className={styles.main}>
        <Banner tone="bad">{error}</Banner>
      </main>
    );
  }
  const loading = (
    <main id="main" className={styles.main}>
      <LoadingBlock label="Loading…" />
    </main>
  );
  if (!user) return loading;
  return (
    <MeContext.Provider value={user}>
      <Suspense fallback={loading}>
        <AppShell me={user}>{children}</AppShell>
      </Suspense>
    </MeContext.Provider>
  );
}
