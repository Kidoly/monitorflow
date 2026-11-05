import type { Metadata } from "next";
import { Suspense } from "react";

import { DisplayScreen } from "@/components/DisplayScreen";

export const metadata: Metadata = { title: "Dashboard · MonitorFlow" };

/** Dashboards on a screen (ADR-0099), outside the signed-in layout: no navigation. */
export default function DisplayPage() {
  return (
    <Suspense fallback={null}>
      <DisplayScreen />
    </Suspense>
  );
}
