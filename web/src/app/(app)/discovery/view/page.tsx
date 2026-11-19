import type { Metadata } from "next";
import { Suspense } from "react";

import { NetworkScanView } from "@/components/NetworkScan";
import { LoadingBlock } from "@/components/ui/PageState";

export const metadata: Metadata = { title: "Network scan · MonitorFlow" };

// The scan ID is a query parameter, as for servers (ADR-0020).
export default function NetworkScanRoute() {
  return (
    <Suspense fallback={<LoadingBlock label="Loading the network scan" />}>
      <NetworkScanView />
    </Suspense>
  );
}
