import type { Metadata } from "next";

import { CampaignListPage } from "@/components/CampaignList";

export const metadata: Metadata = { title: "Campaigns · MonitorFlow" };

export default function CampaignsPage() {
  return <CampaignListPage />;
}
