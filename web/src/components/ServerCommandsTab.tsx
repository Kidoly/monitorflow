"use client";

import Link from "next/link";
import { useEffect, useState } from "react";

import { t } from "@/i18n";
import { type Command, type CommandDetail, api, describeError } from "@/lib/api";
import { commandLog, describeCommand, lastStepAt } from "@/lib/commands";
import { useMe } from "@/lib/me";
import { campaignHref, serverHref } from "@/lib/routes";
import { scopedHref, useScope } from "@/lib/scope";
import { useUrlParams } from "@/lib/urlState";
import { Button, ButtonLink } from "@/components/ui/Button";
import { type Column, DataTable, PrimaryCell } from "@/components/ui/DataTable";
import { FilterBar, NoMatches } from "@/components/ui/FilterBar";
import { Banner, EmptyState } from "@/components/ui/PageState";
import { StatusLabel } from "@/components/ui/StatusLabel";
import { StatTiles } from "@/components/ui/StatTiles";
import { formatClock, formatDay } from "@/components/ui/time";

import styles from "./ServerPage.module.css";

const DAY = 86_400_000;
const PERIODS: Record<string, number> = { "24h": DAY, "7d": 7 * DAY, "30d": 30 * DAY };

type Log = { state: Command["state"] } & ({ detail: CommandDetail } | { error: string } | { loading: true });

/** "running" covers commands sent and not started yet. */
function matchesState(command: Command, state: string | null): boolean {
  if (!state) return true;
  return state === "running" ? command.state === "running" || command.state === "sent" : command.state === state;
}

/** The Commands tab: counters, filters and the server's commands, with their output under the row. */
export function ServerCommandsTab({
  serverId,
  commands,
  now,
  onLoadMore,
  canLoadMore,
}: {
  serverId: string;
  commands: { commands: Command[]; nextCursor: string | null } | { error: string };
  now: Date;
  onLoadMore: () => void;
  canLoadMore: boolean;
}) {
  const me = useMe();
  const scope = useScope();
  const [params] = useUrlParams();
  const [logs, setLogs] = useState<ReadonlyMap<string, Log>>(new Map());
  const list = "commands" in commands ? commands.commands : [];

  // An open log follows its command: a new state fetches the output again.
  useEffect(() => {
    for (const command of list) {
      const log = logs.get(command.id);
      if (log && !("loading" in log) && log.state !== command.state) void fetchLog(command);
    }
  });

  async function fetchLog(command: Command) {
    setLogs((m) => new Map(m).set(command.id, { state: command.state, loading: true }));
    let log: Log;
    try {
      log = { state: command.state, detail: await api.command(command.id) };
    } catch (e) {
      log = { state: command.state, error: describeError(e) };
    }
    setLogs((m) => (m.has(command.id) ? new Map(m).set(command.id, log) : m));
  }

  function toggleLog(command: Command) {
    if (logs.has(command.id)) {
      setLogs((m) => {
        const next = new Map(m);
        next.delete(command.id);
        return next;
      });
    } else {
      void fetchLog(command);
    }
  }

  if ("error" in commands) {
    return (
      <Banner tone="bad">
        <strong>{t("server.commands.loadFailed")}</strong> {t("server.commands.retrying", { error: commands.error })}
      </Banner>
    );
  }

  const recent = list.filter((c) => now.getTime() - Date.parse(c.issued_at) <= PERIODS["30d"]!);
  const state = params.get("state");
  const period = PERIODS[params.get("period") ?? ""];
  const query = params.get("q")?.trim().toLowerCase() ?? "";
  const shown = list.filter(
    (c) =>
      matchesState(c, state) &&
      (!period || now.getTime() - Date.parse(c.issued_at) <= period) &&
      (!query || describeCommand(c).title.toLowerCase().includes(query)),
  );
  const tileHref = (value: string) => {
    const next = new URLSearchParams(params);
    if (state === value) next.delete("state");
    else next.set("state", value);
    return `/servers/view/?${next}`;
  };

  const columns: Column<Command>[] = [
    {
      key: "command",
      header: t("server.commands.column.command"),
      cell: (c) => {
        const { title, sub } = describeCommand(c);
        return <PrimaryCell title={title} sub={sub} />;
      },
    },
    {
      key: "state",
      header: t("server.commands.column.state"),
      cell: (c) => {
        const at = lastStepAt(c);
        return (
          <>
            <StatusLabel kind="run" value={c.state} />{" "}
            {c.state === "expired" ? (
              <span className={styles.muted}>{t("server.commands.notStarted")}</span>
            ) : (
              at && (
                <span className={styles.muted}>
                  {c.state === "sent" ? t("server.commands.sent", { at: formatClock(new Date(at)) }) : formatClock(new Date(at))}
                </span>
              )
            )}
          </>
        );
      },
    },
    { key: "exit", header: t("server.commands.column.exit"), cell: (c) => c.exit_code },
    { key: "queued", header: t("server.commands.column.queued"), className: styles.muted, cell: (c) => formatDay(new Date(c.issued_at), now) },
    {
      key: "by",
      header: t("server.commands.column.by"),
      cell: (c) =>
        c.campaign_id ? (
          <Link href={scopedHref(campaignHref(c.campaign_id), scope)}>{t("server.commands.campaign")}</Link>
        ) : c.requested_by === null ? (
          t("server.commands.system")
        ) : c.requested_by === me?.id ? (
          me.username
        ) : (
          t("server.commands.anotherUser")
        ),
    },
  ];

  return (
    <div className={styles.tab}>
      <StatTiles
        maxWidth={800}
        tiles={[
          { label: t("server.commands.tile.running"), value: list.filter((c) => c.state === "running" || c.state === "sent").length, tone: "info", href: tileHref("running"), active: state === "running" },
          { label: t("server.commands.tile.queued"), value: list.filter((c) => c.state === "queued").length, tone: "neutral", href: tileHref("queued"), active: state === "queued" },
          { label: t("server.commands.tile.succeeded"), value: recent.filter((c) => c.state === "succeeded").length, tone: "ok", href: tileHref("succeeded"), active: state === "succeeded" },
          { label: t("server.commands.tile.failed"), value: recent.filter((c) => c.state === "failed").length, tone: "bad", href: tileHref("failed"), active: state === "failed" },
        ]}
      />
      <FilterBar
        filters={[
          {
            name: "state",
            label: t("server.commands.filter.state"),
            options: (["queued", "running", "succeeded", "failed", "expired", "cancelled"] as const).map((s) => ({
              value: s,
              label: t(`server.commands.state.${s}`),
            })),
          },
          {
            name: "period",
            label: t("server.commands.filter.period"),
            options: (["24h", "7d", "30d"] as const).map((value) => ({ value, label: t(`server.commands.period.${value}`) })),
          },
        ]}
        search={{
          name: "q",
          label: t("server.commands.search"),
          placeholder: t("server.commands.searchPlaceholder"),
          width: 200,
        }}
      />
      {list.length === 0 ? (
        <EmptyState actions={<ButtonLink href={scopedHref(serverHref(serverId, "updates"), scope)}>{t("server.commands.openUpdates")}</ButtonLink>}>
          {t("server.commands.none")}
        </EmptyState>
      ) : shown.length === 0 ? (
        <NoMatches names={["state", "period", "q"]}>{t("server.commands.noMatch")}</NoMatches>
      ) : (
        <DataTable
          label={t("server.commands.table")}
          columns={columns}
          rows={shown}
          rowKey={(c) => c.id}
          dense
          rowMuted={(c) => c.state === "expired" || c.state === "cancelled"}
          rowActions={(c) => (
            <Button size="small" aria-expanded={logs.has(c.id)} onClick={() => toggleLog(c)}>
              {logs.has(c.id) ? t("server.commands.hideLog") : t("server.commands.log")}
            </Button>
          )}
          rowDetail={(c) => {
            const log = logs.get(c.id);
            if (!log) return null;
            return (
              <pre className={styles.log} aria-label={t("server.commands.output", { title: describeCommand(c).title })}>
                {"detail" in log
                  ? commandLog(log.detail, formatClock)
                  : "error" in log
                    ? t("server.commands.outputFailed", { error: log.error })
                    : t("server.commands.outputLoading")}
              </pre>
            );
          }}
          hasMore={commands.nextCursor !== null && canLoadMore}
          onLoadMore={onLoadMore}
        />
      )}
    </div>
  );
}
