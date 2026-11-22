import type { Metadata } from "next";

import { IntegrationsCatalogue } from "@/components/IntegrationsCatalogue";

export const metadata: Metadata = { title: "Integrations · MonitorFlow" };

export default function Integrations() {
  return <IntegrationsCatalogue />;
}
