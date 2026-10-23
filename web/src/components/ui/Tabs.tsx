"use client";

import { type KeyboardEvent, type ReactNode, useRef } from "react";

import styles from "./Tabs.module.css";

export interface Tab {
  id: string;
  label: string;
  /** After the label: a count or a small label ("Reboot required"). */
  extra?: ReactNode;
}

/** The ID of the panel of tab `id` in the tablist `idBase`, for `aria-controls`. */
export const tabPanelId = (idBase: string, id: string) => `${idBase}-panel-${id}`;
const tabId = (idBase: string, id: string) => `${idBase}-tab-${id}`;

/**
 * A `role="tablist"` of buttons with roving focus: ← → Home End move
 * between tabs and select them. Controlled: keep `selected` in the URL
 * (`?tab=`) so it is shareable. Render the panel with `TabPanel`.
 */
export function Tabs({
  label,
  tabs,
  selected,
  onSelect,
  idBase = "tabs",
}: {
  label: string;
  tabs: Tab[];
  selected: string;
  onSelect: (id: string) => void;
  idBase?: string;
}) {
  const list = useRef<HTMLDivElement>(null);

  function onKeyDown(event: KeyboardEvent) {
    const index = tabs.findIndex((t) => t.id === selected);
    const last = tabs.length - 1;
    const next =
      event.key === "ArrowRight" ? (index + 1) % tabs.length
      : event.key === "ArrowLeft" ? (index + last) % tabs.length
      : event.key === "Home" ? 0
      : event.key === "End" ? last
      : null;
    if (next === null) return;
    event.preventDefault();
    onSelect(tabs[next]!.id);
    list.current?.querySelectorAll<HTMLElement>('[role="tab"]')[next]?.focus();
  }

  return (
    <div ref={list} className={styles.tabs} role="tablist" aria-label={label} onKeyDown={onKeyDown}>
      {tabs.map((tab) => {
        const isSelected = tab.id === selected;
        return (
          <button
            key={tab.id}
            id={tabId(idBase, tab.id)}
            type="button"
            role="tab"
            className={styles.tab}
            aria-selected={isSelected}
            aria-controls={tabPanelId(idBase, tab.id)}
            tabIndex={isSelected ? 0 : -1}
            onClick={() => onSelect(tab.id)}
          >
            {tab.label}
            {/* The space keeps "Sensors · 11" one phrase in the accessible name. */}
            {tab.extra !== undefined && <> <span className={styles.extra}>{tab.extra}</span></>}
          </button>
        );
      })}
    </div>
  );
}

/** The panel of the selected tab `id`. */
export function TabPanel({ idBase = "tabs", id, children }: { idBase?: string; id: string; children: ReactNode }) {
  return (
    <div role="tabpanel" id={tabPanelId(idBase, id)} aria-labelledby={tabId(idBase, id)}>
      {children}
    </div>
  );
}
