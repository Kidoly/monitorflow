"use client";

import { useCallback } from "react";

import { t } from "@/i18n";
import { type WebsiteCertificate, type WebsiteCheck, api, describeError } from "@/lib/api";
import { formatNumber, formatSeconds } from "@/lib/format";
import { usePolledData } from "@/lib/usePolledData";
import { websiteHost } from "@/lib/website";
import { type Column, DataTable, PrimaryCell } from "@/components/ui/DataTable";
import { Missing } from "@/components/ui/Missing";
import { Banner } from "@/components/ui/PageState";
import { Label } from "@/components/ui/StatusLabel";

import styles from "./ServerPage.module.css";

/** "Signed by an authority this server does not trust": what is wrong with a certificate. */
export function certificateProblem(cert: WebsiteCertificate): string {
  const known = ["expired", "not_yet_valid", "self_signed", "untrusted", "name", "revoked"];
  return t(`server.websites.cert.${known.includes(cert.reason ?? "") ? cert.reason : "other"}` as "server.websites.cert.other");
}

/** "The page does not hold the text expected": why a run failed, in words. */
export function failureText(check: WebsiteCheck): string {
  const known = ["dns", "connect", "timeout", "tls", "certificate", "status", "content", "redirects", "refused", "protocol"];
  const reason = known.includes(check.reason ?? "") ? check.reason : "unknown";
  if (reason === "status" && check.status_code !== null) return t("server.websites.reason.statusCode", { code: check.status_code });
  return t(`server.websites.reason.${reason}` as "server.websites.reason.unknown");
}

/** Who runs it: this server's agent, the central server or a relay. */
export function runByText(check: WebsiteCheck): string {
  if (check.run_by === "central") return t("server.websites.runBy.central");
  if (check.run_by === "relay") return t("server.websites.runBy.relay", { relay: check.relay ?? "" });
  return t("server.websites.runBy.agent");
}

function Result({ check }: { check: WebsiteCheck }) {
  if (check.up === null) return <Label tone="neutral">{t("server.websites.waiting")}</Label>;
  if (check.up) return <Label tone="ok">{t("server.websites.answers")}</Label>;
  return <Label tone="bad">{t("server.websites.fails")}</Label>;
}

function Certificate({ cert, url }: { cert: WebsiteCertificate | null; url: string }) {
  if (!cert) {
    return url.toLowerCase().startsWith("https://") ? <Missing word="noData" /> : <Missing word="none" />;
  }
  const until = cert.not_after ? new Date(cert.not_after).toLocaleDateString(undefined, { dateStyle: "medium" }) : null;
  const days = cert.days_left !== null ? Math.floor(cert.days_left) : null;
  const left =
    days === null ? null : days < 0 ? t("server.websites.expiredAgo", { count: -days }) : t("server.websites.daysLeft", { count: days });
  return (
    <div className={styles.certificate}>
      {cert.ok ? (
        <Label tone={days !== null && days < 14 ? "warn" : "ok"}>{left ?? t("server.websites.valid")}</Label>
      ) : (
        <Label tone="bad">{certificateProblem(cert)}</Label>
      )}
      <span className={styles.certFacts}>
        {until && t("server.websites.until", { date: until })}
        {cert.issuer && ` · ${t("server.websites.issuer", { issuer: cert.issuer })}`}
      </span>
    </div>
  );
}

/**
 * The website checks of a server (ADR-0150): whether each answers as
 * expected and why not, its status, time and final address, its
 * certificate, and who runs it. Shown on the Sensors tab when the server
 * has any.
 */
export function WebsiteChecks({ serverId }: { serverId: string }) {
  const fetcher = useCallback((signal: AbortSignal) => api.websiteChecks(serverId, signal), [serverId]);
  const { data, error } = usePolledData(fetcher);
  if (error && !data) {
    return <Banner tone="warn">{t("server.websites.loadFailed", { error: describeError(error) })}</Banner>;
  }
  if (!data || data.checks.length === 0) return null;

  const columns: Column<WebsiteCheck>[] = [
    {
      key: "site",
      header: t("server.websites.column.site"),
      cell: (check) => (
        <PrimaryCell
          title={websiteHost(check.url) || check.name}
          sub={
            <span className={styles.why}>
              {check.up === false ? failureText(check) : check.url}
              {check.final_url && check.redirects ? ` → ${check.final_url}` : ""}
            </span>
          }
        />
      ),
    },
    { key: "result", header: t("server.websites.column.result"), cell: (check) => <Result check={check} /> },
    {
      key: "status",
      header: t("server.websites.column.status"),
      cell: (check) => (check.status_code !== null ? String(check.status_code) : <Missing word="noData" />),
    },
    {
      key: "time",
      header: t("server.websites.column.time"),
      cell: (check) =>
        check.response_seconds === null ? (
          <Missing word="noData" />
        ) : check.response_seconds < 1 ? (
          t("server.websites.ms", { ms: formatNumber(check.response_seconds * 1000, 0) })
        ) : (
          formatSeconds(check.response_seconds)
        ),
    },
    { key: "certificate", header: t("server.websites.column.certificate"), cell: (check) => <Certificate cert={check.certificate} url={check.url} /> },
    { key: "runBy", header: t("server.websites.column.runBy"), cell: runByText },
  ];

  return (
    <section className={styles.websites} aria-label={t("server.websites.title")}>
      <h3 className={styles.offTitle}>{t("server.websites.title")}</h3>
      <DataTable label={t("server.websites.title")} columns={columns} rows={data.checks} rowKey={(c) => `${c.name}/${c.instance}`} />
    </section>
  );
}

