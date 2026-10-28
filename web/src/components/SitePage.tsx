"use client";

import { useSearchParams } from "next/navigation";
import { useEffect, useMemo } from "react";

import { api, describeError, isUnauthorized } from "@/lib/api";
import { modifiesSite, useMe } from "@/lib/me";
import { scopedHref } from "@/lib/scope";
import { usePolledData } from "@/lib/usePolledData";
import { UUID } from "@/lib/uuid";
import { Breadcrumb } from "@/components/ui/Breadcrumb";
import { ButtonLink } from "@/components/ui/Button";
import { Page } from "@/components/ui/Page";
import { ErrorState, LoadingBlock, NotFound, PageHeader } from "@/components/ui/PageState";

import { MonitoringSettings } from "./MonitoringSettings";

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
    return <NotFound title="Site not found" actions={<ButtonLink href="/">Back to the fleet</ButtonLink>} />;
  }
  if (!site || !target) {
    return error !== null && !isUnauthorized(error) ? (
      <ErrorState what="the site" error={describeError(error)} onRetry={refresh} />
    ) : (
      <LoadingBlock label="Loading site" />
    );
  }
  const fleet = scopedHref("/", { customerId: site.customer_id, siteId: site.id });
  const servers = `${site.server_count} ${site.server_count === 1 ? "server" : "servers"}`;
  return (
    <Page>
      <Breadcrumb parents={[{ label: "Fleet", href: "/" }]} current={site.name} />
      <PageHeader
        detail
        title={site.name}
        meta={`${site.customer_name} · ${servers}`}
        actions={<ButtonLink href={fleet}>Show its {servers}</ButtonLink>}
      />
      <MonitoringSettings target={target} canEdit={modifiesSite(me, site.customer_id, site.id)} />
    </Page>
  );
}
