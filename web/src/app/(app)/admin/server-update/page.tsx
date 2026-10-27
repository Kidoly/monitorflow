import type { Metadata } from "next";

import { ServerUpdatePage } from "@/components/ServerUpdatePage";

export const metadata: Metadata = { title: "Server update · MonitorFlow" };

export default function ServerUpdateRoute() {
  return <ServerUpdatePage />;
}
