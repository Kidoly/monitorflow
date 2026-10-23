"use client";

import Link from "next/link";
import { useEffect, useState } from "react";

import { type Command, type CommandDetail, api, describeError } from "@/lib/api";
import { commandLog, describeCommand, lastStepAt } from "@/lib/commands";
import { useMe } from "@/lib/me";
import { campaignHref } from "@/lib/routes";
import { scopedHref, useScope } from "@/lib/scope";
import { useUrlParams } from "@/lib/urlState";
import { Button } from "@/components/ui/Button";
import { type Column, DataTable, PrimaryCell } from "@/components/ui/DataTable";
import { FilterBar } from "@/components/ui/FilterBar";
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
  commands,
  now,
  onLoadMore,
  canLoadMore,
}: {
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
        <strong>Could not load the commands:</strong> {commands.error} Retrying automatically.
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
      header: "Command",
      cell: (c) => {
        const { title, sub } = describeCommand(c);
        return <PrimaryCell title={title} sub={sub} />;
      },
    },
    {
      key: "state",
      header: "State",
      cell: (c) => {
        const at = lastStepAt(c);
        return (
          <>
            <StatusLabel kind="run" value={c.state} />{" "}
            {c.state === "expired" ? (
              <span className={styles.muted}>agent did not start it</span>
            ) : (
              at && <span className={styles.muted}>{c.state === "sent" ? "sent " : ""}{formatClock(new Date(at))}</span>
            )}
          </>
        );
      },
    },
    { key: "exit", header: "Exit code", cell: (c) => (c.exit_code === null ? <span className={styles.muted}>—</span> : c.exit_code) },
    { key: "queued", header: "Queued", className: styles.muted, cell: (c) => formatDay(new Date(c.issued_at), now) },
    {
      key: "by",
      header: "By",
      cell: (c) =>
        c.campaign_id ? (
          <Link href={scopedHref(campaignHref(c.campaign_id), scope)}>campaign</Link>
        ) : c.requested_by === null ? (
          "system"
        ) : c.requested_by === me?.id ? (
          me.username
        ) : (
          "another user"
        ),
    },
  ];

  return (
    <div className={styles.tab}>
      <StatTiles
        maxWidth={800}
        tiles={[
          { label: "Running", value: list.filter((c) => c.state === "running" || c.state === "sent").length, tone: "info", href: tileHref("running"), active: state === "running" },
          { label: "Queued", value: list.filter((c) => c.state === "queued").length, tone: "neutral", href: tileHref("queued"), active: state === "queued" },
          { label: "Succeeded, 30 d", value: recent.filter((c) => c.state === "succeeded").length, tone: "ok", href: tileHref("succeeded"), active: state === "succeeded" },
          { label: "Failed, 30 d", value: recent.filter((c) => c.state === "failed").length, tone: "bad", href: tileHref("failed"), active: state === "failed" },
        ]}
      />
      <FilterBar
        filters={[
          {
            name: "state",
            label: "State",
            options: (["queued", "running", "succeeded", "failed", "expired", "cancelled"] as const).map((s) => ({
              value: s,
              label: s[0]!.toUpperCase() + s.slice(1),
            })),
          },
          {
            name: "period",
            label: "Period",
            options: [
              { value: "24h", label: "24 h" },
              { value: "7d", label: "7 d" },
              { value: "30d", label: "30 d" },
            ],
          },
        ]}
        search={{ name: "q", label: "Search commands", placeholder: "Command", width: 200 }}
      />
      {list.length === 0 ? (
        <EmptyState>No commands yet. Updates applied to this server show here with their output.</EmptyState>
      ) : shown.length === 0 ? (
        <EmptyState>No commands match these filters.</EmptyState>
      ) : (
        <DataTable
          label="Commands"
          columns={columns}
          rows={shown}
          rowKey={(c) => c.id}
          dense
          rowMuted={(c) => c.state === "expired" || c.state === "cancelled"}
          rowActions={(c) => (
            <Button size="small" aria-expanded={logs.has(c.id)} onClick={() => toggleLog(c)}>
              {logs.has(c.id) ? "Hide log" : "Log"}
            </Button>
          )}
          rowDetail={(c) => {
            const log = logs.get(c.id);
            if (!log) return null;
            return (
              <pre className={styles.log} aria-label={`Output of ${describeCommand(c).title}`}>
                {"detail" in log
                  ? commandLog(log.detail, formatClock)
                  : "error" in log
                    ? `Could not load the output: ${log.error}`
                    : "Loading the output…"}
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
