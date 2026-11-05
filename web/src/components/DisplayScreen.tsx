"use client";

import Link from "next/link";
import { useSearchParams } from "next/navigation";
import { useCallback, useEffect, useMemo, useRef, useState, useSyncExternalStore } from "react";

import { ApiError, type DashboardData, type DashboardSpec, type ScreenView, type Within, api, isAbortError } from "@/lib/api";
import { DASHBOARD_REFRESH_MS, readyMade, withinOf } from "@/lib/dashboards";
import {
  CONTROLS_MS,
  PROBLEM_TEXT,
  type ScreenProblem,
  VIEW_REFRESH_MS,
  gridRows,
  isStale,
  nextPage,
  staleText,
  textScale,
} from "@/lib/display";
import { dashboardHref, readyMadeHref } from "@/lib/routes";
import { scopedHref, useScope } from "@/lib/scope";
import { usePolling } from "@/lib/usePolling";
import { formatClock } from "@/components/ui/time";

import styles from "./Display.module.css";
import { ScreenPanel } from "./ScreenPanel";

interface Shown {
  id: string;
  name: string;
  spec: DashboardSpec;
  within: Within;
}

interface Screen {
  dashboards: Shown[];
  secondsPerPage: number;
  refreshMs: number;
}

/** A clock that only goes forward, whatever the device's time does. */
const monotonic = () => (typeof performance !== "undefined" ? performance.now() : Date.now());

function fromView(view: ScreenView): Screen {
  return {
    dashboards: view.dashboards.map((d) => ({ ...d, within: { kind: "all" } })),
    secondsPerPage: view.seconds_per_page,
    refreshMs: view.refresh_seconds * 1000,
  };
}

function subscribeHash(listener: () => void): () => void {
  window.addEventListener("hashchange", listener);
  return () => window.removeEventListener("hashchange", listener);
}

const readHash = (): string | null => window.location.hash.slice(1) || null;

const statusOf = (e: unknown) => (e instanceof ApiError ? e.status : null);

/**
 * `/display/` (ADR-0099): dashboards full screen, for a wall. `?id=` or
 * `?ready=` shows one with the user's session; `#<secret>` shows a display
 * link's dashboards in turn, without signing in. No navigation, no edit
 * controls; the clock and the time of the data in a corner, and a banner
 * when the data is more than two refresh periods old.
 */
export function DisplayScreen() {
  const params = useSearchParams();
  const scope = useScope();
  const id = params.get("id");
  const ready = params.get("ready");
  const theme = params.get("theme") === "light" ? "light" : "dark";

  // The secret is read after hydration: the static page has no fragment.
  const secret = useSyncExternalStore(subscribeHash, readHash, () => undefined);
  const link = secret ? secret : null;

  // Dark unless asked otherwise, for this page only.
  useEffect(() => {
    const root = document.documentElement;
    const previous = root.getAttribute("data-theme");
    root.setAttribute("data-theme", theme);
    return () => {
      if (previous === null) root.removeAttribute("data-theme");
      else root.setAttribute("data-theme", previous);
    };
  }, [theme]);

  const [screen, setScreen] = useState<Screen | null>(null);
  const [problem, setProblem] = useState<ScreenProblem | null>(null);
  const [unreachable, setUnreachable] = useState(false);
  const [answers, setAnswers] = useState<Record<string, DashboardData>>({});
  const [lastAnswer, setLastAnswer] = useState<number | null>(null);
  const [dataTime, setDataTime] = useState<Date | null>(null);
  const [offset, setOffset] = useState(0);
  const [page, setPage] = useState(0);
  const [now, setNow] = useState(() => ({ wall: Date.now(), mono: monotonic() }));
  const [controls, setControls] = useState(false);
  const started = useRef(false);

  const fail = useCallback((e: unknown) => {
    const status = statusOf(e);
    if (status === 410) {
      started.current = false;
      setProblem("gone");
    } else if (status === 403) setProblem("nothing");
    else if (status === 404 && !link) setProblem("missing");
    else setUnreachable(true);
  }, [link]);

  const restart = useCallback(
    async (signal: AbortSignal) => {
      const view = await api.startDisplay(link ?? "", signal);
      started.current = true;
      setScreen(fromView(view));
      return view;
    },
    [link],
  );

  const loadView = useCallback(
    async (signal: AbortSignal) => {
      try {
        if (link) {
          if (!started.current) await restart(signal);
          else {
            try {
              setScreen(fromView(await api.display(signal)));
            } catch (e) {
              if (statusOf(e) !== 401) throw e;
              await restart(signal);
            }
          }
        } else if (ready) {
          const made = readyMade(ready);
          if (!made) throw new ApiError(404, "not found");
          const within: Within = scope.customerId ? { kind: "customer", id: scope.customerId } : { kind: "all" };
          setScreen({ dashboards: [{ id: made.id, name: made.name, spec: made.spec, within }], secondsPerPage: 60, refreshMs: DASHBOARD_REFRESH_MS });
        } else if (id) {
          const d = await api.dashboard(id, signal);
          setScreen({
            dashboards: [{ id: d.id, name: d.name, spec: d.spec, within: withinOf(d.shared_with) }],
            secondsPerPage: 60,
            refreshMs: DASHBOARD_REFRESH_MS,
          });
        } else {
          throw new ApiError(404, "not found");
        }
        setProblem(null);
        setUnreachable(false);
      } catch (e) {
        if (signal.aborted || isAbortError(e)) return;
        fail(e);
      }
    },
    [link, ready, id, scope.customerId, restart, fail],
  );
  const viewEvery = screen && problem === null ? VIEW_REFRESH_MS : problem === "nothing" ? VIEW_REFRESH_MS : DASHBOARD_REFRESH_MS;
  usePolling(loadView, viewEvery, secret !== undefined && problem !== "gone" && problem !== "missing");

  const count = screen?.dashboards.length ?? 0;
  const current = screen && count > 0 ? screen.dashboards[page % count] : undefined;
  const currentKey = current ? JSON.stringify(current) : "";

  const loadData = useCallback(
    async (signal: AbortSignal) => {
      if (!currentKey) return;
      const shown = JSON.parse(currentKey) as Shown;
      const fetchData = () =>
        link
          ? api.displayData(shown.id, signal)
          : api.dashboardData({ range: shown.spec.range, within: shown.within, panels: shown.spec.panels }, signal);
      try {
        let data: DashboardData;
        try {
          data = await fetchData();
        } catch (e) {
          // The screen session ended: open the link again, once.
          if (!link || statusOf(e) !== 401) throw e;
          await restart(signal);
          data = await fetchData();
        }
        setAnswers((a) => ({ ...a, [shown.id]: data }));
        setLastAnswer(monotonic());
        setDataTime(new Date(data.now * 1000));
        setOffset(data.now * 1000 - Date.now());
        setUnreachable(false);
      } catch (e) {
        if (signal.aborted || isAbortError(e)) return;
        fail(e);
      }
    },
    [currentKey, link, restart, fail],
  );
  usePolling(loadData, screen?.refreshMs ?? DASHBOARD_REFRESH_MS, Boolean(currentKey) && problem === null);

  // Dashboards in turn.
  const secondsPerPage = screen?.secondsPerPage ?? 60;
  useEffect(() => {
    if (count < 2) return;
    const timer = setInterval(() => setPage((p) => nextPage(p, count)), secondsPerPage * 1000);
    return () => clearInterval(timer);
  }, [count, secondsPerPage]);

  // The clock, and the age of the data.
  useEffect(() => {
    const timer = setInterval(() => setNow({ wall: Date.now(), mono: monotonic() }), 1000);
    return () => clearInterval(timer);
  }, []);

  // The buttons show for a moment when the mouse moves.
  const hide = useRef<ReturnType<typeof setTimeout> | undefined>(undefined);
  const showControls = useCallback(() => {
    setControls(true);
    clearTimeout(hide.current);
    hide.current = setTimeout(() => setControls(false), CONTROLS_MS);
  }, []);
  useEffect(() => () => clearTimeout(hide.current), []);

  const panels = useMemo(() => current?.spec.panels ?? [], [current]);
  const rows = gridRows(panels);
  const stale = isStale(lastAnswer, now.mono, screen?.refreshMs ?? DASHBOARD_REFRESH_MS);
  const data = current ? answers[current.id] : undefined;
  const byId = new Map(data?.panels.map((p) => [p.id, p]));
  const clock = formatClock(new Date(now.wall + offset)).slice(0, 5);
  const root = typeof document !== "undefined" ? (document.documentElement as HTMLElement & { webkitRequestFullscreen?: () => void }) : null;
  const canFullScreen = root !== null && (typeof root.requestFullscreen === "function" || typeof root.webkitRequestFullscreen === "function");

  function fullScreen() {
    if (!root) return;
    if (typeof root.requestFullscreen === "function") void root.requestFullscreen().catch(() => undefined);
    else root.webkitRequestFullscreen?.();
  }

  const closeHref = !link && (id || ready) ? scopedHref(id ? dashboardHref(id) : readyMadeHref(ready ?? ""), scope) : null;
  const message =
    problem !== null
      ? PROBLEM_TEXT[problem]
      : !screen && unreachable
        ? { title: "Cannot reach MonitorFlow", text: "This screen keeps trying by itself." }
        : null;

  return (
    <div className={styles.screen} onMouseMove={showControls} onTouchStart={showControls}>
      <header className={styles.head}>
        <div className={styles.heading}>
          <h1>{message ? "MonitorFlow" : (current?.name ?? "")}</h1>
          {count > 1 && !message && (
            <span className={styles.pageOf}>
              {(page % count) + 1} of {count}
            </span>
          )}
        </div>
        <div className={styles.clock}>
          <b>{clock}</b>
          {dataTime && !message && <span>Updated {formatClock(dataTime)}</span>}
        </div>
      </header>
      {message ? (
        <main className={styles.message} role="alert">
          <h2>{message.title}</h2>
          <p>{message.text}</p>
        </main>
      ) : (
        <>
          {stale && dataTime && (
            <p className={styles.stale} role="alert">
              {staleText(dataTime)}
            </p>
          )}
          <main
            className={styles.grid}
            data-stale={stale || undefined}
            style={{ gridTemplateRows: `repeat(${rows}, minmax(0, 1fr))`, fontSize: `${textScale(rows)}em` }}
          >
            {panels.map((panel) => (
              <ScreenPanel key={panel.id} panel={panel} data={byId.get(panel.id)} window={data} />
            ))}
          </main>
        </>
      )}
      {controls && (canFullScreen || closeHref) && (
        <div className={styles.controls}>
          {canFullScreen && (
            <button type="button" onClick={fullScreen}>
              Full screen
            </button>
          )}
          {closeHref && <Link href={closeHref}>Close</Link>}
        </div>
      )}
    </div>
  );
}
