import type { Metadata } from "next";
import { Suspense } from "react";

import { CampaignView } from "@/components/CampaignPage";

export const metadata: Metadata = { title: "Campaign · MonitorFlow" };

// The campaign ID is a query parameter, as for servers (ADR-0020).
export default function CampaignViewPage() {
  return (
    <Suspense fallback={<p role="status">Loading campaign…</p>}>
      <CampaignView />
    </Suspense>
  );
}
