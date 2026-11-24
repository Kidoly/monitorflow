import type { Metadata } from "next";
import { Suspense } from "react";

import { ScriptView } from "@/components/ScriptPage";

export const metadata: Metadata = { title: "Script · MonitorFlow" };

// The script ID is a query parameter, as for servers (ADR-0020).
export default function ScriptViewPage() {
  return (
    <Suspense fallback={<p role="status">Loading script…</p>}>
      <ScriptView />
    </Suspense>
  );
}
