import type { Metadata } from "next";

import { TokensAdminPage } from "@/components/TokensAdmin";

export const metadata: Metadata = { title: "Enrollment tokens · MonitorFlow" };

export default function TokensPage() {
  return <TokensAdminPage />;
}
