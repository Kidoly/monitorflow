import type { Metadata } from "next";

import { OnCallPage } from "@/components/OnCall";

export const metadata: Metadata = { title: "On call · MonitorFlow" };

export default function Page() {
  return <OnCallPage />;
}
