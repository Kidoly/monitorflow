"use client";

import { type FormEvent, type ReactNode, useState } from "react";

import { rich, t } from "@/i18n";
import {
  ApiError,
  type PsaConnection,
  type PsaConnectionRequest,
  type PsaConnectionTest,
  api,
  describeError,
} from "@/lib/api";
import { administersEverything, useMe } from "@/lib/me";
import { INTEGRATIONS_PATH } from "@/lib/routes";
import { usePolledData } from "@/lib/usePolledData";

import styles from "./AdminPages.module.css";
import own from "./IntegrationsCatalogue.module.css";
import { psaStatus } from "./IntegrationsCatalogue";
import { PsaSiteLinks } from "./PsaSiteLinks";
import { PsaTickets } from "./PsaTickets";
import { Breadcrumb } from "./ui/Breadcrumb";
import { Button, fieldClass } from "./ui/Button";
import { InlineConfirm } from "./ui/InlineConfirm";
import { Banner, ErrorState, LoadingBlock, NoPermission, PageHeader } from "./ui/PageState";
import { RelativeTime } from "./ui/RelativeTime";
import { Label } from "./ui/StatusLabel";

/** `/settings/integrations/halopsa/`: global administrators only, like the API (ADR-0164). */
export function HaloPsaPage() {
  const me = useMe();
  if (!administersEverything(me)) {
    return <NoPermission title={t("psa.needGlobal")}>{t("psa.needGlobalBody")}</NoPermission>;
  }
  return <HaloPsa />;
}

/** The address as the server keeps it, to tell whether the saved secret still applies. */
export function sameAddress(typed: string, saved: string): boolean {
  const clean = (url: string) => url.trim().replace(/\/+$/, "").replace(/\/(api|auth)$/, "");
  return clean(typed) === clean(saved);
}

function HaloPsa() {
  const { data, error, refresh } = usePolledData(api.psaConnection);
  const crumb = <Breadcrumb parents={[{ label: t("integrations.title"), href: INTEGRATIONS_PATH }]} />;
  const status = psaStatus(data?.connection);
  const header = (
    <PageHeader
      title={t("psa.catalogue.halopsa")}
      status={status && <Label tone={status.tone}>{status.text}</Label>}
    />
  );
  if (!data) {
    return (
      <div className={styles.page}>
        {crumb}
        {header}
        {error instanceof ApiError && error.status === 403 ? (
          <NoPermission title={t("psa.needGlobal")} />
        ) : error ? (
          <ErrorState what={t("psa.what")} error={describeError(error)} onRetry={refresh} />
        ) : (
          <LoadingBlock />
        )}
      </div>
    );
  }
  return (
    <div className={styles.page}>
      {crumb}
      {header}
      <p className={styles.footnote}>{t("psa.intro")}</p>
      <ConnectionForm key={data.connection?.updated_at ?? "new"} saved={data.connection} onSaved={refresh} />
      {data.connection ? (
        <>
          <PsaSiteLinks key={data.connection.base_url} />
          <PsaTickets key={`tickets-${data.connection.base_url}`} connection={data.connection} onSaved={refresh} />
        </>
      ) : (
        <p className={styles.footnote}>
          <strong>{t("psa.step.sites")}</strong> {t("psa.sites.saveFirst")}
        </p>
      )}
    </div>
  );
}

type TestState = PsaConnectionTest | "testing" | null;

function ConnectionForm({ saved, onSaved }: { saved: PsaConnection | null; onSaved: () => void }) {
  const [baseUrl, setBaseUrl] = useState(saved?.base_url ?? "");
  const [tenant, setTenant] = useState(saved?.tenant ?? "");
  const [clientId, setClientId] = useState(saved?.client_id ?? "");
  const [secret, setSecret] = useState("");
  const [enabled, setEnabled] = useState(saved?.enabled ?? true);
  const [test, setTest] = useState<TestState>(null);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [done, setDone] = useState(false);

  const secretKept = saved !== null && saved.secret_error === null && sameAddress(baseUrl, saved.base_url);
  const ready = baseUrl.trim() !== "" && clientId.trim() !== "" && (secretKept || secret.trim() !== "");
  const body: PsaConnectionRequest = {
    base_url: baseUrl.trim(),
    tenant: tenant.trim() || null,
    client_id: clientId.trim(),
    client_secret: secret,
    enabled,
  };

  function changed() {
    setTest(null);
    setDone(false);
  }

  async function runTest() {
    setTest("testing");
    try {
      setTest(await api.testPsaConnection(body));
    } catch (e) {
      setTest({ ok: false, agent_name: null, error: describeError(e) });
    }
  }

  async function submit(event: FormEvent) {
    event.preventDefault();
    setBusy(true);
    setError(null);
    try {
      await api.savePsaConnection(body);
      setDone(true);
      onSaved();
    } catch (e) {
      setError(describeError(e));
    } finally {
      setBusy(false);
    }
  }

  let secretNote: ReactNode = t("psa.secretNote");
  if (saved?.secret_error) secretNote = t("psa.secretBroken", { error: saved.secret_error });
  else if (saved && !secretKept) secretNote = t("psa.secretAgain");

  return (
    <>
      <form className={styles.panel} onSubmit={submit} aria-label={t("psa.step.connect")}>
        <h2 className={styles.panelTitle}>{t("psa.step.connect")}</h2>
        <section aria-label={t("psa.where.title")}>
          <p className={styles.fieldNote}>{t("psa.where.title")}</p>
          <ol className={own.where}>
            <li>{t("psa.where.application")}</li>
            <li>{t("psa.where.method")}</li>
            <li>{t("psa.where.permissions")}</li>
            <li>{t("psa.where.copy")}</li>
          </ol>
        </section>
        <label className={styles.field}>
          {t("psa.address")}
          <input
            className={fieldClass}
            type="url"
            inputMode="url"
            value={baseUrl}
            onChange={(e) => {
              setBaseUrl(e.target.value);
              changed();
            }}
            placeholder="https://halo.example.com"
            spellCheck={false}
            required
          />
          <span className={styles.fieldNote}>{t("psa.addressNote")}</span>
        </label>
        <label className={styles.field}>
          {t("psa.clientId")}
          <input
            className={fieldClass}
            value={clientId}
            onChange={(e) => {
              setClientId(e.target.value);
              changed();
            }}
            autoComplete="off"
            spellCheck={false}
            required
          />
        </label>
        <label className={styles.field}>
          {t("psa.clientSecret")}
          <input
            className={fieldClass}
            type="password"
            autoComplete="new-password"
            value={secret}
            onChange={(e) => {
              setSecret(e.target.value);
              changed();
            }}
            placeholder={secretKept ? t("psa.secretKept") : undefined}
            required={!secretKept}
          />
          <span className={styles.fieldNote}>{secretNote}</span>
        </label>
        <label className={styles.field}>
          {t("psa.tenant")}
          <input
            className={fieldClass}
            value={tenant}
            onChange={(e) => {
              setTenant(e.target.value);
              changed();
            }}
            autoComplete="off"
            spellCheck={false}
          />
          <span className={styles.fieldNote}>{t("psa.tenantNote")}</span>
        </label>
        <label>
          <input
            type="checkbox"
            checked={enabled}
            onChange={(e) => {
              setEnabled(e.target.checked);
              changed();
            }}
          />{" "}
          {t("psa.enabled")}
          <span className={styles.fieldNote}> {t("psa.enabledNote")}</span>
        </label>
        {error && (
          <p className={styles.formError} role="alert">
            {error}
          </p>
        )}
        <div className={styles.formFoot}>
          <Button disabled={!ready || test === "testing"} onClick={() => void runTest()}>
            {test === "testing" ? t("psa.testing") : t("psa.test")}
          </Button>
          <Button type="submit" variant="k" disabled={busy || !ready}>
            {busy ? t("psa.saving") : t("psa.save")}
          </Button>
          <TestLine test={test} />
          {done && <span className={styles.dim}>{t("psa.saved")}</span>}
        </div>
        <span className={styles.fieldNote}>{t("psa.testNote")}</span>
      </form>
      {saved && <SavedState saved={saved} />}
      {saved && (
        <div className={styles.buttons}>
          <InlineConfirm
            trigger={t("psa.remove")}
            title={t("psa.removeTitle")}
            consequence={t("psa.removeConsequence")}
            confirmLabel={t("psa.remove")}
            onConfirm={async () => {
              await api.deletePsaConnection();
              onSaved();
            }}
            describeError={describeError}
          />
        </div>
      )}
    </>
  );
}

function TestLine({ test }: { test: TestState }) {
  if (test === null || test === "testing") return null;
  return test.ok ? (
    <span role="status">
      <Label tone="ok">{t("psa.testOk", { agent: test.agent_name ?? "" })}</Label>
    </span>
  ) : (
    <span role="alert">
      <Label tone="bad">{t("psa.testFailed")}</Label> {test.error}
    </span>
  );
}

/** The saved connection's last success and last error. */
function SavedState({ saved }: { saved: PsaConnection }) {
  const now = new Date();
  return (
    <div className={styles.stack}>
      {saved.last_success_at ? (
        <span>{rich("psa.state.lastSuccess", { when: <RelativeTime value={saved.last_success_at} now={now} /> })}</span>
      ) : (
        <span>{t("psa.state.never")}</span>
      )}
      {saved.last_error && saved.last_error_at && (
        <Banner tone="bad">
          {rich("psa.state.lastError", {
            when: <RelativeTime value={saved.last_error_at} now={now} />,
            error: saved.last_error,
          })}
        </Banner>
      )}
    </div>
  );
}
