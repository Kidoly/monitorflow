import type { Metadata } from "next";
import { Suspense } from "react";

import { ServerView } from "@/components/ServerPage";
import { LoadingBlock } from "@/components/ui/PageState";

export const metadata: Metadata = { title: "Server · MonitorFlow" };

// The server ID is a query parameter (`/servers/view/?id=…`), not a path
// segment: a static export cannot serve dynamic routes (ADR-0020).
export default function ServerViewPage() {
  return (
    <Suspense fallback={<LoadingBlock label="Loading server" />}>
      <ServerView />
    </Suspense>
  );
}
