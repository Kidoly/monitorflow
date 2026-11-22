import type { Metadata } from "next";

import { IntegrationsPage } from "@/components/IntegrationsAdmin";

export const metadata: Metadata = { title: "Custom webhooks · MonitorFlow" };

export default function Webhooks() {
  return <IntegrationsPage />;
}
