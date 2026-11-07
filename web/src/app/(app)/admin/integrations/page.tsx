import type { Metadata } from "next";

import { IntegrationsPage } from "@/components/IntegrationsAdmin";

export const metadata: Metadata = { title: "Integrations · MonitorFlow" };

export default function Integrations() {
  return <IntegrationsPage />;
}
