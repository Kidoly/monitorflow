import type { Metadata } from "next";

import { NewCampaignPage } from "@/components/CampaignForm";

export const metadata: Metadata = { title: "New campaign · MonitorFlow" };

export default function NewCampaign() {
  return <NewCampaignPage />;
}
