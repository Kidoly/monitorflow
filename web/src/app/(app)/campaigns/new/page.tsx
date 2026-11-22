import type { Metadata } from "next";
import { Suspense } from "react";

import { NewCampaignPage } from "@/components/CampaignForm";

export const metadata: Metadata = { title: "New campaign · MonitorFlow" };

// `?edit=` and `?from=` name a campaign as a query parameter (ADR-0020).
export default function NewCampaign() {
  return (
    <Suspense fallback={<p role="status">Loading…</p>}>
      <NewCampaignPage />
    </Suspense>
  );
}
