import type { Metadata } from "next";

import { NotificationsPage } from "@/components/NotificationsAdmin";

export const metadata: Metadata = { title: "Notifications · MonitorFlow" };

export default function Notifications() {
  return <NotificationsPage />;
}
