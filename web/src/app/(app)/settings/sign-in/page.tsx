import type { Metadata } from "next";

import { SignInSettingsPage } from "@/components/SignInSettings";

export const metadata: Metadata = { title: "Sign-in · MonitorFlow" };

export default function SignIn() {
  return <SignInSettingsPage />;
}
