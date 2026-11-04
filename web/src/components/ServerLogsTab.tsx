"use client";

import { useCallback, useState } from "react";

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
      <EmptyState title="Windows event logs aren't forwarded yet">
        Linux servers send the warnings and errors of their system journal here. The Event Log checks of{" "}
        {server.hostname} still count the events they watch.
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
      header: "Time",
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
      header: "Message",
      cell: (line) => (
        <>
          <span className={styles.logSource}>{lineSource(line) || "no unit"}</span>
          <span className={styles.logMessage}>{line.message}</span>
        </>
      ),
    },
  ];

  return (
    <div className={styles.tab}>
      <FilterBar
        label="Log filters"
        search={{ name: "q", label: "Search the messages", placeholder: "Search the messages", width: 260 }}
        filters={[
          {
            name: "level",
            label: "Level",
            options: [{ value: "error", label: "errors and worse" }],
          },
          { name: "unit", label: "Unit", options: unitOptions },
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
        <Banner tone="warn">Could not refresh the logs: {describeError(error)} Retrying automatically.</Banner>
      )}
      {!data ? (
        error !== null ? (
          <ErrorState what="the logs" error={describeError(error)} onRetry={refresh} />
        ) : (
          <LoadingBlock label="Loading logs" />
        )
      ) : data.lines.length === 0 ? (
        filtered ? (
          <NoMatches names={FILTER_KEYS}>No line in this time matches the filters.</NoMatches>
        ) : (
          <EmptyState title="No warnings or errors in this time">
            {server.hostname} forwards the warnings and errors of its system journal. Quiet is good news.
          </EmptyState>
        )
      ) : (
        <>
          <DataTable
            label={`Logs of ${server.hostname}`}
            columns={columns}
            rows={data.lines}
            rowKey={(line) => `${line.time} ${line.pid ?? ""} ${line.message}`}
            pageSize={MAX_LOG_LINES}
            dense
          />
          {data.more &&
            (limit < MAX_LOG_LINES ? (
              <div>
                <Button onClick={() => setLimit((n) => Math.min(MAX_LOG_LINES, n + LOG_PAGE))}>Show older</Button>
              </div>
            ) : (
              <p className={styles.note}>
                Showing the newest {MAX_LOG_LINES} lines. Narrow the filters to see older ones.
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
        ? `From ${formatExact(start)} to now, refreshed every 15 seconds.`
        : `From ${formatExact(start)} to ${formatExact(end)}.`}
      {(chosen || !live) && (
        <>
          {" "}
          <button type="button" className={styles.linkButton} onClick={onLastHour}>
            Show the last hour
          </button>
        </>
      )}
    </p>
  );
}
