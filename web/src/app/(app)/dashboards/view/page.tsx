import type { Metadata } from "next";
import { Suspense } from "react";

import { DashboardView } from "@/components/Dashboards";
import { LoadingBlock } from "@/components/ui/PageState";

export const metadata: Metadata = { title: "Dashboard · MonitorFlow" };

// The dashboard ID is a query parameter, as for servers (ADR-0020).
export default function Page() {
  return (
    <Suspense fallback={<LoadingBlock label="Loading dashboard" />}>
      <DashboardView />
    </Suspense>
  );
}
