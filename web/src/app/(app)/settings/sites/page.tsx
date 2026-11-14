import type { Metadata } from "next";

import { SitesAdminPage } from "@/components/SitesAdmin";

export const metadata: Metadata = { title: "Sites · MonitorFlow" };

export default function SitesPage() {
  return <SitesAdminPage />;
}
