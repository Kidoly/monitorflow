import type { Metadata } from "next";
import { Suspense } from "react";

import { SiteView } from "@/components/SitePage";

export const metadata: Metadata = { title: "Site · MonitorFlow" };

// The site ID is a query parameter, as for servers (ADR-0020).
export default function SiteViewPage() {
  return (
    <Suspense fallback={<p role="status">Loading site…</p>}>
      <SiteView />
    </Suspense>
  );
}
