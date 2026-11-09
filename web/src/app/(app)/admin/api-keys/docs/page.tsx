import type { Metadata } from "next";

import { ApiDocsPage } from "@/components/ApiDocs";

export const metadata: Metadata = { title: "API documentation · MonitorFlow" };

export default function ApiDocsRoute() {
  return <ApiDocsPage />;
}
