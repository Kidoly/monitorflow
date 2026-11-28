"use client";

import { useCallback } from "react";

import { t } from "@/i18n";
import { type MajorUpgradeCheck, type ServerDetail, type ServerUpgrade, api, describeError } from "@/lib/api";
import { releaseLabel } from "@/lib/campaigns";
import { rollbackText } from "@/lib/remediation";
import { majorUpgradeHref } from "@/lib/routes";
import { usePolledData } from "@/lib/usePolledData";

import admin from "./AdminPages.module.css";
import styles from "./ServerRollback.module.css";
import { ButtonLink } from "./ui/Button";
import { type Column, DataTable } from "./ui/DataTable";
import { InlineConfirm } from "./ui/InlineConfirm";
import { ErrorState } from "./ui/PageState";
import { RelativeTime } from "./ui/RelativeTime";
import { Label } from "./ui/StatusLabel";

/** The checks of `GET /api/major-upgrades`, central server's first. */
const CHECKS = [
  "allowed",
  "agent_version",
  "mirror",
  "up_to_date",
  "held_packages",
  "broken_packages",
  "free_space",
  "reboot_pending",
  "sources",
  "release_offered",
  "leapp",
  "third_party_sources",
] as const;

const checkName = (name: string) => {
  const known = CHECKS.find((c) => c === name);
  return known ? t(`campaigns.major.check.${known}`) : name;
};

const TONES = { ok: "ok", warning: "warn", failed: "bad" } as const;

/** One check: its name, its state and what was found, with the items it lists. */
function CheckLine({ check }: { check: MajorUpgradeCheck }) {
  return (
    <li>
      <Label small tone={TONES[check.state] ?? "neutral"}>
        {checkName(check.name)}
      </Label>{" "}
      {check.detail}
      {check.items.length > 0 && (
        <ul>
          {check.items.map((item) => (
            <li key={item}>
              <code>{item}</code>
            </li>
          ))}
        </ul>
      )}
    </li>
  );
}

/** Every check of a server, failed ones first. */
export function CheckList({ checks }: { checks: MajorUpgradeCheck[] }) {
  const order = { failed: 0, warning: 1, ok: 2 };
  const sorted = [...checks].sort((a, b) => order[a.state] - order[b.state]);
  return (
    <ul className={styles.checks} aria-label={t("campaigns.major.checks")}>
      {sorted.map((check) => (
        <CheckLine key={check.name} check={check} />
      ))}
    </ul>
  );
}

/** "Rollback limited: / is on plain ext4", or the snapshot a server gets. */
export function rollbackLine(server: Pick<ServerUpgrade, "rollback">): string {
  const { text, note } = rollbackText(server.rollback);
  return note ? `${text} ${note}` : text;
}

/** The checks of the servers of a major upgrade campaign, before it starts (ADR-0182). */
export function MajorUpgradeChecks({ servers }: { servers: ServerUpgrade[] }) {
  const columns: Column<ServerUpgrade>[] = [
    { key: "server", header: t("campaigns.major.server"), cell: (s) => s.hostname, sortValue: (s) => s.hostname },
    {
      key: "ready",
      header: t("campaigns.major.checks"),
      cell: (s) => {
        const failed = s.checks.filter((c) => c.state === "failed").map((c) => checkName(c.name));
        return s.ready ? (
          <Label tone="ok">{t("campaigns.major.ready")}</Label>
        ) : (
          <>
            <Label tone="bad">{t("campaigns.major.notReady")}</Label> {failed.join(", ")}
          </>
        );
      },
      sortValue: (s) => (s.ready ? 1 : 0),
    },
    {
      key: "rollback",
      header: t("campaigns.major.rollback"),
      cell: (s) => <Label tone={s.rollback.available ? "ok" : "warn"}>{rollbackLine(s)}</Label>,
    },
  ];
  return (
    <DataTable
      label={t("campaigns.major.checks")}
      columns={columns}
      rows={servers}
      rowKey={(s) => s.server_id}
      detailInset
      rowDetail={(s) => <CheckList checks={s.checks} />}
    />
  );
}

/** The last upgrade of a server, or its last `leapp preupgrade` alone, as its agent tells it. */
function LastUpgrade({ server }: { server: ServerUpgrade }) {
  const progress = server.progress;
  if (!progress) return null;
  const preupgrade = progress.preupgrade_only === true;
  return (
    <div>
      <strong>{t(preupgrade ? "campaigns.major.lastPreupgrade" : "campaigns.major.last")}</strong>{" "}
      {t("campaigns.update.major", { from: releaseLabel(progress.from), to: releaseLabel(progress.to) })}{" "}
      <Label tone={progress.phase === "done" ? "ok" : progress.phase === "failed" ? "bad" : "info"}>
        {preupgrade && progress.phase === "done"
          ? t("campaigns.major.preupgradeDone")
          : t(`campaigns.major.phase.${progress.phase}`)}
      </Label>
      {progress.error && <p className={styles.note}>{t("campaigns.major.lastFailed", { error: progress.error })}</p>}
      {progress.sources_restored && <p className={styles.note}>{t("campaigns.major.lastRestored")}</p>}
      {progress.disabled_sources.length > 0 && (
        <p className={styles.note}>{t("campaigns.major.disabled", { sources: progress.disabled_sources.join(", ") })}</p>
      )}
      {progress.report && (
        <details className={styles.report} open={preupgrade}>
          <summary>{t("campaigns.major.report")}</summary>
          <pre>{progress.report}</pre>
        </details>
      )}
    </div>
  );
}

/** An upgrade or a `leapp preupgrade` is under way on the server: nothing else starts. */
function running(server: ServerUpgrade): boolean {
  const phase = server.progress?.phase;
  return phase !== undefined && phase !== "done" && phase !== "failed";
}

/** The Major upgrade section of a server's Updates tab (ADR-0182); nothing on a release without a next one. */
export function ServerMajorUpgrade({ server }: { server: ServerDetail }) {
  const fetcher = useCallback((signal: AbortSignal) => api.majorUpgrades({ server_id: server.id }, signal), [server.id]);
  const { data, error, refresh, updatedAt } = usePolledData(fetcher);
  if (!data) {
    return error ? <ErrorState what={t("campaigns.major.title")} error={describeError(error)} onRetry={refresh} /> : null;
  }
  const upgrade = data.servers[0];
  if (!upgrade) return null;
  const now = updatedAt ?? new Date();
  return (
    <section className={admin.section} aria-labelledby="major-upgrade-title">
      <div className={admin.sectionHead}>
        <h2 id="major-upgrade-title" className={admin.sectionTitle}>
          {t("campaigns.major.title")}
        </h2>
        {upgrade.target && upgrade.can_upgrade && upgrade.ready && (
          <ButtonLink href={majorUpgradeHref(server.id)}>
            {t("campaigns.major.upgrade", { release: upgrade.target_label ?? upgrade.target })}
          </ButtonLink>
        )}
      </div>
      <div className={styles.body}>
        <p className={styles.note}>
          {upgrade.target
            ? t("campaigns.major.next", { release: upgrade.target_label ?? upgrade.target })
            : t("campaigns.major.none", { release: releaseLabel(upgrade.release) })}
          {upgrade.reported_at && (
            <>
              {" · "}
              {t("campaigns.major.reported")} <RelativeTime value={upgrade.reported_at} now={now} />
            </>
          )}
        </p>
        {upgrade.target && (
          <>
            <p className={styles.note}>
              {t("campaigns.major.rollbackLine", { rollback: rollbackLine(upgrade) })}
            </p>
            <CheckList checks={upgrade.checks} />
          </>
        )}
        {upgrade.target && upgrade.can_preupgrade && !running(upgrade) && (
          <div>
            <InlineConfirm
              trigger={t("campaigns.major.preupgrade.trigger")}
              triggerVariant="default"
              title={t("campaigns.major.preupgrade.title", { server: server.hostname })}
              consequence={t("campaigns.major.preupgrade.consequence", {
                release: upgrade.target_label ?? upgrade.target,
              })}
              confirmLabel={t("campaigns.major.preupgrade.confirm")}
              confirmVariant="k"
              pendingLabel={t("campaigns.major.preupgrade.sending")}
              onConfirm={async () => {
                await api.leappPreupgrade(server.id);
                refresh();
              }}
              describeError={(e) => t("campaigns.major.preupgrade.failed", { error: describeError(e) })}
            />
          </div>
        )}
        <LastUpgrade server={upgrade} />
      </div>
    </section>
  );
}
