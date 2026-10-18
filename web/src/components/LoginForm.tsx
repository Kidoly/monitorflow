"use client";

import { useRouter } from "next/navigation";
import { type FormEvent, useState } from "react";

import { ApiError, api, describeError } from "@/lib/api";

import styles from "./LoginForm.module.css";

export function LoginForm() {
  const router = useRouter();
  const [username, setUsername] = useState("");
  const [password, setPassword] = useState("");
  const [pending, setPending] = useState(false);
  const [error, setError] = useState<string | null>(null);

  async function onSubmit(event: FormEvent<HTMLFormElement>) {
    event.preventDefault();
    setPending(true);
    setError(null);
    try {
      await api.login(username, password);
      router.replace("/");
    } catch (e) {
      setPassword("");
      setError(
        e instanceof ApiError && e.status === 401
          ? "Invalid username or password."
          : describeError(e),
      );
      setPending(false);
    }
  }

  return (
    <main className={styles.page}>
      <form className={styles.card} onSubmit={onSubmit} aria-describedby={error ? "login-error" : undefined}>
        <h1 className={styles.title}>Sign in to MonitorFlow</h1>
        <label className={styles.label} htmlFor="username">
          Username
        </label>
        <input
          className={styles.input}
          id="username"
          name="username"
          autoComplete="username"
          autoCapitalize="none"
          spellCheck={false}
          required
          value={username}
          onChange={(e) => setUsername(e.target.value)}
        />
        <label className={styles.label} htmlFor="password">
          Password
        </label>
        <input
          className={styles.input}
          id="password"
          name="password"
          type="password"
          autoComplete="current-password"
          required
          value={password}
          onChange={(e) => setPassword(e.target.value)}
        />
        {error && (
          <p id="login-error" className={styles.error} role="alert">
            {error}
          </p>
        )}
        <button className={styles.submit} type="submit" disabled={pending}>
          {pending ? "Signing in…" : "Sign in"}
        </button>
      </form>
    </main>
  );
}
