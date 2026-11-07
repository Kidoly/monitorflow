import type { Metadata } from "next";

import { ApiKeysPage } from "@/components/ApiKeys";

export const metadata: Metadata = { title: "API keys · MonitorFlow" };

export default function ApiKeysRoute() {
  return <ApiKeysPage />;
}
