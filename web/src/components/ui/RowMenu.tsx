"use client";

import Link from "next/link";
import { type CSSProperties, type KeyboardEvent, type ReactNode, useEffect, useId, useRef, useState } from "react";

import { buttonClass } from "./Button";
import { cx } from "./cx";
import { floatingAt } from "./floating";
import styles from "./RowMenu.module.css";

export type RowMenuItem =
  | { label: string; href: string; danger?: boolean }
  | { label: string; onSelect: () => void; danger?: boolean };

/**
 * The `⋯` button of a table row and its menu. It opens on click, Enter,
 * Space or ↓, focuses the first item, moves with ↑ ↓ Home End, and closes on
 * Escape (focus back on the button), Tab, a choice or a click elsewhere.
 * `label` names the row: "Actions for Disk used /var". With `trigger`, the
 * button is a small labelled one ("Silence ▾") instead of `⋯`.
 */
export function RowMenu({
  label,
  items,
  trigger,
  disabled,
}: {
  label: string;
  items: RowMenuItem[];
  trigger?: ReactNode;
  disabled?: boolean;
}) {
  const [open, setOpen] = useState(false);
  const [at, setAt] = useState<CSSProperties>();
  const buttonRef = useRef<HTMLButtonElement>(null);
  const menuRef = useRef<HTMLDivElement>(null);
  const id = useId();

  const itemsOf = () => [...(menuRef.current?.querySelectorAll<HTMLElement>('[role="menuitem"]') ?? [])];

  useEffect(() => {
    if (!open) return;
    itemsOf()[0]?.focus();
    function away(event: MouseEvent) {
      if (!menuRef.current?.contains(event.target as Node) && !buttonRef.current?.contains(event.target as Node)) {
        setOpen(false);
      }
    }
    // Fixed in the window: scrolling would leave it behind, so it closes.
    function scrolled(event: Event) {
      if (!menuRef.current?.contains(event.target as Node)) setOpen(false);
    }
    document.addEventListener("mousedown", away);
    window.addEventListener("scroll", scrolled, true);
    window.addEventListener("resize", scrolled);
    return () => {
      document.removeEventListener("mousedown", away);
      window.removeEventListener("scroll", scrolled, true);
      window.removeEventListener("resize", scrolled);
    };
  }, [open]);

  function show() {
    if (buttonRef.current) setAt(floatingAt(buttonRef.current.getBoundingClientRect(), 4, "right"));
    setOpen(true);
  }

  function close(refocus: boolean) {
    setOpen(false);
    if (refocus) buttonRef.current?.focus();
  }

  function onMenuKey(event: KeyboardEvent) {
    const all = itemsOf();
    const at = all.indexOf(document.activeElement as HTMLElement);
    const go = (index: number) => {
      event.preventDefault();
      all[(index + all.length) % all.length]?.focus();
    };
    if (event.key === "ArrowDown") go(at + 1);
    else if (event.key === "ArrowUp") go(at - 1);
    else if (event.key === "Home") go(0);
    else if (event.key === "End") go(all.length - 1);
    else if (event.key === "Escape") {
      event.preventDefault();
      event.stopPropagation();
      close(true);
    } else if (event.key === "Tab") close(false);
  }

  return (
    <span className={styles.wrap}>
      <button
        ref={buttonRef}
        type="button"
        className={trigger ? buttonClass({ size: "small" }) : styles.button}
        aria-label={label}
        disabled={disabled}
        aria-haspopup="menu"
        aria-expanded={open}
        aria-controls={open ? id : undefined}
        onClick={() => (open ? setOpen(false) : show())}
        onKeyDown={(event) => {
          if (event.key === "ArrowDown" && !open) {
            event.preventDefault();
            show();
          }
        }}
      >
        {trigger ? (
          <>
            {trigger} <span aria-hidden="true">▾</span>
          </>
        ) : (
          <span aria-hidden="true">⋯</span>
        )}
      </button>
      {open && (
        <div ref={menuRef} id={id} role="menu" aria-label={label} className={styles.menu} style={at} onKeyDown={onMenuKey}>
          {items.map((item) =>
            "href" in item ? (
              <Link
                key={item.label}
                href={item.href}
                role="menuitem"
                tabIndex={-1}
                className={cx(styles.item, item.danger && styles.danger)}
                onClick={() => setOpen(false)}
              >
                {item.label}
              </Link>
            ) : (
              <button
                key={item.label}
                type="button"
                role="menuitem"
                tabIndex={-1}
                className={cx(styles.item, item.danger && styles.danger)}
                onClick={() => {
                  setOpen(false);
                  item.onSelect();
                }}
              >
                {item.label}
              </button>
            ),
          )}
        </div>
      )}
    </span>
  );
}
