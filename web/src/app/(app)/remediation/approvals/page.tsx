import type { Metadata } from "next";
import { Suspense } from "react";

import { ApprovalQueue } from "@/components/ApprovalQueue";

export const metadata: Metadata = { title: "Approvals · MonitorFlow" };

// The tier filter and the scope live in the query string, read on the client.
export default function ApprovalsPage() {
  return (
    <Suspense fallback={<p role="status">Loading approvals…</p>}>
      <ApprovalQueue />
    </Suspense>
  );
}
