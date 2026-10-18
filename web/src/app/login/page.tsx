import type { Metadata } from "next";

import { LoginForm } from "@/components/LoginForm";

export const metadata: Metadata = { title: "Sign in · MonitorFlow" };

export default function LoginPage() {
  return <LoginForm />;
}
