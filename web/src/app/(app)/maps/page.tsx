import type { Metadata } from "next";
import { Suspense } from "react";

import { NetworkMapsPage } from "@/components/NetworkMaps";
import { LoadingBlock } from "@/components/ui/PageState";

export const metadata: Metadata = { title: "Network maps · MonitorFlow" };

export default function NetworkMapsRoute() {
  return (
    <Suspense fallback={<LoadingBlock label="Loading network maps" />}>
      <NetworkMapsPage />
    </Suspense>
  );
}
