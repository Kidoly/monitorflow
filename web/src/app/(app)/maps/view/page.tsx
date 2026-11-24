import type { Metadata } from "next";
import { Suspense } from "react";

import { NetworkMapView } from "@/components/NetworkMapPage";
import { LoadingBlock } from "@/components/ui/PageState";

export const metadata: Metadata = { title: "Network map · MonitorFlow" };

// The map ID is a query parameter, as for servers (ADR-0020).
export default function NetworkMapRoute() {
  return (
    <Suspense fallback={<LoadingBlock label="Loading the network map" />}>
      <NetworkMapView />
    </Suspense>
  );
}
