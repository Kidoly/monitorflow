import type { Metadata } from "next";
import { Suspense } from "react";

import { ScriptRunView } from "@/components/ScriptRunPage";

export const metadata: Metadata = { title: "Script run · MonitorFlow" };

// The run ID is a query parameter, as for servers (ADR-0020).
export default function ScriptRunViewPage() {
  return (
    <Suspense fallback={<p role="status">Loading run…</p>}>
      <ScriptRunView />
    </Suspense>
  );
}
