"use client";

import { useEffect, useState } from "react";

import { t } from "@/i18n";
import { type Server, api } from "@/lib/api";
import { updateAvailable } from "@/lib/agentVersion";
import type { BulkRunner } from "@/lib/bulk";
import { administersCustomer, modifiesSite, useMe } from "@/lib/me";
import { InlineConfirm } from "@/components/ui/InlineConfirm";
import { RowMenu, type RowMenuItem } from "@/components/ui/RowMenu";

import styles from "./BulkSilence.module.css";
import { WindowForm } from "./MaintenanceWindows";

type Open = "update" | "window" | "remove" | null;

/**
 * "More ▾" of the fleet's bulk bar: what a server's page does one server
 * at a time, for the checked ones. Updating the agent and removing are for
 * administrators of the server's customer and ask first, with the count;
 * offline alerts and a maintenance window for operators of its site. A
 * window covers servers of one site, as on the Silences page.
 */
export function FleetAdminActions({
  servers,
  bulk,
  onChanged,
}: {
  servers: Server[];
  bulk: BulkRunner<Server>;
  onChanged: () => void;
}) {
  const me = useMe();
  const [open, setOpen] = useState<Open>(null);
  const [latest, setLatest] = useState<string | null>(null);
  const administers = administersCustomer(me);

  // The release to update to, as a server's page shows it; only administrators update.
  useEffect(() => {
    if (!administers) return;
    const controller = new AbortController();
    api.agentReleases(controller.signal).then((r) => setLatest(r.latest), () => {});
    return () => controller.abort();
  }, [administers]);

  const count = servers.length;
  const owned = (s: Server) => administersCustomer(me, s.customer_id);
  const writable = (s: Server) => modifiesSite(me, s.customer_id, s.site_id);

  function offlineAlerts(on: boolean) {
    void bulk.run(servers, {
      done: (n) => t("fleet.bulk.offlineChanged", { count: n }),
      skipped: (n) => t("fleet.bulk.offlineSame", { count: n }),
      allowed: writable,
      act: async (s) => {
        if ((s.offline_alerts !== false) === on) return "skipped";
        await api.setOfflineAlerts("servers", s.id, on);
        return "done";
      },
    });
  }

  function chooseWindow() {
    if (new Set(servers.map((s) => s.site_id)).size > 1) {
      bulk.say(t("fleet.bulk.windowOneSite"), "warn");
      return;
    }
    if (!servers.every(writable)) {
      bulk.say(t("ui.bulk.refused", { count }), "warn");
      return;
    }
    setOpen("window");
  }

  const items: RowMenuItem[] = [
    ...(administers && latest ? [{ label: t("fleet.bulk.update", { version: latest }), onSelect: () => setOpen("update") }] : []),
    { label: t("fleet.bulk.offlineOff"), onSelect: () => offlineAlerts(false) },
    { label: t("fleet.bulk.offlineOn"), onSelect: () => offlineAlerts(true) },
    { label: t("maintenance.menu.window"), onSelect: chooseWindow },
    ...(administers ? [{ label: t("fleet.bulk.remove"), onSelect: () => setOpen("remove"), danger: true }] : []),
  ];
  // A viewer changes nothing here.
  if (!me?.roles.some((grant) => grant.role !== "viewer")) return null;

  return (
    <>
      <RowMenu label={t("fleet.bulk.moreName", { count })} trigger={t("fleet.bulk.more")} disabled={bulk.busy} items={items} />
      {open === "update" && latest && (
        <div className={styles.form}>
          <InlineConfirm
            onClose={() => setOpen(null)}
            trigger=""
            title={t("fleet.bulk.updateTitle", { count, version: latest })}
            consequence={t("fleet.bulk.updateConsequence", { version: latest })}
            confirmLabel={t("fleet.bulk.updateConfirm", { count })}
            confirmVariant="k"
            onConfirm={() =>
              bulk.run(servers, {
                done: (n) => t("fleet.bulk.updated", { count: n }),
                skipped: (n) => t("fleet.bulk.upToDate", { count: n }),
                allowed: owned,
                act: async (s) => {
                  if (s.kind === "device" || !updateAvailable(s.agent_version, latest)) return "skipped";
                  await api.updateAgent(s.id, latest);
                  return "done";
                },
              })
            }
          />
        </div>
      )}
      {open === "remove" && (
        <div className={styles.form}>
          <InlineConfirm
            onClose={() => setOpen(null)}
            trigger=""
            title={t("fleet.bulk.removeTitle", { count })}
            consequence={t("fleet.bulk.removeConsequence")}
            confirmLabel={t("fleet.bulk.removeConfirm", { count })}
            onConfirm={() =>
              bulk.run(servers, {
                done: (n) => t("fleet.bulk.removed", { count: n }),
                allowed: owned,
                act: async (s) => {
                  await api.revoke("servers", s.id, "cessation_of_operation");
                  return "done";
                },
              })
            }
          />
        </div>
      )}
      {open === "window" && servers[0] && (
        <div className={styles.form}>
          <WindowForm
            window={null}
            place={{ siteId: servers[0].site_id, serverIds: servers.map((s) => s.id) }}
            onSaved={() => {
              setOpen(null);
              bulk.say(t("fleet.bulk.windowSaved", { count }));
              onChanged();
            }}
            onCancel={() => setOpen(null)}
          />
        </div>
      )}
    </>
  );
}
