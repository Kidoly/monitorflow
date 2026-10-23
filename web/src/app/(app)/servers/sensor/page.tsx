import type { Metadata } from "next";
import { Suspense } from "react";

import { SensorView } from "@/components/SensorPage";
import { LoadingBlock } from "@/components/ui/PageState";

export const metadata: Metadata = { title: "Sensor · MonitorFlow" };

// The server ID and the sensor's metric are query parameters, like the server page's (ADR-0020).
export default function SensorViewPage() {
  return (
    <Suspense fallback={<LoadingBlock label="Loading sensor" />}>
      <SensorView />
    </Suspense>
  );
}
