"use client";

import { useSearchParams } from "next/navigation";
import { useEffect, useMemo } from "react";

import { t } from "@/i18n";
import { api, describeError, isUnauthorized } from "@/lib/api";
import { modifiesSite, multiCustomer, useMe } from "@/lib/me";
import { scopedHref } from "@/lib/scope";
import { usePolledData } from "@/lib/usePolledData";
import { UUID } from "@/lib/uuid";
import { Breadcrumb } from "@/components/ui/Breadcrumb";
import { ButtonLink } from "@/components/ui/Button";
import { Page } from "@/components/ui/Page";
import { ErrorState, LoadingBlock, NotFound, PageHeader } from "@/components/ui/PageState";

import { MonitoringSettings } from "./MonitoringSettings";
import { SiteSources } from "./PackageSources";
import { SiteAiCommands } from "./SiteAiCommands";
import { SilenceBar } from "./Silences";

/** The site page for the `id` query parameter. */
export function SiteView() {
  const id = useSearchParams().get("id");
  return <SitePage key={id} id={id} />;
}

const loadSites = (signal: AbortSignal) => api.sites(signal);

/** A site's alerts and checks: the defaults of every server of the site (ADR-0057). */
export function SitePage({ id }: { id: string | null }) {
  const valid = id !== null && UUID.test(id);
  const me = useMe();
  const { data, error, refresh } = usePolledData(loadSites, { enabled: valid });
  const site = data?.sites.find((s) => s.id === id?.toLowerCase());
  // Stable across refreshes, so the settings do not reload with the site list.
  const siteId = site?.id;
  const siteName = site?.name;
  const target = useMemo(
    () => (siteId && siteName ? { kind: "site" as const, id: siteId, siteName } : undefined),
    [siteId, siteName],
  );

  useEffect(() => {
    if (site) document.title = `${site.name} · MonitorFlow`;
  }, [site]);

  if (!valid || (data && !site)) {
    return <NotFound title={t("fleet.site.notFound")} actions={<ButtonLink href="/">{t("fleet.site.back")}</ButtonLink>} />;
  }
  if (!site || !target) {
    return error !== null && !isUnauthorized(error) ? (
      <ErrorState what={t("fleet.site.what")} error={describeError(error)} onRetry={refresh} />
    ) : (
      <LoadingBlock label={t("fleet.site.loading")} />
    );
  }
  const fleet = scopedHref("/", { customerId: site.customer_id, siteId: site.id });
  return (
    <Page>
      <Breadcrumb parents={[{ label: t("fleet.title"), href: "/" }]} current={site.name} />
      <PageHeader
        detail
        title={site.name}
        // The button names the server count; the customer is the only other fact.
        meta={multiCustomer(me) ? site.customer_name : undefined}
        actions={<ButtonLink href={fleet}>{t("fleet.site.show", { count: site.server_count })}</ButtonLink>}
      />
      <SilenceBar target={{ scope: "site", id: site.id, name: site.name, customerId: site.customer_id, siteId: site.id }} />
      <MonitoringSettings target={target} canEdit={modifiesSite(me, site.customer_id, site.id)} />
      <SiteSources site={site} />
      <SiteAiCommands site={site} />
    </Page>
  );
}
