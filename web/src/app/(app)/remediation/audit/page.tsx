import type { Metadata } from "next";
import { Suspense } from "react";

import { RemediationAuditView } from "@/components/RemediationAudit";

export const metadata: Metadata = { title: "Audit log · MonitorFlow" };

// Filters live in the query string, read on the client (ADR-0020).
export default function AuditPage() {
  return (
    <Suspense fallback={<p role="status">Loading the audit log…</p>}>
      <RemediationAuditView />
    </Suspense>
  );
}
