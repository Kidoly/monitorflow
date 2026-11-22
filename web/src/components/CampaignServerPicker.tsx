"use client";

import { useMemo, useState } from "react";

import { t } from "@/i18n";
import type { Server } from "@/lib/api";
import { searchServers } from "@/lib/campaigns";
import { formatOs } from "@/lib/format";

import own from "./Campaigns.module.css";
import { Button, fieldClass } from "./ui/Button";

/** Results per page: enough to scan, few enough to render for thousands of servers. */
export const PICKER_PAGE = 50;
/** Chosen servers shown by name; the rest are counted. */
const SHOWN_CHOSEN = 100;

/**
 * The servers a campaign names besides its site's: a search over name, OS
 * and site, one page of results at a time, a button choosing every result,
 * and the chosen servers, each removable.
 */
export function CampaignServerPicker({
  servers,
  siteId,
  selected,
  onChange,
  unavailable,
  showSite,
}: {
  /** The customer's unrevoked servers, in display order. */
  servers: Server[];
  /** Servers of this site are in the campaign already. */
  siteId: string;
  selected: string[];
  onChange: (ids: string[]) => void;
  /** Why a server cannot be chosen, or null when it can. */
  unavailable: (server: Server) => string | null;
  showSite: boolean;
}) {
  const [query, setQuery] = useState("");
  const [page, setPage] = useState(0);
  const matches = useMemo(() => searchServers(servers, query), [servers, query]);
  const byId = useMemo(() => new Map(servers.map((s) => [s.id, s])), [servers]);
  const chosen = new Set(selected);
  const inSite = (s: Server) => siteId !== "" && s.site_id === siteId;
  const addable = matches.filter((s) => unavailable(s) === null && !inSite(s) && !chosen.has(s.id));
  const pages = Math.max(1, Math.ceil(matches.length / PICKER_PAGE));
  const current = Math.min(page, pages - 1);
  const first = current * PICKER_PAGE;
  const shown = matches.slice(first, first + PICKER_PAGE);

  return (
    <div className={own.picker}>
      <div className={own.row}>
        <label className={own.fld}>
          {t("campaigns.picker.search")}
          <input
            className={fieldClass}
            type="search"
            value={query}
            placeholder={t("campaigns.picker.placeholder")}
            onChange={(e) => {
              setQuery(e.target.value);
              setPage(0);
            }}
          />
        </label>
        <Button disabled={addable.length === 0} onClick={() => onChange([...selected, ...addable.map((s) => s.id)])}>
          {t("campaigns.picker.selectAll", { count: addable.length })}
        </Button>
      </div>
      {matches.length === 0 ? (
        <p className={own.note}>{t("campaigns.picker.none")}</p>
      ) : (
        <ul className={own.servers} aria-label={t("campaigns.form.servers")}>
          {shown.map((s) => {
            const why = unavailable(s);
            const detail = [showSite && s.site_name, formatOs(s.os_family, s.os_version), why].filter(Boolean);
            return (
              <li key={s.id}>
                <label>
                  <input
                    type="checkbox"
                    checked={why === null && (chosen.has(s.id) || inSite(s))}
                    disabled={why !== null || inSite(s)}
                    onChange={(e) =>
                      onChange(e.target.checked ? [...selected, s.id] : selected.filter((id) => id !== s.id))
                    }
                  />
                  {s.hostname} <span className={own.dim}>{detail.join(" · ")}</span>
                </label>
              </li>
            );
          })}
        </ul>
      )}
      {pages > 1 && (
        <div className={own.pager}>
          <Button size="small" disabled={current === 0} onClick={() => setPage(current - 1)}>
            {t("campaigns.picker.previous")}
          </Button>
          <span className={own.note}>
            {t("campaigns.picker.range", { from: first + 1, to: first + shown.length, total: matches.length })}
          </span>
          <Button size="small" disabled={current === pages - 1} onClick={() => setPage(current + 1)}>
            {t("campaigns.picker.next")}
          </Button>
        </div>
      )}
      {selected.length > 0 && (
        <div className={own.chosen} role="group" aria-label={t("campaigns.picker.chosen", { count: selected.length })}>
          <div className={own.row}>
            <strong>{t("campaigns.picker.chosen", { count: selected.length })}</strong>
            <Button size="small" onClick={() => onChange([])}>
              {t("campaigns.picker.clear")}
            </Button>
          </div>
          <ul className={own.chips}>
            {selected.slice(0, SHOWN_CHOSEN).map((id) => {
              const name = byId.get(id)?.hostname ?? id;
              return (
                <li key={id}>
                  {name}
                  <button
                    type="button"
                    aria-label={t("campaigns.picker.remove", { name })}
                    onClick={() => onChange(selected.filter((other) => other !== id))}
                  >
                    ×
                  </button>
                </li>
              );
            })}
          </ul>
          {selected.length > SHOWN_CHOSEN && (
            <p className={own.note}>{t("campaigns.picker.more", { count: selected.length - SHOWN_CHOSEN })}</p>
          )}
        </div>
      )}
    </div>
  );
}
