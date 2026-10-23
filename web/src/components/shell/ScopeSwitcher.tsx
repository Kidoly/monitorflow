"use client";

import { type KeyboardEvent, useEffect, useId, useRef, useState } from "react";

import { buttonClass } from "@/components/ui/Button";
import { cx } from "@/components/ui/cx";
import type { Customer, Site } from "@/lib/api";
import { type Scope, useScope } from "@/lib/scope";
import { replaceParams } from "@/lib/urlState";

import { Icon } from "./icons";
import styles from "./Shell.module.css";

interface Option {
  key: string;
  label: string;
  scope: Scope;
  site?: boolean;
}

/**
 * The customer the pages show, picked from a two-level list (customer, then
 * its sites). The button shows only the customer name; the choice goes into
 * the URL (`?customer=`, and `?site=` for a site), so links keep it.
 */
export function ScopeSwitcher({ customers, sites }: { customers: Customer[]; sites: Site[] }) {
  const scope = useScope();
  const listId = useId();
  const [open, setOpen] = useState(false);
  const [active, setActive] = useState(0);
  const button = useRef<HTMLButtonElement>(null);
  const list = useRef<HTMLUListElement>(null);
  const root = useRef<HTMLDivElement>(null);

  const current = customers.find((c) => c.id === scope.customerId);
  const name = current?.name ?? "All customers";

  const options: Option[] = [{ key: "all", label: "All customers", scope: { customerId: null, siteId: null } }];
  for (const customer of customers) {
    options.push({ key: customer.id, label: customer.name, scope: { customerId: customer.id, siteId: null } });
    for (const site of sites.filter((s) => s.customer_id === customer.id)) {
      options.push({ key: site.id, label: site.name, scope: { customerId: customer.id, siteId: site.id }, site: true });
    }
  }
  const selectedIndex = Math.max(
    0,
    options.findIndex((o) => o.scope.customerId === scope.customerId && o.scope.siteId === scope.siteId),
  );

  useEffect(() => {
    if (!open) return;
    list.current?.querySelectorAll<HTMLElement>('[role="option"]')[active]?.focus();
  }, [open, active]);

  useEffect(() => {
    if (!open) return;
    const onPointerDown = (event: PointerEvent) => {
      if (!root.current?.contains(event.target as Node)) setOpen(false);
    };
    document.addEventListener("pointerdown", onPointerDown);
    return () => document.removeEventListener("pointerdown", onPointerDown);
  }, [open]);

  function show() {
    setActive(selectedIndex);
    setOpen(true);
  }

  function close() {
    setOpen(false);
    button.current?.focus();
  }

  function choose(option: Option) {
    replaceParams({ customer: option.scope.customerId, site: option.scope.siteId });
    close();
  }

  function onKeyDown(event: KeyboardEvent) {
    const last = options.length - 1;
    const keys: Record<string, () => void> = {
      ArrowDown: () => setActive((i) => Math.min(last, i + 1)),
      ArrowUp: () => setActive((i) => Math.max(0, i - 1)),
      Home: () => setActive(0),
      End: () => setActive(last),
      Enter: () => choose(options[active]!),
      " ": () => choose(options[active]!),
      Escape: close,
    };
    if (event.key === "Tab") {
      setOpen(false);
      return;
    }
    const action = keys[event.key];
    if (action) {
      event.preventDefault();
      action();
    }
  }

  const renderOption = (option: Option) => {
    const index = options.indexOf(option);
    return (
      <li
        key={option.key}
        role="option"
        tabIndex={-1}
        aria-selected={index === selectedIndex}
        className={cx(styles.option, option.site && styles.siteOption)}
        onClick={() => choose(option)}
      >
        {option.label}
      </li>
    );
  };

  return (
    <div className={styles.scope} ref={root}>
      <button
        ref={button}
        type="button"
        className={buttonClass({}, styles.scopeButton)}
        aria-haspopup="listbox"
        aria-expanded={open}
        aria-controls={open ? listId : undefined}
        aria-label={`Scope: ${name}`}
        onClick={() => (open ? close() : show())}
        onKeyDown={(e) => {
          if (!open && (e.key === "ArrowDown" || e.key === "ArrowUp")) {
            e.preventDefault();
            show();
          }
        }}
      >
        <Icon name="scope" className={styles.scopeIcon} />
        <span className={styles.scopeName}>{name}</span>
        <Icon name="chevron" className={styles.scopeChevron} />
      </button>
      {open && (
        <ul ref={list} id={listId} role="listbox" aria-label="Scope" className={styles.popup} onKeyDown={onKeyDown}>
          {renderOption(options[0]!)}
          {customers.map((customer) => {
            const own = options.filter((o) => o.scope.customerId === customer.id);
            const [head, ...siteOptions] = own;
            return (
              <li key={customer.id} role="presentation">
                <ul role="group" aria-label={customer.name}>
                  {renderOption(head!)}
                  {siteOptions.map(renderOption)}
                </ul>
              </li>
            );
          })}
        </ul>
      )}
    </div>
  );
}
