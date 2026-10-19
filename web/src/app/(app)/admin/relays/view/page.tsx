import type { Metadata } from "next";
import { Suspense } from "react";

import { RelayView } from "@/components/RelayPage";

export const metadata: Metadata = { title: "Relay · MonitorFlow" };

// The relay ID is a query parameter, as for servers (ADR-0020).
export default function RelayViewPage() {
  return (
    <Suspense fallback={<p role="status">Loading relay…</p>}>
      <RelayView />
    </Suspense>
  );
}
