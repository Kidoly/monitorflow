import type { Metadata } from "next";

import { UsersAdminPage } from "@/components/UsersAdmin";

export const metadata: Metadata = { title: "Users and roles · MonitorFlow" };

export default function UsersPage() {
  return <UsersAdminPage />;
}
