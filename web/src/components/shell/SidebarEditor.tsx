"use client";

import { useEffect, useRef, useState } from "react";

import { Button } from "@/components/ui/Button";
import { SegmentedControl } from "@/components/ui/SegmentedControl";
import { Switch } from "@/components/ui/Switch";
import { t } from "@/i18n";
import type { Me } from "@/lib/api";
import { type SidebarPreferences, setPreference, usePreferences } from "@/lib/preferences";

import { Icon } from "./icons";
import { type NavSection, SETTINGS_ITEM, orderedNavigation } from "./navigation";
import styles from "./SidebarEditor.module.css";

type Labels = "auto" | "labels" | "icons";

const LABELS: { value: Labels; label: () => string }[] = [
  { value: "auto", label: () => t("shell.sidebar.labels.auto") },
  { value: "labels", label: () => t("shell.sidebar.labels.labels") },
  { value: "icons", label: () => t("shell.sidebar.labels.icons") },
];

/** Stores `sidebar`, or nothing when it says only defaults. */
function save(sidebar: SidebarPreferences) {
  const clean: SidebarPreferences = {
    ...(sidebar.collapsed !== undefined && { collapsed: sidebar.collapsed }),
    ...(sidebar.hidden?.length && { hidden: sidebar.hidden }),
    ...(sidebar.order?.length && { order: sidebar.order }),
  };
  setPreference("sidebar", Object.keys(clean).length > 0 ? clean : undefined);
}

/**
 * "Personalize the sidebar" (ADR-0137): labels or icons only, which items
 * show, and their order within their section, saved with the person's
 * preferences. Lists only what `me`'s roles show; Settings always shows.
 * Moving is by buttons, so it works from the keyboard, and a polite live
 * region says where the item went.
 */
export function SidebarEditor({ me }: { me: Me }) {
  const stored = usePreferences().preferences.sidebar;
  const sidebar = stored ?? {};
  const sections = orderedNavigation(me, sidebar);
  const hidden = sidebar.hidden ?? [];
  const [announcement, setAnnouncement] = useState("");
  /** The button to focus after the next render: `<id>:up` or `<id>:down`. */
  const focus = useRef<string | null>(null);
  const buttons = useRef(new Map<string, HTMLButtonElement>());

  // After a move, keep focus on the moved item's button, or on its other
  // one when it reached the end of its section.
  useEffect(() => {
    if (!focus.current) return;
    const [id, direction] = focus.current.split(":");
    const button = buttons.current.get(focus.current);
    const other = buttons.current.get(`${id}:${direction === "up" ? "down" : "up"}`);
    (button && !button.disabled ? button : other)?.focus();
    focus.current = null;
  });

  const labels: Labels = sidebar.collapsed === undefined ? "auto" : sidebar.collapsed ? "icons" : "labels";

  function setLabels(value: Labels) {
    save({ ...sidebar, collapsed: value === "auto" ? undefined : value === "icons" });
  }

  function setShown(id: string, shown: boolean) {
    save({ ...sidebar, hidden: shown ? hidden.filter((other) => other !== id) : [...hidden, id] });
  }

  function move(section: NavSection, index: number, delta: -1 | 1) {
    const items = [...section.items];
    const item = items[index]!;
    items.splice(index, 1);
    items.splice(index + delta, 0, item);
    const shown = sections.flatMap((other) => (other === section ? items : other.items)).map((i) => i.id);
    // Items another role of this person showed keep their place in the order.
    const kept = (sidebar.order ?? []).filter((id) => !shown.includes(id));
    save({ ...sidebar, order: [...shown, ...kept] });
    setAnnouncement(
      t("shell.sidebar.moved", { name: t(item.label), position: index + delta + 1, total: items.length }),
    );
    focus.current = `${item.id}:${delta < 0 ? "up" : "down"}`;
  }

  function reset() {
    setPreference("sidebar", undefined);
    setAnnouncement(t("shell.sidebar.wasReset"));
  }

  const ref = (key: string) => (element: HTMLButtonElement | null) => {
    if (element) buttons.current.set(key, element);
    else buttons.current.delete(key);
  };

  return (
    <div className={styles.editor}>
      <fieldset className={styles.labels}>
        <legend className={styles.legend}>{t("shell.sidebar.labels")}</legend>
        <SegmentedControl
          label={t("shell.sidebar.labels")}
          options={LABELS.map((option) => ({ value: option.value, label: option.label() }))}
          value={labels}
          onChange={setLabels}
        />
      </fieldset>

      <div className={styles.legend} id="sidebar-items">
        {t("shell.sidebar.items")}
      </div>
      <p className={styles.hint}>{t("shell.sidebar.itemsHint")}</p>
      {sections.map((section) => (
        <section key={section.heading} className={styles.section} aria-labelledby={`sidebar-${section.heading}`}>
          <h3 className={styles.heading} id={`sidebar-${section.heading}`}>
            {t(section.heading)}
          </h3>
          <ul className={styles.list}>
            {section.items.map((item, index) => {
              const name = t(item.label);
              return (
                <li key={item.id} className={styles.row}>
                  <Switch
                    rowClassName={styles.check}
                    checked={!hidden.includes(item.id)}
                    onChange={(shown) => setShown(item.id, shown)}
                    label={
                      <span className={styles.item}>
                        <Icon name={item.icon} />
                        {name}
                      </span>
                    }
                  />
                  <Button
                    size="small"
                    ref={ref(`${item.id}:up`)}
                    aria-label={t("shell.sidebar.moveUp", { name })}
                    title={t("shell.sidebar.moveUp", { name })}
                    disabled={index === 0}
                    onClick={() => move(section, index, -1)}
                  >
                    ↑
                  </Button>
                  <Button
                    size="small"
                    ref={ref(`${item.id}:down`)}
                    aria-label={t("shell.sidebar.moveDown", { name })}
                    title={t("shell.sidebar.moveDown", { name })}
                    disabled={index === section.items.length - 1}
                    onClick={() => move(section, index, 1)}
                  >
                    ↓
                  </Button>
                </li>
              );
            })}
          </ul>
        </section>
      ))}
      <ul className={styles.list}>
        <li className={styles.row}>
          <Switch
            rowClassName={styles.check}
            checked
            disabled
            onChange={() => {}}
            aria-describedby="sidebar-settings-always"
            label={
              <span className={styles.item}>
                <Icon name={SETTINGS_ITEM.icon} />
                {t(SETTINGS_ITEM.label)}
              </span>
            }
          />
          <span className={styles.always} id="sidebar-settings-always">
            {t("shell.sidebar.alwaysShown")}
          </span>
        </li>
      </ul>

      <div className={styles.actions}>
        <Button onClick={reset} disabled={!stored}>
          {t("shell.sidebar.reset")}
        </Button>
      </div>
      <p className="visually-hidden" role="status" aria-live="polite">
        {announcement}
      </p>
    </div>
  );
}
