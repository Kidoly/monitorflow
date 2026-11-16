import { t } from "@/i18n";
import type { ServerDetail } from "@/lib/api";
import { readsCustomers, useMe } from "@/lib/me";
import { CAMPAIGNS_PATH, serverHref } from "@/lib/routes";
import { scopedHref, useScope } from "@/lib/scope";
import { ButtonLink } from "@/components/ui/Button";
import { EmptyState } from "@/components/ui/PageState";
import { Page } from "@/components/ui/Page";

import { PackageSources } from "./PackageSources";
import { PendingUpdates } from "./PendingUpdates";
import { ServerRollback } from "./ServerRollback";

/**
 * The Updates tab. The server page had no updates section before the
 * redesign; the patch workflow of the handoff (03c) replaces this file.
 */
export function ServerUpdatesTab({ server }: { server: ServerDetail }) {
  const me = useMe();
  const scope = useScope();
  const sensors = <ButtonLink href={scopedHref(serverHref(server.id), scope)}>{t("server.updatesTab.back")}</ButtonLink>;
  if (server.package_updates === false) {
    return (
      <EmptyState title={t("server.updatesTab.unavailable")} actions={sensors}>
        {t("server.updatesTab.unavailableText", { server: server.hostname })}
      </EmptyState>
    );
  }
  return (
    <Page>
      <PendingUpdates server={server} />
      <PackageSources server={server} />
      <ServerRollback server={server} />
      <EmptyState
        actions={
          readsCustomers(me) ? <ButtonLink href={scopedHref(CAMPAIGNS_PATH, scope)}>{t("server.updatesTab.openCampaigns")}</ButtonLink> : sensors
        }
      >
        {t("server.updatesTab.throughCampaigns", { server: server.hostname })}
      </EmptyState>
    </Page>
  );
}
