import type { Metadata } from "next";

import { RelayListPage } from "@/components/RelayList";

export const metadata: Metadata = { title: "Relays · MonitorFlow" };

export default function RelaysPage() {
  return <RelayListPage />;
}
