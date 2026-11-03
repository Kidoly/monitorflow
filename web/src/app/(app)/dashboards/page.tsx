import type { Metadata } from "next";
import { Suspense } from "react";

import { DashboardsPage } from "@/components/Dashboards";
import { LoadingBlock } from "@/components/ui/PageState";

export const metadata: Metadata = { title: "Dashboards · MonitorFlow" };

export default function Page() {
  return (
    <Suspense fallback={<LoadingBlock label="Loading dashboards" />}>
      <DashboardsPage />
    </Suspense>
  );
}
