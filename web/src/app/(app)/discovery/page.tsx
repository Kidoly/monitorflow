import type { Metadata } from "next";
import { Suspense } from "react";

import { NetworkScansPage } from "@/components/NetworkScans";
import { LoadingBlock } from "@/components/ui/PageState";

export const metadata: Metadata = { title: "Network scans · MonitorFlow" };

export default function NetworkScansRoute() {
  return (
    <Suspense fallback={<LoadingBlock label="Loading network scans" />}>
      <NetworkScansPage />
    </Suspense>
  );
}
