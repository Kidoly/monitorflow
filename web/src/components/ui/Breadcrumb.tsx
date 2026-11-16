"use client";

import Link from "next/link";
import { Fragment, type KeyboardEvent, useEffect, useId, useRef, useState } from "react";

import { t } from "@/i18n";

import styles from "./Breadcrumb.module.css";
import { SkeletonLines } from "./PageState";

/** A page next to the current one: another server of the site, another sensor of the server. */
export interface Sibling {
  label: string;
  href: string;
  /** The page shown now, marked and not a link. */
  current?: boolean;
}

/** Where a crumb's ▾ list comes from: given, or loaded when first opened. */
export interface Siblings {
  /** The button's accessible name: "Other servers of paris". */
  label: string;
  load: (signal: AbortSignal) => Promise<Sibling[]>;
}

export interface Crumb {
  label: string;
  href: string;
  siblings?: Siblings;
}

/**
 * "Fleet › web-01 ▾ › ▾": links to the parents. The page's own name is
 * left out when the title right below says it; pass `current` only for
 * something the title does not show (an incident's short id), and it
 * carries `aria-current="page"`. A crumb with `siblings` gets a ▾ that
 * lists them, to jump to one without going back; the current page's
 * siblings get a ▾ of their own at the end.
 */
export function Breadcrumb({
  parents,
  current,
  siblings,
}: {
  parents: Crumb[];
  current?: string;
  /** The current page's siblings. */
  siblings?: Siblings;
}) {
  const last = current !== undefined || siblings !== undefined;
  return (
    <nav className={styles.crumb} aria-label={t("ui.breadcrumb.label")}>
      {parents.map((parent, i) => (
        <Fragment key={parent.href}>
          <span className={styles.item}>
            <Link href={parent.href}>{parent.label}</Link>
            {parent.siblings && <SiblingMenu siblings={parent.siblings} />}
          </span>
          {(last || i < parents.length - 1) && <span aria-hidden="true">›</span>}
        </Fragment>
      ))}
      {last && (
        <span className={styles.item}>
          {current !== undefined && (
            <span className={styles.current} aria-current="page">
              {current}
            </span>
          )}
          {siblings && <SiblingMenu siblings={siblings} />}
        </span>
      )}
    </nav>
  );
}

/** Lists with more entries than this get a filter box. */
const FILTER_FROM = 8;

/**
 * A disclosure, not a modal: it never blocks the page. Escape, Tab out of
 * it or a click elsewhere closes it, and the click still lands; ↑ ↓ move
 * between its links.
 */
function SiblingMenu({ siblings }: { siblings: Siblings }) {
  const [open, setOpen] = useState(false);
  const [list, setList] = useState<Sibling[] | "error" | null>(null);
  const [filter, setFilter] = useState("");
  const id = useId();
  const root = useRef<HTMLSpanElement>(null);
  const button = useRef<HTMLButtonElement>(null);
  const panel = useRef<HTMLDivElement>(null);
  // The page re-renders on every refresh; a load in flight keeps going.
  const load = useRef(siblings.load);
  useEffect(() => {
    load.current = siblings.load;
  }, [siblings.load]);

  useEffect(() => {
    if (!open || list !== null) return;
    const controller = new AbortController();
    load
      .current(controller.signal)
      .then(setList)
      .catch((e: unknown) => {
        if (!controller.signal.aborted) {
          console.warn("could not load the breadcrumb's list", e);
          setList("error");
        }
      });
    return () => controller.abort();
  }, [open, list]);

  useEffect(() => {
    if (!open) return;
    const outside = (event: PointerEvent) => {
      if (!root.current?.contains(event.target as Node)) setOpen(false);
    };
    document.addEventListener("pointerdown", outside);
    return () => document.removeEventListener("pointerdown", outside);
  }, [open]);

  // Focus the filter, else the first entry, once the list is there.
  useEffect(() => {
    if (open && Array.isArray(list)) panel.current?.querySelector<HTMLElement>("input, a")?.focus();
  }, [open, list]);

  const close = () => {
    setOpen(false);
    button.current?.focus();
  };

  const onKeyDown = (event: KeyboardEvent) => {
    if (event.key === "Escape") {
      event.preventDefault();
      close();
      return;
    }
    if (event.key !== "ArrowDown" && event.key !== "ArrowUp") return;
    const items = [...(panel.current?.querySelectorAll<HTMLElement>("input, a") ?? [])];
    const at = items.indexOf(document.activeElement as HTMLElement);
    const next = items[(at + (event.key === "ArrowDown" ? 1 : -1) + items.length) % items.length];
    if (next) {
      event.preventDefault();
      next.focus();
    }
  };

  const shown = Array.isArray(list) ? list.filter((s) => s.label.toLowerCase().includes(filter.trim().toLowerCase())) : [];

  return (
    <span
      ref={root}
      className={styles.menu}
      onKeyDown={open ? onKeyDown : undefined}
      onBlur={(event) => {
        if (open && !root.current?.contains(event.relatedTarget as Node | null) && event.relatedTarget !== null) setOpen(false);
      }}
    >
      <button
        ref={button}
        type="button"
        className={styles.toggle}
        aria-label={siblings.label}
        aria-expanded={open}
        aria-controls={id}
        onClick={() => setOpen(!open)}
      >
        ▾
      </button>
      {open && (
        <div ref={panel} id={id} className={styles.panel}>
          {list === null ? (
            <SkeletonLines lines={3} />
          ) : list === "error" ? (
            <p className={styles.note} role="status">
              {t("ui.breadcrumb.loadFailed")}
            </p>
          ) : (
            <>
              {list.length > FILTER_FROM && (
                <input
                  type="search"
                  className={styles.filter}
                  aria-label={t("ui.breadcrumb.filter", { label: siblings.label })}
                  placeholder={t("ui.breadcrumb.filterPlaceholder")}
                  value={filter}
                  onChange={(e) => setFilter(e.target.value)}
                />
              )}
              <ul aria-label={siblings.label}>
                {shown.map((s) => (
                  <li key={s.href}>
                    {s.current ? (
                      <span className={styles.here} aria-current="page">
                        {s.label}
                      </span>
                    ) : (
                      <Link href={s.href} onClick={() => setOpen(false)}>
                        {s.label}
                      </Link>
                    )}
                  </li>
                ))}
              </ul>
              {shown.length === 0 && <p className={styles.note}>{t("ui.breadcrumb.noMatch")}</p>}
            </>
          )}
        </div>
      )}
    </span>
  );
}
