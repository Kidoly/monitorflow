"use client";

import { type ReactNode, createContext, useCallback, useContext, useMemo } from "react";

import { type Me, api, isAbortError } from "@/lib/api";
import { multiCustomer } from "@/lib/me";
import { usePolledData } from "@/lib/usePolledData";

import { type SettingsId, visibleSettings } from "./catalogue";
import {
  type SettingsStatus,
  type SettingsStatuses,
  addServersStatus,
  aiStatus,
  apiKeysStatus,
  authorityLine,
  backupsStatus,
  integrationsStatus,
  licenseStatus,
  notificationsStatus,
  pluginsStatus,
  relaysStatus,
  serverUpdateStatus,
  sitesStatus,
  usersStatus,
} from "./status";

/** These change by the day, not the second; opening Settings refreshes them. */
const REFRESH_MS = 5 * 60 * 1000;

type Loader = (signal: AbortSignal, now: Date, me: Me) => Promise<SettingsStatus | undefined>;

/**
 * Where each status line comes from: lists and views the settings pages
 * read already. General has none: its values are settings, not states.
 */
const LOADERS: Partial<Record<SettingsId, Loader>> = {
  sites: (signal, _, me) => api.sites(signal).then((l) => sitesStatus(l.sites, multiCustomer(me))),
  relays: (signal) => api.relays(signal).then((l) => relaysStatus(l.relays)),
  "add-servers": (signal) => api.installKeys(signal).then(addServersStatus),
  "certificate-authority": (signal) => api.certificateAuthority(signal).then(authorityLine),
  notifications: (signal) => api.notificationChannels(signal).then((l) => notificationsStatus(l.channels)),
  integrations: (signal) => api.integrations(signal).then(integrationsStatus),
  users: (signal) => api.users(signal).then((l) => usersStatus(l.users)),
  "api-keys": (signal, now) => api.apiKeys(signal).then((l) => apiKeysStatus(l.keys, now)),
  ai: (signal) => api.aiSettings(signal).then(aiStatus),
  plugins: (signal) => api.plugins(signal).then((l) => pluginsStatus(l.plugins)),
  "server-update": (signal) => api.serverUpdate(signal).then(serverUpdateStatus),
  backups: (signal, now) => api.backups(signal).then((v) => backupsStatus(v, now)),
  license: (signal) => api.license(signal).then(licenseStatus),
};

/** The status of every entry `ids` names; one that cannot be read has none, and the others still show. */
async function loadStatuses(me: Me, ids: SettingsId[], signal: AbortSignal): Promise<SettingsStatuses> {
  const now = new Date();
  const results = await Promise.all(
    ids.map(async (id) => {
      try {
        return [id, await LOADERS[id]?.(signal, now, me)] as const;
      } catch (e) {
        if (isAbortError(e)) throw e;
        return [id, undefined] as const;
      }
    }),
  );
  return Object.fromEntries(results.filter(([, status]) => status !== undefined));
}

interface Value {
  statuses: SettingsStatuses;
  refresh: () => void;
}

const Context = createContext<Value>({ statuses: {}, refresh: () => {} });

/** Reads the status lines of the settings `me` sees, every 5 minutes, for the pages and the sidebar under it. */
export function SettingsStatusProvider({ me, children }: { me: Me; children: ReactNode }) {
  const ids = useMemo(() => visibleSettings(me).flatMap((g) => g.entries.map((e) => e.id)), [me]);
  const key = ids.join(",");
  const fetcher = useCallback(
    (signal: AbortSignal) => loadStatuses(me, key ? (key.split(",") as SettingsId[]) : [], signal),
    // `me` changes object on every reload of it; what the lines depend on is in `key` and the flag.
    // eslint-disable-next-line react-hooks/exhaustive-deps
    [key, me.multi_customer],
  );
  const { data, refresh } = usePolledData(fetcher, { intervalMs: REFRESH_MS, enabled: ids.length > 0 });
  const value = useMemo(() => ({ statuses: data ?? {}, refresh }), [data, refresh]);
  return <Context.Provider value={value}>{children}</Context.Provider>;
}

/** The status lines, empty until they load or outside the provider. */
export function useSettingsStatuses(): Value {
  return useContext(Context);
}
