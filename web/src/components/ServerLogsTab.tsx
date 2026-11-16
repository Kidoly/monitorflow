"use client";

import { useCallback, useState } from "react";

import { t } from "@/i18n";
import { type LogLine, type ServerDetail, api, describeError } from "@/lib/api";
import { LOG_PAGE, MAX_LOG_LINES, lineSource, logWindow, priorityTone, priorityWord } from "@/lib/logs";
import { usePolledData } from "@/lib/usePolledData";
import { useUrlParams } from "@/lib/urlState";
import { Button } from "@/components/ui/Button";
import { type Column, DataTable } from "@/components/ui/DataTable";
import { FilterBar, NoMatches } from "@/components/ui/FilterBar";
import { Banner, EmptyState, ErrorState, LoadingBlock } from "@/components/ui/PageState";
import { Label } from "@/components/ui/StatusLabel";
import { formatExact } from "@/components/ui/time";

import styles from "./ServerPage.module.css";

/** Query parameters of the filters; `from` and `to` set the time. */
const FILTER_KEYS = ["level", "unit", "q"];
/** Refreshes of a window that ends at a fixed time: none worth having. */
const FIXED_MS = 24 * 60 * 60 * 1000;

/** The Logs tab: the server's forwarded journal lines, newest first (ADR-0091). */
export function ServerLogsTab({ server }: { server: ServerDetail }) {
  if (server.os_family === "windows") {
    return (
      <EmptyState title={t("server.logs.windows")}>
        {t("server.logs.windowsText", { server: server.hostname })}
      </EmptyState>
    );
  }
  return <Logs server={server} />;
}

function Logs({ server }: { server: ServerDetail }) {
  const [params, setParams] = useUrlParams();
  const from = params.get("from");
  const to = params.get("to");
  const level = params.get("level") === "error" ? "error" : undefined;
  const unit = params.get("unit") || undefined;
  const q = params.get("q") || undefined;
  const [limit, setLimit] = useState(LOG_PAGE);
  const live = to === null;

  const fetcher = useCallback(
    (signal: AbortSignal) => {
      const window = logWindow(from, to, new Date());
      return api
        .serverLogs(server.id, { start: window.start, end: window.end, level, unit, q, limit }, signal)
        .then((list) => ({ ...list, window }));
    },
    [server.id, from, to, level, unit, q, limit],
  );
  const { data, error, refresh } = usePolledData(fetcher, { intervalMs: live ? undefined : FIXED_MS });

  const units = data?.units ?? [];
  // The unit asked for stays offered while no line of it is in the range.
  const unitOptions = [...new Set([...units, ...(unit ? [unit] : [])])].sort().map((u) => ({ value: u, label: u }));
  const filtered = FILTER_KEYS.some((key) => params.get(key));

  // Two columns, so messages wrap on a phone rather than scroll away.
  const columns: Column<LogLine>[] = [
    {
      key: "time",
      header: t("server.logs.column.time"),
      className: styles.logTime,
      cell: (line) => (
        <>
          <time dateTime={line.time}>{formatExact(new Date(line.time))}</time>
          <Label small tone={priorityTone(line.priority)}>
            {priorityWord(line.priority)}
          </Label>
        </>
      ),
    },
    {
      key: "message",
      header: t("server.logs.column.message"),
      cell: (line) => (
        <>
          <span className={styles.logSource}>{lineSource(line) || t("server.logs.noUnit")}</span>
          <span className={styles.logMessage}>{line.message}</span>
        </>
      ),
    },
  ];

  return (
    <div className={styles.tab}>
      <FilterBar
        label={t("server.logs.filters")}
        search={{ name: "q", label: t("server.logs.search"), placeholder: t("server.logs.search"), width: 260 }}
        filters={[
          {
            name: "level",
            label: t("server.logs.level"),
            options: [{ value: "error", label: t("server.logs.errors") }],
          },
          { name: "unit", label: t("server.logs.unit"), options: unitOptions },
        ]}
        onChange={() => setLimit(LOG_PAGE)}
      />
      {data && (
        <WindowLine
          start={data.window.start}
          end={data.window.end}
          live={live}
          chosen={from !== null}
          onLastHour={() => setParams({ from: null, to: null })}
        />
      )}
      {error !== null && data && (
        <Banner tone="warn">{t("server.logs.refreshFailed", { error: describeError(error) })}</Banner>
      )}
      {!data ? (
        error !== null ? (
          <ErrorState what={t("server.logs.what")} error={describeError(error)} onRetry={refresh} />
        ) : (
          <LoadingBlock label={t("server.logs.loading")} />
        )
      ) : data.lines.length === 0 ? (
        filtered ? (
          <NoMatches names={FILTER_KEYS}>{t("server.logs.noMatch")}</NoMatches>
        ) : (
          <EmptyState title={t("server.logs.quiet")}>
            {t("server.logs.quietText", { server: server.hostname })}
          </EmptyState>
        )
      ) : (
        <>
          <DataTable
            label={t("server.logs.table", { server: server.hostname })}
            columns={columns}
            rows={data.lines}
            rowKey={(line) => `${line.time} ${line.pid ?? ""} ${line.message}`}
            pageSize={MAX_LOG_LINES}
            dense
          />
          {data.more &&
            (limit < MAX_LOG_LINES ? (
              <div>
                <Button onClick={() => setLimit((n) => Math.min(MAX_LOG_LINES, n + LOG_PAGE))}>{t("server.logs.older")}</Button>
              </div>
            ) : (
              <p className={styles.note}>
                {t("server.logs.newest", { count: MAX_LOG_LINES })}
              </p>
            ))}
        </>
      )}
    </div>
  );
}

/** "From 10:02 to now, refreshed every 15 seconds", with a way back to the last hour from a chosen time. */
function WindowLine({
  start,
  end,
  live,
  chosen,
  onLastHour,
}: {
  start: Date;
  end: Date;
  live: boolean;
  /** The time came from the URL, like an incident's link. */
  chosen: boolean;
  onLastHour: () => void;
}) {
  return (
    <p className={styles.note}>
      {live
        ? t("server.logs.live", { start: formatExact(start) })
        : t("server.logs.fixed", { start: formatExact(start), end: formatExact(end) })}
      {(chosen || !live) && (
        <>
          {" "}
          <button type="button" className={styles.linkButton} onClick={onLastHour}>
            {t("server.logs.lastHour")}
          </button>
        </>
      )}
    </p>
  );
}
