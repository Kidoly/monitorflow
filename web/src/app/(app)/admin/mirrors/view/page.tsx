import type { Metadata } from "next";
import { Suspense } from "react";

import { MirrorView } from "@/components/MirrorPage";

export const metadata: Metadata = { title: "Mirror · MonitorFlow" };

// The mirror ID is a query parameter, as for servers (ADR-0020).
export default function MirrorViewPage() {
  return (
    <Suspense fallback={<p role="status">Loading mirror…</p>}>
      <MirrorView />
    </Suspense>
  );
}
